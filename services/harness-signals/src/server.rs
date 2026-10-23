//! The webhook receiver: `POST /hooks/nightwatch/<project name>` and
//! `GET /healthz`. A delivery counts only with a valid signature. When the
//! project also polls Nightwatch, a delivery just triggers an immediate poll;
//! otherwise it is applied directly and the heat recomputed at once.

use crate::config::{read_secret, Config, Provider};
use crate::poll::{log, now};
use crate::providers::nightwatch::{self, Outcome};
use crate::store::{lock, resolve, PollStatus};
use anyhow::{anyhow, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, PoisonError, RwLock};

const BODY_LIMIT: usize = 256 * 1024;

pub type Shared = Arc<RwLock<Option<Config>>>;

pub fn bind(listen: &str) -> Result<tiny_http::Server> {
    tiny_http::Server::http(listen).map_err(|error| anyhow!("listening on {listen}: {error}"))
}

pub fn run(server: tiny_http::Server, db_root: PathBuf, config: Shared, poll_now: Sender<String>) {
    for mut request in server.incoming_requests() {
        let (status, body) = handle(&mut request, &db_root, &config, &poll_now);
        let response = tiny_http::Response::from_string(body).with_status_code(status);
        let _ = request.respond(response);
    }
}

fn handle(
    request: &mut tiny_http::Request,
    db_root: &Path,
    config: &Shared,
    poll_now: &Sender<String>,
) -> (u16, String) {
    let url = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    if url == "/healthz" && method == tiny_http::Method::Get {
        return (200, "ok\n".into());
    }
    let Some(name) = url.strip_prefix("/hooks/nightwatch/") else {
        return (404, "not found\n".into());
    };
    if method != tiny_http::Method::Post {
        return (405, "use POST\n".into());
    }
    let config = config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let Some(project) = config
        .as_ref()
        .and_then(|c| c.projects.iter().find(|p| p.name == name))
        .cloned()
    else {
        return (404, "unknown project\n".into());
    };
    let Some(Provider::Nightwatch {
        secret_file,
        application,
        credentials_file,
        ..
    }) = project
        .providers
        .iter()
        .find(|p| matches!(p, Provider::Nightwatch { .. }))
        .cloned()
    else {
        return (404, "no nightwatch provider for this project\n".into());
    };
    let Some(secret_file) = secret_file else {
        return (
            404,
            "nightwatch webhooks are not enabled for this project\n".into(),
        );
    };
    if request.body_length().is_some_and(|n| n > BODY_LIMIT) {
        return (413, "too large\n".into());
    }
    let mut body = Vec::new();
    if request
        .as_reader()
        .take(BODY_LIMIT as u64 + 1)
        .read_to_end(&mut body)
        .is_err()
    {
        return (400, "unreadable body\n".into());
    }
    if body.len() > BODY_LIMIT {
        return (413, "too large\n".into());
    }
    let signature = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Nightwatch-Signature"))
        .map(|h| h.value.as_str().to_owned())
        .unwrap_or_default();
    let secret = match read_secret(&secret_file) {
        Ok(secret) => secret,
        Err(error) => {
            log(&format!("{name}: {error:#}"));
            return (503, "receiver not configured\n".into());
        }
    };
    if !nightwatch::verify(&secret, &body, &signature) {
        return (401, "bad signature\n".into());
    }
    let Ok(delivery) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (400, "invalid JSON\n".into());
    };
    if credentials_file.is_some() {
        let _ = poll_now.send(project.name.clone());
        return (202, "poll scheduled\n".into());
    }
    let target = match resolve(db_root, &project.target) {
        Ok(target) => target,
        Err(error) => {
            log(&format!("{name}: {error:#}"));
            return (503, "target not registered on this machine\n".into());
        }
    };
    let half_life = config.as_ref().map_or(180.0, |c| c.half_life_minutes);
    let _guard = lock();
    let mut issues = target.issues();
    let outcome = nightwatch::apply(
        &mut issues,
        &delivery,
        project.environment.as_deref(),
        application.as_deref(),
    );
    let stored = (|| -> Result<()> {
        if let Outcome::Updated(_) = outcome {
            target.save_issues(&issues)?;
        }
        target.record_poll(
            nightwatch::SOURCE,
            PollStatus {
                kind: "nightwatch".into(),
                ok: true,
                at: now(),
                issues: issues
                    .values()
                    .filter(|i| i.source == nightwatch::SOURCE && i.status == "open")
                    .count(),
                error: None,
            },
        )?;
        crate::heat::refresh(&target, &project, half_life)?;
        Ok(())
    })();
    match (stored, outcome) {
        (Err(error), _) => {
            log(&format!("{name}: {error:#}"));
            (500, "could not store the delivery\n".into())
        }
        (Ok(()), Outcome::Updated(_)) => (204, String::new()),
        (Ok(()), Outcome::Ignored(why)) => (202, format!("ignored: {why}\n")),
    }
}
