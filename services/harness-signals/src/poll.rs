//! One pass over a project: ask each pull provider for its current issues,
//! replace what that provider contributed before, then recompute the heat.

use crate::config::{read_secret, Project, Provider};
use crate::heat;
use crate::providers::{command, nightwatch_poll, sentry::Sentry};
use crate::store::{lock, resolve, PollStatus};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn log(message: &str) {
    eprintln!("{} harness-signals: {message}", now());
}

pub fn poll_project(db_root: &Path, project: &Project, half_life: f64) -> Result<Value> {
    let target = resolve(db_root, &project.target)?;
    for (index, provider) in project.providers.iter().enumerate() {
        let source = provider.source(index);
        let result = match provider {
            // Webhook-only Nightwatch has nothing to poll.
            Provider::Nightwatch {
                credentials_file: None,
                ..
            } => continue,
            Provider::Nightwatch {
                credentials_file: Some(credentials),
                application,
                limit,
                ..
            } => nightwatch_poll::Poll {
                credentials: &crate::config::expand(credentials),
                application: application.as_deref(),
                environment: project.environment.as_deref(),
                limit: *limit as usize,
            }
            .poll(&target.issues()),
            Provider::Sentry {
                base_url,
                org,
                project: sentry_project,
                token_file,
                limit,
            } => read_secret(token_file).and_then(|token| {
                Sentry {
                    base_url,
                    org,
                    project: sentry_project,
                    token,
                    limit: *limit,
                    environment: project.environment.as_deref(),
                    source: source.clone(),
                }
                .poll(&target.issues())
            }),
            Provider::Command {
                argv,
                timeout_seconds,
            } => command::poll(argv, Duration::from_secs(*timeout_seconds), &source),
        };
        let _guard = lock();
        let status = match result {
            Ok(issues) => {
                let count = target.replace_source(&source, issues)?;
                PollStatus {
                    kind: provider.kind().to_owned(),
                    ok: true,
                    at: now(),
                    issues: count,
                    error: None,
                }
            }
            Err(error) => {
                log(&format!("{}: {source}: {error:#}", project.name));
                PollStatus {
                    kind: provider.kind().to_owned(),
                    ok: false,
                    at: now(),
                    issues: target
                        .issues()
                        .values()
                        .filter(|i| i.source == source)
                        .count(),
                    error: Some(format!("{error:#}").chars().take(300).collect()),
                }
            }
        };
        target.record_poll(&source, status)?;
    }
    let _guard = lock();
    let sources = project
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| provider.source(index))
        .collect();
    target.retain_sources(&sources)?;
    let heat = heat::refresh(&target, project, half_life)?;
    Ok(json!({
        "project": project.name,
        "target": target.id,
        "totals": heat["totals"],
        "sources": heat["sources"],
    }))
}
