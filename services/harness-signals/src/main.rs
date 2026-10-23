//! harness-signals: pulls errors, exceptions and warnings from the
//! observability tools of the projects this machine is configured to track,
//! maps them to architecture board cards, and writes the heat layer the
//! dashboard draws. See docs/observability-heatmap.md.
//!
//!   harness-signals [--config FILE] [--db-root DIR] check | once | serve | prune
//!   harness-signals login nightwatch [--credentials FILE] [--mcp-url URL]
//!   harness-signals tools nightwatch [--credentials FILE] [--call TOOL [--args JSON]]

mod config;
mod glob;
mod heat;
mod mcp;
mod model;
mod oauth;
mod poll;
mod providers;
mod server;
mod store;

#[cfg(test)]
mod e2e_tests;

use anyhow::{bail, Result};
use poll::log;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

const USAGE: &str = "Usage: harness-signals [--config FILE] [--db-root DIR] check|once|serve|prune
       harness-signals login nightwatch [--credentials FILE] [--mcp-url URL]
       harness-signals tools nightwatch [--credentials FILE] [--call TOOL [--args JSON]]";
const NIGHTWATCH_MCP: &str = "https://nightwatch.laravel.com/mcp";

fn main() {
    if let Err(error) = run(std::env::args().skip(1).collect()) {
        eprintln!("harness-signals: {error:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let mut config_path = config::path();
    let mut db_root = store::default_db_root();
    let mut command = None;
    let mut provider = None;
    let mut credentials = None;
    let mut mcp_url = NIGHTWATCH_MCP.to_owned();
    let mut tool = None;
    let mut tool_args = "{}".to_owned();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = PathBuf::from(args.next().unwrap_or_default()),
            "--db-root" => db_root = PathBuf::from(args.next().unwrap_or_default()),
            "--credentials" => credentials = Some(config::expand(&args.next().unwrap_or_default())),
            "--mcp-url" => mcp_url = args.next().unwrap_or_default(),
            "--call" => tool = args.next(),
            "--args" => tool_args = args.next().unwrap_or_default(),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "check" | "once" | "serve" | "prune" | "login" | "tools" if command.is_none() => {
                command = Some(arg)
            }
            "nightwatch" if matches!(command.as_deref(), Some("login" | "tools")) => {
                provider = Some(arg)
            }
            other => bail!("unexpected argument {other:?}\n{USAGE}"),
        }
    }
    if matches!(command.as_deref(), Some("login" | "tools")) && provider.is_none() {
        bail!("name the provider: nightwatch\n{USAGE}");
    }
    let credentials = credentials.unwrap_or_else(|| oauth::default_path("nightwatch"));
    match command.as_deref() {
        Some("login") => oauth::login(&mcp_url, &credentials),
        Some("tools") => {
            let arguments: Value = serde_json::from_str(&tool_args)?;
            let result = providers::nightwatch::with_client(&credentials, |client| {
                match &tool {
                Some(name) => client.call(name, arguments.clone()),
                None => Ok(Value::Array(
                    client
                        .tools()?
                        .into_iter()
                        .map(|t| json!({"name": t["name"], "description": t["description"], "input": t["inputSchema"]}))
                        .collect(),
                )),
            }
            })?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Some("prune") => {
            let config = config::load(&config_path)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "no configuration at {}: refusing to prune",
                    config_path.display()
                )
            })?;
            prune(&db_root, &config);
            Ok(())
        }
        Some("check") => {
            let report = check(&config_path, &db_root)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report["ok"] != true {
                std::process::exit(1);
            }
            Ok(())
        }
        Some("once") => {
            let summaries = once(&config_path, &db_root)?;
            println!("{}", serde_json::to_string_pretty(&summaries)?);
            Ok(())
        }
        Some("serve") => serve(&config_path, &db_root),
        _ => bail!("{USAGE}"),
    }
}

