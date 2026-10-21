//! Architecture boards: a thin bridge to `scripts/board.py`.
//!
//! The dashboard owns no board data or logic. Every read, and the one change it
//! makes (removing a project from the list, or restoring it), shells out to the CLI
//! off the async executor, with a timeout, passing only a validated target id
//! as its own argv element. The project list feeds every page's sidebar, so it
//! is cached briefly; when the CLI is missing or fails, callers get an empty
//! list or a typed failure and the page still renders.

use actix_web::{web, HttpRequest};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

const CLI_TIMEOUT: Duration = Duration::from_secs(20);
const LIST_TTL: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Sidebar states the layout knows how to draw; anything else is shown as unmapped.
const STATES: &[&str] = &[
    "ready",
    "drifted",
    "mapping",
    "refreshing",
    "failed",
    "none",
];

/// A harness target id, checked against `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
///
/// The leading alphanumeric rules out `.` and `..`, which the CLI would
/// otherwise also try as a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetId(String);

impl TargetId {
    pub fn parse(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        let valid = (1..=128).contains(&bytes.len())
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The per-target read commands the dashboard may run.
#[derive(Debug, Clone, Copy)]
pub enum Query {
    Show,
    State,
    Overlay,
}

impl Query {
    fn args(self, id: &TargetId) -> Vec<&str> {
        match self {
            Self::Show => vec!["show", "--target", id.as_str()],
            Self::State => vec!["state", "--target", id.as_str(), "--json"],
            Self::Overlay => vec!["overlay", "--target", id.as_str(), "--json"],
        }
    }
}

/// What one CLI call produced.
#[derive(Debug)]
pub enum Outcome {
    Json(Value),
    /// `show` exits 3 when the target has no board yet.
    NotMapped,
    /// The id is well formed but not a registered harness target.
    UnknownTarget,
    Failed(String),
}

type CachedList = (Instant, Arc<Vec<Value>>);

pub struct Boards {
    python: PathBuf,
    script: PathBuf,
    db_root: PathBuf,
    timeout: Duration,
    list_ttl: Duration,
    cache: Mutex<Option<CachedList>>,
    /// Serialises list refreshes so concurrent page loads start one process.
    refresh: Mutex<()>,
}

impl Boards {
    pub fn new(script: impl Into<PathBuf>, db_root: impl Into<PathBuf>) -> Self {
        Self {
            python: PathBuf::from("python3"),
            script: absolute(script.into()),
            db_root: absolute(db_root.into()),
            timeout: CLI_TIMEOUT,
            list_ttl: LIST_TTL,
            cache: Mutex::new(None),
            refresh: Mutex::new(()),
        }
    }

    /// `HARNESS_BOARD_SCRIPT` and `HARNESS_DB_ROOT`, defaulting to this checkout.
    pub fn from_env() -> Self {
        let script = std::env::var_os("HARNESS_BOARD_SCRIPT").map_or_else(
            || {
                PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/board.py"
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

    /// The process-wide instance, so every worker shares one list cache.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<Boards>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Self::from_env())).clone()
    }

    /// The instance registered as app data, or the shared one.
    pub fn for_request(request: &HttpRequest) -> Arc<Self> {
        request
            .app_data::<web::Data<Self>>()
            .map_or_else(Self::shared, |data| data.clone().into_inner())
    }

    /// Sidebar rows from `list --json`, cached for a few seconds. Never fails:
    /// a broken CLI yields the last good list, or none.
    pub async fn list(self: Arc<Self>) -> Arc<Vec<Value>> {
        if let Some(rows) = self.fresh_list() {
            return rows;
        }
        web::block(move || self.list_blocking())
            .await
            .unwrap_or_default()
    }

    /// Removed projects from `list --removed --json`, uncached: only the
    /// Projects page shows them. A broken CLI yields none.
    pub async fn removed(self: Arc<Self>) -> Vec<Value> {
        match web::block(move || self.run(&["list", "--removed", "--json"])).await {
            Ok(Outcome::Json(Value::Array(rows))) => rows.iter().filter_map(sidebar_row).collect(),
            other => {
                tracing::warn!(outcome=?other, "removed board list failed");
                Vec::new()
            }
        }
    }

    /// Remove a project from the list (keeping its board) or restore it, then
    /// drop the cached list so the next page shows the change.
    pub async fn set_removed(self: Arc<Self>, id: TargetId, removed: bool) -> Outcome {
        let command = if removed { "remove" } else { "restore" };
        let boards = self.clone();
        let outcome = web::block(move || boards.run(&[command, "--target", id.as_str()]))
            .await
            .unwrap_or_else(|error| Outcome::Failed(error.to_string()));
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
        outcome
    }

    /// Run one per-target query off the executor.
    pub async fn query(self: Arc<Self>, query: Query, id: TargetId) -> Outcome {
        web::block(move || self.run(&query.args(&id)))
            .await
            .unwrap_or_else(|error| Outcome::Failed(error.to_string()))
    }

    fn fresh_list(&self) -> Option<Arc<Vec<Value>>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .as_ref()
            .filter(|(at, _)| at.elapsed() < self.list_ttl)
            .map(|(_, rows)| rows.clone())
    }

    fn list_blocking(&self) -> Arc<Vec<Value>> {
        let _refresh = self.refresh.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(rows) = self.fresh_list() {
            return rows;
        }
        let rows = match self.run(&["list", "--json"]) {
            Outcome::Json(Value::Array(rows)) => {
                Some(Arc::new(rows.iter().filter_map(sidebar_row).collect()))
            }
            Outcome::Json(_) => {
                tracing::warn!("board list returned a non-array document");
                None
            }
            other => {
                tracing::warn!(outcome=?other, "board list failed; the sidebar keeps its last rows");
                None
            }
        };
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        // A failure is cached too, with the previous rows, so a broken CLI is retried once per TTL.
        let rows = rows
            .or_else(|| cache.as_ref().map(|(_, rows)| rows.clone()))
            .unwrap_or_default();
        *cache = Some((Instant::now(), rows.clone()));
        rows
    }

    /// Blocking: spawn the CLI, collect stdout and stderr, enforce the timeout.
    fn run(&self, args: &[&str]) -> Outcome {
        let cwd = if self.db_root.is_dir() {
            self.db_root.as_path()
        } else {
            Path::new("/")
        };
        let child = Command::new(&self.python)
            .arg(&self.script)
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
            Err(error) => {
                return Outcome::Failed(format!("could not start the board CLI: {error}"))
            }
        };
        // Drain both pipes while waiting: a board document can exceed the pipe buffer.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
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
            return Outcome::Failed(format!(
                "the board CLI did not finish within {}s",
                self.timeout.as_secs()
            ));
        };
        match status.code() {
            Some(0) => serde_json::from_slice(&stdout).map_or_else(
                |error| Outcome::Failed(format!("the board CLI printed invalid JSON: {error}")),
                Outcome::Json,
            ),
            Some(3) => Outcome::NotMapped,
            _ if stderr.contains("no registered harness target") => Outcome::UnknownTarget,
            code => Outcome::Failed(format!(
                "the board CLI exited with {}: {}",
                code.map_or_else(|| "a signal".to_owned(), |code| code.to_string()),
                tail(&stderr, 400)
            )),
        }
    }
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buffer);
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

/// Keep only what the sidebar and project list show, and only rows whose id is
/// safe to put in a link.
fn sidebar_row(row: &Value) -> Option<Value> {
    let id = TargetId::parse(row.get("id")?.as_str()?)?;
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
    let state = Some(text("state"))
        .filter(|state| STATES.contains(state))
        .unwrap_or("none");
    let label = Some(text("label"))
        .filter(|label| !label.is_empty())
        .unwrap_or(if state == "none" { "not mapped" } else { state });
    let name = Some(text("name"))
        .filter(|name| !name.is_empty())
        .unwrap_or(id.as_str());
    Some(serde_json::json!({
        "id": id.as_str(),
        "name": name,
        "root": text("root"),
        "last_seen": text("last_seen"),
        "state": state,
        "label": label,
        "mapped_at": row.get("mapped_at").and_then(Value::as_str),
        "cards": row.get("cards").and_then(Value::as_u64).unwrap_or(0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn target_ids_are_restricted_to_a_safe_alphabet() {
        for valid in ["demo-1", "android-terminal-dfd9fce7b1a1", "a", "A.b_c-9"] {
            assert!(TargetId::parse(valid).is_some(), "{valid}");
        }
        let long = "a".repeat(129);
        for invalid in [
            "", ".", "..", "../x", "a/b", ".hidden", "-flag", "a b", "a\nb", "é", &long,
        ] {
            assert!(TargetId::parse(invalid).is_none(), "{invalid:?}");
        }
        assert!(TargetId::parse(&"a".repeat(128)).is_some());
    }

    #[test]
    fn sidebar_rows_drop_unsafe_ids_and_unknown_states() {
        assert!(sidebar_row(&json!({"id": "../x", "name": "x"})).is_none());
        assert!(sidebar_row(&json!({"name": "x"})).is_none());
        let row = sidebar_row(&json!({"id": "p-1", "state": "weird", "label": ""})).unwrap();
        assert_eq!(row["name"], "p-1");
        assert_eq!(row["state"], "none");
        assert_eq!(row["label"], "not mapped");
        let row =
            sidebar_row(&json!({"id": "p-1", "name": "P", "state": "mapping", "label": "mapping\u{2026}", "cards": 3}))
                .unwrap();
        assert_eq!(row["state"], "mapping");
        assert_eq!(row["cards"], 3);
    }

    #[test]
    fn a_missing_cli_fails_softly() {
        let boards = Boards::new("/nonexistent/board.py", "/nonexistent/db");
        assert!(matches!(
            boards.run(&["list", "--json"]),
            Outcome::Failed(_)
        ));
        assert!(boards.list_blocking().is_empty());
        // The failure is cached for the TTL instead of respawning per page.
        assert!(boards.fresh_list().is_some());
    }

    #[test]
    fn tail_respects_character_boundaries() {
        assert_eq!(tail("  abc  ", 2), "bc");
        assert_eq!(tail("ééé", 3), "é");
    }
}
