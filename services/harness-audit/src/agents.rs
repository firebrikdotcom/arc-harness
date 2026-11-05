//! Issue agents: a thin bridge to `scripts/signals-service.sh jobs`.
//!
//! The dashboard owns no job state. Every read and action runs the signals job
//! CLI off the async executor with a fixed argv, a timeout and bounded output,
//! passing only validated target ids, fingerprints, issue keys and card ids,
//! each as its own argv element. The CLI's exit code becomes the HTTP status.

use crate::boards::TargetId;
use actix_web::http::StatusCode;
use actix_web::{web, HttpRequest};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Long enough for `cargo run` to notice nothing changed and for difit to bind.
const CLI_TIMEOUT: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const STDOUT_LIMIT: u64 = 4 << 20;
const STDERR_LIMIT: u64 = 64 << 10;

/// A job fingerprint: 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn parse(raw: &str) -> Option<Self> {
        let valid = raw.len() == 64 && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An issue key or card id from a form: printable, short, and never an option.
pub fn form_word(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let valid = (1..=300).contains(&raw.len())
        && !raw.starts_with('-')
        && !raw.chars().any(char::is_control);
    valid.then(|| raw.to_owned())
}

/// The actions a job form may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stop,
    Takeover,
    Handback,
    DifitStart,
    DifitStop,
}

impl Action {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "stop" => Some(Self::Stop),
            "takeover" => Some(Self::Takeover),
            "handback" => Some(Self::Handback),
            "difit/start" => Some(Self::DifitStart),
            "difit/stop" => Some(Self::DifitStop),
            _ => None,
        }
    }

    fn words(self) -> &'static [&'static str] {
        match self {
            Self::Stop => &["stop"],
            Self::Takeover => &["takeover"],
            Self::Handback => &["handback"],
            Self::DifitStart => &["difit", "start"],
            Self::DifitStop => &["difit", "stop"],
        }
    }
}

/// What one CLI call produced: a document, or an HTTP status with an error body.
#[derive(Debug)]
pub enum Reply {
    Json(Value),
    Error(StatusCode, Value),
}

fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Reply {
    Reply::Error(
        status,
        json!({"error": {"code": code, "message": message.into()}}),
    )
}

pub struct Agents {
    shell: PathBuf,
    script: PathBuf,
    db_root: PathBuf,
    timeout: Duration,
}

impl Agents {
    pub fn new(script: impl Into<PathBuf>, db_root: impl Into<PathBuf>) -> Self {
        Self {
            shell: PathBuf::from("/bin/sh"),
            script: absolute(script.into()),
            db_root: absolute(db_root.into()),
            timeout: CLI_TIMEOUT,
        }
    }

    /// `HARNESS_SIGNALS_SCRIPT` and `HARNESS_DB_ROOT`, defaulting to this checkout.
    pub fn from_env() -> Self {
        let script = std::env::var_os("HARNESS_SIGNALS_SCRIPT").map_or_else(
            || {
                PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/signals-service.sh"
                ))
            },
            PathBuf::from,
        );
        let db_root = std::env::var_os("HARNESS_DB_ROOT").map_or_else(
            || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.harness-db")),
            PathBuf::from,
        );
        Self::new(script, db_root)
    }

    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<Agents>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Self::from_env())).clone()
    }

    /// The instance registered as app data, or the shared one.
    pub fn for_request(request: &HttpRequest) -> Arc<Self> {
        request
            .app_data::<web::Data<Self>>()
            .map_or_else(Self::shared, |data| data.clone().into_inner())
    }

    /// Every job on this machine, optionally for one project.
    pub async fn list(self: Arc<Self>, target: Option<TargetId>) -> Reply {
        let mut args = vec!["list".to_owned()];
        if let Some(target) = target {
            args.extend(["--target".to_owned(), target.as_str().to_owned()]);
        }
        self.call(args).await
    }

    pub async fn show(self: Arc<Self>, target: TargetId, job: Fingerprint) -> Reply {
        self.call(job_args(&["show"], &target, &job)).await
    }

    pub async fn analyze(
        self: Arc<Self>,
        target: TargetId,
        issue: String,
        card: String,
        reanalyze: bool,
    ) -> Reply {
        let mut args = vec![
            "analyze".to_owned(),
            "--target".to_owned(),
            target.as_str().to_owned(),
            "--issue".to_owned(),
            issue,
            "--card".to_owned(),
            card,
        ];
        if reanalyze {
            args.push("--reanalyze".to_owned());
        }
        self.call(args).await
    }

    pub async fn act(self: Arc<Self>, action: Action, target: TargetId, job: Fingerprint) -> Reply {
        self.call(job_args(action.words(), &target, &job)).await
    }

    async fn call(self: Arc<Self>, args: Vec<String>) -> Reply {
        web::block(move || self.run(&args))
            .await
            .unwrap_or_else(|e| error(StatusCode::BAD_GATEWAY, "failed", e.to_string()))
    }

    /// Blocking: spawn the CLI, collect bounded stdout and stderr, enforce the timeout.
    fn run(&self, args: &[String]) -> Reply {
        let cwd = if self.db_root.is_dir() {
            self.db_root.as_path()
        } else {
            Path::new("/")
        };
        let child = Command::new(&self.shell)
            .arg(&self.script)
            .arg("jobs")
            .arg("--db-root")
            .arg(&self.db_root)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                return error(
                    StatusCode::BAD_GATEWAY,
                    "failed",
                    format!("could not start the signals job CLI: {e}"),
                )
            }
        };
        let stdout = drain(child.stdout.take(), STDOUT_LIMIT);
        let stderr = drain(child.stderr.take(), STDERR_LIMIT);
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
        let Some(status) = status else {
            return error(
                StatusCode::BAD_GATEWAY,
                "failed",
                format!(
                    "the signals job CLI did not finish within {}s",
                    self.timeout.as_secs()
                ),
            );
        };
        let document = serde_json::from_slice::<Value>(&stdout).ok();
        let http = match status.code() {
            Some(0) => {
                return document.map_or_else(
                    || {
                        error(
                            StatusCode::BAD_GATEWAY,
                            "failed",
                            "the signals job CLI printed invalid JSON",
                        )
                    },
                    Reply::Json,
                )
            }
            Some(3) => StatusCode::NOT_FOUND,
            Some(4) => StatusCode::CONFLICT,
            Some(5) => StatusCode::UNPROCESSABLE_ENTITY,
            Some(6) => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::BAD_GATEWAY,
        };
        match document.filter(|d| d["error"]["code"].is_string()) {
            Some(body) => Reply::Error(http, body),
            None => error(
                http,
                "failed",
                format!(
                    "the signals job CLI exited with {}: {}",
                    status
                        .code()
                        .map_or_else(|| "a signal".to_owned(), |code| code.to_string()),
                    tail(&stderr, 400)
                ),
            ),
        }
    }
}

