use std::{env, fs, path::Path, sync::Arc};

use anyhow::{bail, Context};
use arc_web::{ArcApp, ArcAppBuilder};
use diesel::prelude::*;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use rand::RngCore;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::domain::audit_event::aggregate::AuditEventAggregate;

mod boards;
mod domain;
mod orchestration;
mod reports;
mod routes;
mod settings;
mod summary;
mod ui;
mod workflow;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "serve".to_string());
    if command == "setup" {
        prepare_env()?;
    }
    dotenv::dotenv().ok();
    init_logging();
    match command.as_str() {
        "setup" => {
            migrate()?;
            println!("Setup complete. Run `make serve`.");
            Ok(())
        }
        "migrate" => migrate(),
        "serve" => serve().await,
        other => bail!("unknown command `{other}`; expected setup, migrate, or serve"),
    }
}

fn builder(reports: reports::ReportStore) -> ArcAppBuilder {
    ArcApp::builder()
        .register_aggregate::<AuditEventAggregate>()
        .register_data(Arc::new(reports))
        .register_data(boards::Boards::shared())
        .register_ui_host(ui::host())
        .register_ui(ui::contribution())
        .register_routes(routes::config)
}

fn init_logging() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,actix_web=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

fn prepare_env() -> anyhow::Result<()> {
    if !Path::new(".env").exists() {
        let example = fs::read_to_string(".env.example").context("missing .env.example")?;
        let mut secret = [0_u8; 64];
        rand::thread_rng().fill_bytes(&mut secret);
        let secret = secret
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        fs::write(".env", example.replace("generate-me", &secret))?;
    }
    Ok(())
}

fn migrate() -> anyhow::Result<()> {
    let url = env::var("DATABASE_URL").unwrap_or_else(|_| "database/database.sqlite".into());
    if let Some(parent) = Path::new(&url).parent() {
        fs::create_dir_all(parent)?;
    }
    let mut connection = SqliteConnection::establish(&url)?;
    connection
        .run_pending_migrations(MIGRATIONS)
        .map_err(|error| anyhow::anyhow!("migration failed: {error}"))?;
    Ok(())
}

fn start_delivery_worker() {
    let collector = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/workflow_audit.py");
    if !collector.is_file() {
        return;
    }
    std::thread::spawn(move || loop {
        let _ = std::process::Command::new("python3")
            .arg(&collector)
            .arg("flush")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        std::thread::sleep(std::time::Duration::from_secs(2));
    });
}

fn start_report_worker(reports: reports::ReportStore) {
    std::thread::spawn(move || {
        let mut failures = 0_u64;
        loop {
            match reports.refresh() {
                Ok(_) => {
                    if failures > 0 {
                        tracing::info!(failures, "Dashboard report catch-up recovered");
                        failures = 0;
                    }
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    if failures == 1 {
                        tracing::error!(error=%error, failures, "Dashboard report catch-up failed; checkpoint retained");
                    } else {
                        tracing::warn!(
                            failures,
                            "Dashboard report catch-up still failing; checkpoint retained"
                        );
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

async fn serve() -> anyhow::Result<()> {
    for key in ["APP_URL", "SECRET_KEY", "DATABASE_URL"] {
        if env::var_os(key).is_none() {
            bail!("missing {key}; run `make setup` first");
        }
    }
    let host = env::var("APP_URL")?;
    let port = env::var("APP_PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse::<u16>()?;
    info!(url=%format!("http://{host}:{port}"), "Arc audit application starting");
    // Make the shared switches available to collectors before the server accepts events.
    settings::initialize()?;
    start_delivery_worker();
    let reports = reports::ReportStore::open(&env::var("DATABASE_URL")?)?;
    reports.refresh()?;
    start_report_worker(reports.clone());
    // Arc still scans its event stream at startup. No projector is registered:
    // historical views remain intact and incremental reports own all read paths.
    builder(reports).serve(host, port).await?;
    Ok(())
}