/// What this machine tracks and whether each part is usable. Never prints secrets.
fn check(config_path: &Path, db_root: &Path) -> Result<Value> {
    let Some(config) = config::load(config_path)? else {
        return Ok(json!({"ok": true, "config": config_path, "projects": [],
            "note": "no configuration file: this machine tracks nothing"}));
    };
    let mut ok = true;
    let projects: Vec<Value> = config
        .projects
        .iter()
        .map(|project| {
            let target = store::resolve(db_root, &project.target);
            let board = target.as_ref().ok().and_then(store::Target::board).is_some();
            ok &= target.is_ok();
            let providers: Vec<Value> = project
                .providers
                .iter()
                .enumerate()
                .map(|(index, provider)| {
                    let mut files = Vec::new();
                    match provider {
                        config::Provider::Sentry { token_file, .. } => files.push(token_file),
                        config::Provider::Nightwatch {
                            credentials_file,
                            secret_file,
                            ..
                        } => files.extend(credentials_file.iter().chain(secret_file.iter())),
                        config::Provider::Command { .. } => {}
                    }
                    let files: Vec<Value> = files
                        .into_iter()
                        .map(|file| {
                            let readable = config::read_secret(file).is_ok();
                            ok &= readable;
                            json!({"file": file, "readable": readable})
                        })
                        .collect();
                    json!({"source": provider.source(index), "kind": provider.kind(), "secret_files": files})
                })
                .collect();
            json!({
                "name": project.name,
                "target": match &target {
                    Ok(t) => json!(t.id),
                    Err(e) => json!({"error": e.to_string()}),
                },
                "board": board,
                "providers": providers,
                "webhook": project.providers.iter().any(|p| matches!(p, config::Provider::Nightwatch { secret_file: Some(_), .. }))
                    .then(|| format!("http://{}/hooks/nightwatch/{}", config.listen, project.name)),
            })
        })
        .collect();
    Ok(
        json!({"ok": ok, "config": config_path, "db_root": db_root, "listen": config.listen,
              "poll_seconds": config.poll_seconds, "projects": projects}),
    )
}

/// Drop the signals of targets a loaded configuration no longer names. When
/// any configured project cannot be resolved right now, nothing is pruned, so a
/// briefly missing checkout never loses history that cannot be fetched again.
fn prune(db_root: &Path, config: &config::Config) {
    let mut tracked = std::collections::BTreeSet::new();
    for project in &config.projects {
        match store::resolve(db_root, &project.target) {
            Ok(target) => {
                tracked.insert(target.id);
            }
            Err(_) => {
                log(&format!(
                    "{}: target not found; skipping cleanup this cycle",
                    project.name
                ));
                return;
            }
        }
    }
    for id in store::prune(db_root, &tracked) {
        log(&format!(
            "{id}: no longer tracked here; its error heat was removed"
        ));
    }
}

fn once(config_path: &Path, db_root: &Path) -> Result<Value> {
    let Some(config) = config::load(config_path)? else {
        return Ok(json!([]));
    };
    Ok(Value::Array(
        config
            .projects
            .iter()
            .map(|project| {
                poll::poll_project(db_root, project, config.half_life_minutes).unwrap_or_else(
                    |error| json!({"project": project.name, "error": format!("{error:#}")}),
                )
            })
            .collect(),
    ))
}

/// Re-read the configuration and poll every project, forever. The webhook
/// receiver binds once, at the address configured at start.
fn serve(config_path: &Path, db_root: &Path) -> Result<()> {
    let initial = config::load(config_path)?;
    let listen = initial
        .as_ref()
        .map_or_else(|| "127.0.0.1:18090".to_owned(), |c| c.listen.clone());
    let shared: server::Shared = Arc::new(RwLock::new(initial));
    let http = server::bind(&listen)?;
    log(&format!(
        "listening on {listen}; config {}",
        config_path.display()
    ));
    let (poll_now, requested) = std::sync::mpsc::channel::<String>();
    {
        let (db_root, shared) = (db_root.to_owned(), shared.clone());
        std::thread::spawn(move || server::run(http, db_root, shared, poll_now));
    }
    loop {
        match config::load(config_path) {
            Ok(fresh) => {
                *shared.write().unwrap_or_else(PoisonError::into_inner) = fresh;
            }
            Err(error) => log(&format!("keeping the last good configuration: {error:#}")),
        }
        let config = shared
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let wait = config.as_ref().map_or(60, |c| c.poll_seconds);
        if let Some(config) = &config {
            prune(db_root, config);
        }
        for project in config.iter().flat_map(|c| c.projects.iter()) {
            let half_life = config.as_ref().map_or(180.0, |c| c.half_life_minutes);
            if let Err(error) = poll::poll_project(db_root, project, half_life) {
                log(&format!("{}: {error:#}", project.name));
            }
        }
        // Wait for the next cycle, polling a project early when a webhook asks.
        let deadline = std::time::Instant::now() + Duration::from_secs(wait);
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            let Ok(name) = requested.recv_timeout(left) else {
                break;
            };
            let config = shared
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some((project, half_life)) = config.as_ref().and_then(|c| {
                c.projects
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| (p.clone(), c.half_life_minutes))
            }) {
                if let Err(error) = poll::poll_project(db_root, &project, half_life) {
                    log(&format!("{name}: {error:#}"));
                }
            }
        }
    }
}