fn job_args(words: &[&str], target: &TargetId, job: &Fingerprint) -> Vec<String> {
    let mut args: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    args.extend([
        "--target".to_owned(),
        target.as_str().to_owned(),
        "--job".to_owned(),
        job.as_str().to_owned(),
    ]);
    args
}

fn absolute(path: PathBuf) -> PathBuf {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
    };
    // Resolve `..` so worktree paths and resume commands show the real location.
    path.canonicalize().unwrap_or(path)
}

fn drain(pipe: Option<impl Read + Send + 'static>, limit: u64) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = pipe {
            let mut pipe = pipe;
            let _ = (&mut pipe).take(limit).read_to_end(&mut buffer);
            // Keep reading so a chatty child never blocks on a full pipe.
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        }
        buffer
    })
}

fn tail(text: &str, max: usize) -> &str {
    let text = text.trim();
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The loopback address of a job's running diff viewer, if `show` reports one.
pub fn difit_url(shown: &Value) -> Option<String> {
    if shown["difit_running"] != true {
        return None;
    }
    let port = shown["job"]["difit"]["port"].as_u64()?;
    (1024..=65535)
        .contains(&port)
        .then(|| format!("http://127.0.0.1:{port}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_validated_before_any_argv_is_built() {
        let fp = "a".repeat(64);
        assert!(Fingerprint::parse(&fp).is_some());
        for bad in [
            "",
            "A".repeat(64).as_str(),
            &"a".repeat(63),
            &format!("{fp}a"),
            "../x",
        ] {
            assert!(Fingerprint::parse(bad).is_none(), "{bad}");
        }
        assert_eq!(
            form_word("  nightwatch#42 ").as_deref(),
            Some("nightwatch#42")
        );
        for bad in ["", "   ", "--reanalyze", "a\nb", &"x".repeat(301)] {
            assert!(form_word(bad).is_none(), "{bad:?}");
        }
        assert_eq!(Action::parse("difit/start"), Some(Action::DifitStart));
        assert_eq!(Action::parse("fix"), None);
    }

    #[test]
    fn the_viewer_link_is_loopback_only_and_needs_a_live_viewer() {
        let shown = json!({"difit_running": true, "job": {"difit": {"port": 41234}}});
        assert_eq!(
            difit_url(&shown).as_deref(),
            Some("http://127.0.0.1:41234/")
        );
        assert!(
            difit_url(&json!({"difit_running": false, "job": {"difit": {"port": 41234}}}))
                .is_none()
        );
        assert!(
            difit_url(&json!({"difit_running": true, "job": {"difit": {"port": 80}}})).is_none()
        );
        assert!(
            difit_url(&json!({"difit_running": true, "job": {"difit": {"port": null}}})).is_none()
        );
    }

    #[test]
    fn a_missing_cli_is_a_gateway_error() {
        let agents = Agents::new("/nonexistent/signals.sh", "/nonexistent/db");
        assert!(matches!(
            agents.run(&["list".to_owned()]),
            Reply::Error(StatusCode::BAD_GATEWAY, _)
        ));
    }
}
