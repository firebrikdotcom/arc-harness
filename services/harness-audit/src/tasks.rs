//! Task managers: a thin bridge to `scripts/task_managers.py`.
//!
//! The dashboard owns no task data, credentials or run state. Every page and
//! every action shells out to the CLI off the async executor with a fixed argv,
//! a timeout and bounded output. Ids are parsed into their own types before
//! they can become argv elements, and free text from a form (a comment, a
//! reject reason) travels in a private temporary file, never as argv text.
//!
//! Every route is for the machine owner only: the owner is the service's OS
//! account plus the `owner_login` the CLI's `check` reports, judged by
//! [`OwnerCheck`]. Anyone else gets a 404. So does a request whose `Host` is
//! not one this machine is known by, which is what a DNS-rebinding page sends;
//! an action must also name that same host as its `Origin`. Text that came
//! from a task manager is untrusted and reaches the page only through Tera's
//! auto-escaping.

// A gate that refuses hands back the response to send, as the handlers do.
#![allow(clippy::result_large_err)]

use crate::boards::TargetId;
use crate::owner::{Owner, OwnerCheck};
use crate::ui::render;
use actix_session::Session;
use actix_web::http::{Method, StatusCode};
use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use arc_web::ui::{Breadcrumb, TemplateBundle, TemplateDef, TemplateName, UiContribution};
use arc_web::UiRegistry;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};
use tera::Context;

mod agent_pages;
mod edit;
mod open;
mod state;

const READ_TIMEOUT: Duration = Duration::from_secs(20);
const RUN_TIMEOUT: Duration = Duration::from_secs(10);
/// `run start` reads the item from the task manager before it answers.
const RUN_START_TIMEOUT: Duration = Duration::from_secs(90);
/// Write-back (`comment`, `complete`): long enough that a slow provider is
/// not cut off halfway through a write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);
const CHECK_TTL: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const STDOUT_LIMIT: u64 = 4 << 20;
const STDERR_LIMIT: u64 = 64 << 10;
/// A comment or a reject reason, in characters.
const TEXT_LIMIT: usize = 10_000;
/// Room for [`TEXT_LIMIT`] four-byte characters, percent-encoded, plus the token.
const TEXT_FORM_BYTES: usize = 192 << 10;
/// What `owned_paths` and `review_evidence` may put on the run page.
const OWNED_PATHS_SHOWN: usize = 500;
const EVIDENCE_SHOWN: usize = 20_000;
/// The names this machine answers to on its own.
const LOCAL_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

const STYLESHEET: &str = include_str!("../public/task-managers.css");
const SCRIPT: &str = include_str!("../public/task-managers.js");

/// The UI owner; capability templates must live under `capabilities/<owner>/`.
const OWNER: &str = "harness-audit-tasks";

macro_rules! view {
    ($file:literal) => {
        TemplateDef {
            name: TemplateName(concat!("capabilities/harness-audit-tasks/", $file)),
            source: include_str!(concat!("../resources/views/tasks/", $file)),
        }
    };
}

const TEMPLATES: &[TemplateDef] = &[
    view!("base.html"),
    view!("macros.html"),
    view!("off.html"),
    view!("problem.html"),
    view!("sources.html"),
    view!("source.html"),
    view!("project.html"),
    view!("list.html"),
    view!("item.html"),
    view!("runs.html"),
    view!("run.html"),
    view!("open.html"),
    view!("agents.html"),
];

const VIEW_OFF: &str = "capabilities/harness-audit-tasks/off.html";
const VIEW_PROBLEM: &str = "capabilities/harness-audit-tasks/problem.html";
const VIEW_SOURCES: &str = "capabilities/harness-audit-tasks/sources.html";
const VIEW_SOURCE: &str = "capabilities/harness-audit-tasks/source.html";
const VIEW_PROJECT: &str = "capabilities/harness-audit-tasks/project.html";
const VIEW_LIST: &str = "capabilities/harness-audit-tasks/list.html";
const VIEW_ITEM: &str = "capabilities/harness-audit-tasks/item.html";
const VIEW_RUNS: &str = "capabilities/harness-audit-tasks/runs.html";
const VIEW_RUN: &str = "capabilities/harness-audit-tasks/run.html";

/// The task-manager pages, registered beside the host's own templates.
pub fn contribution() -> UiContribution {
    UiContribution {
        owner: OWNER,
        templates: TemplateBundle {
            templates: TEMPLATES,
        },
        ..UiContribution::default()
    }
}

// ---- ids -------------------------------------------------------------------

/// A provider id, checked against `^[a-z][a-z0-9_]{0,31}$`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        let valid = (1..=32).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_');
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An item, project or list id, checked against `^[0-9]{1,20}$`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumId(String);

impl NumId {
    pub fn parse(raw: &str) -> Option<Self> {
        let valid = (1..=20).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_digit());
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A run id, checked against `^[0-9a-f]{32}$`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunId(String);

impl RunId {
    pub fn parse(raw: &str) -> Option<Self> {
        let valid = raw.len() == 32 && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The agents a run may be delegated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// The two points where a run waits for its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Plan,
    Review,
}

impl Gate {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "plan" => Some(Self::Plan),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Review => "review",
        }
    }
}

// ---- bridge ----------------------------------------------------------------

/// Why a CLI call produced no document, from its exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Exit 2: the CLI refused the arguments (or a module is not installed).
    Usage,
    /// Exit 3: the feature is off or the provider is not connected.
    Off,
    /// Exit 4.
    NotFound,
    /// Exit 5: the task manager or its sign-in failed.
    Upstream,
    /// Could not start, printed no JSON, or another exit code.
    Broken,
    /// Killed at the timeout: whatever it was doing may or may not have happened.
    TimedOut,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    pub code: String,
    pub message: String,
}

impl Failure {
    fn broken(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Broken,
            code: "failed".to_owned(),
            message: message.into(),
        }
    }
}

type Outcome = Result<Value, Failure>;

/// A `host` or `host:port`, lower-cased: a `Host` header, the authority of an
/// `Origin`, or an entry of `allowed_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    host: String,
    port: Option<u16>,
}

impl HostPort {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.to_ascii_lowercase();
        let (host, port) = if raw.starts_with('[') {
            let (host, rest) = raw.split_at(raw.find(']')? + 1);
            let port = match rest {
                "" => None,
                rest => Some(rest.strip_prefix(':')?),
            };
            (host, port)
        } else {
            match raw.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (raw.as_str(), None),
            }
        };
        let port = match port {
            Some(digits) => {
                let plain = (1..=5).contains(&digits.len())
                    && digits.bytes().all(|byte| byte.is_ascii_digit());
                Some(digits.parse::<u16>().ok().filter(|_| plain)?)
            }
            None => None,
        };
        let valid = match host.strip_prefix('[') {
            Some(inner) => {
                let inner = inner.strip_suffix(']')?;
                !inner.is_empty()
                    && inner.len() <= 45
                    && inner
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() || byte == b':' || byte == b'.')
            }
            None => {
                (1..=253).contains(&host.len())
                    && host.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'.' | b'-' | b'_')
                    })
            }
        };
        valid.then(|| Self {
            host: host.to_owned(),
            port,
        })
    }

    /// The host of an `http(s)://host[:port][/…]` address: an `Origin`, a
    /// `Referer`, or `APP_URL` (which may also be a bare host).
    fn of_url(url: &str, scheme_required: bool) -> Option<Self> {
        let url = url.trim();
        let rest = match url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
        {
            Some(rest) => rest,
            None if scheme_required || url.contains("://") => return None,
            None => url,
        };
        Self::parse(rest.split(['/', '?', '#']).next()?)
    }

    /// Whether `entry` (from configuration) admits this host. An entry with
    /// no port admits the host on any port.
    fn admitted_by(&self, entry: &Self) -> bool {
        entry.host == self.host && (entry.port.is_none() || entry.port == self.port)
    }
}

/// What `check` says about this machine's task-manager setup.
#[derive(Debug, Clone, Default)]
pub struct Check {
    pub enabled: bool,
    pub owner_login: Option<String>,
    pub repos: Vec<String>,
    pub mappings: BTreeMap<String, String>,
    /// `{id, label, connected, account, token_expires_at}`, ids safe for a link.
    pub providers: Vec<Value>,
    /// The names, beyond loopback and `APP_URL`, the pages may be browsed under.
    pub allowed_hosts: Vec<HostPort>,
    /// When set, a loopback request must carry the owner's `Tailscale-User-Login`.
    pub require_login_header: bool,
    /// Set when `check` itself failed; the feature is then treated as off.
    pub problem: Option<String>,
}

impl Check {
    fn from_json(document: &Value) -> Self {
        let providers = document["providers"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let id = ProviderId::parse(row["id"].as_str()?)?;
                        let label = Some(text(row, "label"))
                            .filter(|label| !label.is_empty())
                            .unwrap_or_else(|| id.as_str().to_owned());
                        Some(json!({
                            "id": id.as_str(),
                            "label": label,
                            "connected": row["connected"].as_bool().unwrap_or(false),
                            "account": scalar(&row["account"]),
                            "token_expires_at": text(row, "token_expires_at"),
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let repos: Vec<String> = document["repos"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .filter(|name| TargetId::parse(name).is_some())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mappings = document["mappings"]
            .as_object()
            .map(|rows| {
                rows.iter()
                    .filter_map(|(key, repo)| Some((key.clone(), repo.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            enabled: document["enabled"].as_bool().unwrap_or(false),
            owner_login: document["owner_login"]
                .as_str()
                .map(str::trim)
                .filter(|login| !login.is_empty())
                .map(str::to_owned),
            repos,
            mappings,
            providers,
            allowed_hosts: document["allowed_hosts"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .filter_map(HostPort::parse)
                        .collect()
                })
                .unwrap_or_default(),
            require_login_header: document["require_login_header"].as_bool().unwrap_or(false),
            problem: None,
        }
    }

    fn provider(&self, id: &ProviderId) -> Option<&Value> {
        self.providers
            .iter()
            .find(|row| row["id"].as_str() == Some(id.as_str()))
    }

    fn mapped_repo(&self, provider: &ProviderId, list: &str) -> Option<&str> {
        self.mappings
            .get(&format!("{}:{list}", provider.as_str()))
            .map(String::as_str)
            .filter(|repo| self.repos.iter().any(|name| name == repo))
    }
}

type CachedCheck = (Instant, Arc<Check>);

pub struct Tasks {
    python: PathBuf,
    script: PathBuf,
    db_root: PathBuf,
    /// The OS account this service runs as: half of the owner the gate checks.
    service_user: String,
    /// The host of `APP_URL`, when the service was started with one.
    app_host: Option<HostPort>,
    /// `APP_PORT`: the port the service is reached on, beside the bound one.
    app_port: Option<u16>,
    start_timeout: Duration,
    write_timeout: Duration,
    cache: Mutex<Option<CachedCheck>>,
}

impl Tasks {
    pub fn new(
        script: impl Into<PathBuf>,
        db_root: impl Into<PathBuf>,
        service_user: impl Into<String>,
    ) -> Self {
        Self {
            python: PathBuf::from("python3"),
            script: absolute(script.into()),
            db_root: absolute(db_root.into()),
            service_user: service_user.into(),
            app_host: None,
            app_port: None,
            start_timeout: RUN_START_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
            cache: Mutex::new(None),
        }
    }

    /// Whether a request's `Host` is a name this machine is known by:
    /// loopback (bare, or on a port the service listens on), `APP_URL`, or an
    /// `allowed_hosts` entry.
    fn knows_host(&self, host: &HostPort, bound_port: u16, check: &Check) -> bool {
        let local = LOCAL_HOSTS.contains(&host.host.as_str())
            && match host.port {
                Some(port) => port == bound_port || Some(port) == self.app_port,
                None => true,
            };
        local
            || self
                .app_host
                .iter()
                .chain(&check.allowed_hosts)
                .any(|entry| host.admitted_by(entry))
    }

    /// `HARNESS_TASKS_SCRIPT` and `HARNESS_DB_ROOT`, defaulting to this checkout.
    pub fn from_env() -> Self {
        let script = std::env::var_os("HARNESS_TASKS_SCRIPT").map_or_else(
            || {
                PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/task_managers.py"
                ))
            },
            PathBuf::from,
        );
        let db_root = std::env::var_os("HARNESS_DB_ROOT").map_or_else(
            || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.harness-db")),
            PathBuf::from,
        );
        // The same account `OwnerCheck` takes as the machine owner.
        let user = ["USER", "LOGNAME"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .unwrap_or_default();
        let mut tasks = Self::new(script, db_root, user);
        // main.rs binds `APP_URL` as a bare host; a full address is read too.
        tasks.app_host = std::env::var("APP_URL").ok().and_then(|url| {
            let bare_v6 = !url.contains('[') && url.matches(':').count() > 1;
            if bare_v6 {
                HostPort::parse(&format!("[{}]", url.trim()))
            } else {
                HostPort::of_url(&url, false)
            }
        });
        tasks.app_port = std::env::var("APP_PORT")
            .ok()
            .and_then(|port| port.trim().parse().ok());
        tasks
    }

    /// The process-wide instance, so every worker shares one `check` cache.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<Tasks>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Self::from_env())).clone()
    }

    /// The instance registered as app data, or the shared one.
    pub fn for_request(request: &HttpRequest) -> Arc<Self> {
        request
            .app_data::<web::Data<Self>>()
            .map_or_else(Self::shared, |data| data.clone().into_inner())
    }

    /// `check`, cached for a few seconds because every request starts with it.
    /// Never fails: a broken CLI reads as the feature being off.
    pub async fn check(self: Arc<Self>) -> Arc<Check> {
        if let Some(check) = self.fresh_check() {
            return check;
        }
        web::block(move || self.check_blocking())
            .await
            .unwrap_or_default()
    }

    fn fresh_check(&self) -> Option<Arc<Check>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .as_ref()
            .filter(|(at, _)| at.elapsed() < CHECK_TTL)
            .map(|(_, check)| check.clone())
    }

    fn check_blocking(&self) -> Arc<Check> {
        let check = match self.run(&words(&["check", "--json"]), READ_TIMEOUT) {
            Ok(document) => Check::from_json(&document),
            Err(failure) => {
                tracing::warn!(error=%failure.message, "task manager check failed");
                Check {
                    problem: Some(failure.message),
                    ..Check::default()
                }
            }
        };
        let check = Arc::new(check);
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), check.clone()));
        check
    }

    pub async fn projects(self: Arc<Self>, provider: ProviderId) -> Outcome {
        let args = words(&["projects", "--provider", provider.as_str(), "--json"]);
        self.call(args, READ_TIMEOUT).await
    }

    pub async fn lists(self: Arc<Self>, provider: ProviderId, project: NumId) -> Outcome {
        let args = words(&[
            "lists",
            "--provider",
            provider.as_str(),
            "--project",
            project.as_str(),
            "--json",
        ]);
        self.call(args, READ_TIMEOUT).await
    }

    pub async fn todos(self: Arc<Self>, provider: ProviderId, list: NumId, done: bool) -> Outcome {
        let args = words(&[
            "todos",
            "--provider",
            provider.as_str(),
            "--list",
            list.as_str(),
            "--status",
            if done { "done" } else { "open" },
            "--json",
        ]);
        self.call(args, READ_TIMEOUT).await
    }

    pub async fn item(self: Arc<Self>, provider: ProviderId, item: NumId) -> Outcome {
        let args = words(&[
            "get",
            "--provider",
            provider.as_str(),
            "--item",
            item.as_str(),
            "--json",
        ]);
        self.call(args, READ_TIMEOUT).await
    }

    pub async fn run_start(
        self: Arc<Self>,
        provider: ProviderId,
        item: NumId,
        repo: TargetId,
        agent: Agent,
        plan_gate: bool,
    ) -> Outcome {
        let mut args = words(&[
            "run",
            "start",
            "--provider",
            provider.as_str(),
            "--item",
            item.as_str(),
            "--repo",
            repo.as_str(),
            "--agent",
            agent.word(),
        ]);
        if !plan_gate {
            args.push("--no-plan-gate".into());
        }
        args.push("--json".into());
        let timeout = self.start_timeout;
        self.call(args, timeout).await
    }

    pub async fn run_list(self: Arc<Self>) -> Outcome {
        self.call(words(&["run", "list", "--json"]), RUN_TIMEOUT)
            .await
    }

    pub async fn run_status(self: Arc<Self>, run: RunId) -> Outcome {
        let args = words(&["run", "status", "--id", run.as_str(), "--json"]);
        self.call(args, RUN_TIMEOUT).await
    }

    pub async fn run_approve(self: Arc<Self>, run: RunId, gate: Gate) -> Outcome {
        let args = words(&[
            "run",
            "approve",
            "--id",
            run.as_str(),
            "--gate",
            gate.word(),
            "--json",
        ]);
        self.call(args, RUN_TIMEOUT).await
    }

    pub async fn run_reject(self: Arc<Self>, run: RunId, gate: Gate, reason: String) -> Outcome {
        let args = words(&["run", "reject", "--id", run.as_str(), "--gate", gate.word()]);
        self.call_with_text(args, "--reason-file", reason, RUN_TIMEOUT)
            .await
    }

    pub async fn run_cancel(self: Arc<Self>, run: RunId) -> Outcome {
        let args = words(&["run", "cancel", "--id", run.as_str(), "--json"]);
        self.call(args, RUN_TIMEOUT).await
    }

    pub async fn comment(
        self: Arc<Self>,
        provider: ProviderId,
        item: NumId,
        body: String,
    ) -> Outcome {
        let args = words(&[
            "comment",
            "--provider",
            provider.as_str(),
            "--item",
            item.as_str(),
        ]);
        let timeout = self.write_timeout;
        self.call_with_text(args, "--text-file", body, timeout)
            .await
    }

    pub async fn complete(self: Arc<Self>, provider: ProviderId, item: NumId) -> Outcome {
        let args = words(&[
            "complete",
            "--provider",
            provider.as_str(),
            "--item",
            item.as_str(),
            "--json",
        ]);
        let timeout = self.write_timeout;
        self.call(args, timeout).await
    }

    async fn call(self: Arc<Self>, args: Vec<OsString>, timeout: Duration) -> Outcome {
        web::block(move || self.run(&args, timeout))
            .await
            .unwrap_or_else(|error| Err(Failure::broken(error.to_string())))
    }

    /// Run a command whose free text goes in a private file named by `option`.
    /// The file is removed when the call returns.
    async fn call_with_text(
        self: Arc<Self>,
        mut args: Vec<OsString>,
        option: &'static str,
        body: String,
        timeout: Duration,
    ) -> Outcome {
        web::block(move || {
            let file = TextFile::create(&body).map_err(|error| {
                Failure::broken(format!(
                    "could not write the text to a private file: {error}"
                ))
            })?;
            args.push(option.into());
            args.push(file.path().into());
            args.push("--json".into());
            self.run(&args, timeout)
        })
        .await
        .unwrap_or_else(|error| Err(Failure::broken(error.to_string())))
    }

    /// Blocking: spawn the CLI, collect bounded stdout and stderr, enforce the timeout.
    fn run(&self, args: &[OsString], timeout: Duration) -> Outcome {
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
            .env("HARNESS_DB_ROOT", &self.db_root)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                return Err(Failure::broken(format!(
                    "could not start the task manager CLI: {error}"
                )))
            }
        };
        let stdout = drain(child.stdout.take(), STDOUT_LIMIT);
        let stderr = drain(child.stderr.take(), STDERR_LIMIT);
        let deadline = Instant::now() + timeout;
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
            return Err(Failure {
                kind: FailureKind::TimedOut,
                code: "timed_out".to_owned(),
                message: format!(
                    "the task manager CLI did not finish within {}s",
                    timeout.as_secs()
                ),
            });
        };
        let document = serde_json::from_slice::<Value>(&stdout).ok();
        let kind = match status.code() {
            Some(0) => {
                return document
                    .ok_or_else(|| Failure::broken("the task manager CLI printed invalid JSON"))
            }
            Some(2) => FailureKind::Usage,
            Some(3) => FailureKind::Off,
            Some(4) => FailureKind::NotFound,
            Some(5) => FailureKind::Upstream,
            _ => FailureKind::Broken,
        };
        let reported = document.as_ref().map(|document| &document["error"]);
        let code = reported
            .and_then(|error| error["code"].as_str())
            .unwrap_or("failed");
        let message = reported
            .and_then(|error| error["message"].as_str())
            .map(|message| clip(message.trim(), 400))
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "the task manager CLI exited with {}: {}",
                    status
                        .code()
                        .map_or_else(|| "a signal".to_owned(), |code| code.to_string()),
                    tail(&stderr, 400)
                )
            });
        Err(Failure {
            kind,
            code: clip(code, 64),
            message,
        })
    }
}

/// Form text on its way to the CLI: a new file only this account can read.
struct TextFile(PathBuf);

impl TextFile {
    fn create(body: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "harness-tasks-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        let mut handle = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let file = Self(path);
        handle.write_all(body.as_bytes())?;
        handle.flush()?;
        Ok(file)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TextFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn words(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
    }
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

/// At most `max` characters, with an ellipsis when something was cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(max).collect();
    clipped.push('\u{2026}');
    clipped
}

// ---- shaping CLI documents for the templates -------------------------------

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A string or number as display text; anything else is empty.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

/// An id from a CLI document, kept only when it is safe to put in a link.
fn num_id(value: &Value) -> Option<String> {
    NumId::parse(&scalar(value)).map(|id| id.0)
}

/// A link out of the dashboard, kept only when it is plain http(s).
fn http_url(value: &Value) -> Option<String> {
    let url = value.as_str()?.trim();
    let safe = (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_control() || c.is_whitespace());
    safe.then(|| url.to_owned())
}

fn named(value: &Value) -> Value {
    json!({"id": num_id(&value["id"]), "name": text(value, "name")})
}

fn shape_item(item: &Value) -> Option<Value> {
    let id = num_id(&item["id"])?;
    let assignees: Vec<&str> = item["assignees"]
        .as_array()
        .map(|rows| rows.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Some(json!({
        "id": id,
        "title": text(item, "title"),
        "description_text": text(item, "description_text"),
        "status": if item["status"] == "done" { "done" } else { "open" },
        "assignees": assignees,
        "due_on": text(item, "due_on"),
        "url": http_url(&item["url"]),
        "updated_at": text(item, "updated_at"),
        "comments_count": item["comments_count"].as_u64().unwrap_or(0),
        "project": named(&item["project"]),
        "list": named(&item["list"]),
    }))
}

const TERMINAL: &[&str] = &["failed", "rejected", "cancelled"];

/// `(state, step name, what happens in it)`, in the order a run moves through them.
const STEPS: &[(&str, &str, &str)] = &[
    (
        "queued",
        "Queued",
        "Run created from the item; its text is fenced as untrusted",
    ),
    (
        "planning",
        "Plan",
        "The agent reads the repository and writes the task contract",
    ),
    (
        "awaiting_plan_approval",
        "Your approval: plan",
        "The contract waits for you before any file changes",
    ),
    (
        "building",
        "Build",
        "Edits on the run's branch, inside the contract's scope",
    ),
    (
        "verifying",
        "Verify",
        "The verification suite and every acceptance command",
    ),
    (
        "reviewing",
        "Independent review",
        "A fresh-context reviewer; the author cannot approve itself",
    ),
    (
        "opening_pr",
        "Draft PR",
        "The branch is pushed and a draft pull request opened",
    ),
    (
        "awaiting_review_approval",
        "Your approval: review",
        "You approve the findings; the harness never merges",
    ),
    (
        "ready_to_merge",
        "Ready for you to merge",
        "Done on the harness side. Nothing merges on its own",
    ),
];

/// A badge label and tone for a run state.
fn state_badge(state: &str) -> (&'static str, &'static str) {
    match state {
        "queued" => ("Queued", "neutral"),
        "planning" => ("Planning", "neutral"),
        "awaiting_plan_approval" => ("Needs you: plan", "warning"),
        "building" => ("Building", "neutral"),
        "verifying" => ("Verifying", "neutral"),
        "reviewing" => ("In review", "neutral"),
        "opening_pr" => ("Opening PR", "neutral"),
        "awaiting_review_approval" => ("Needs you: review", "warning"),
        "ready_to_merge" => ("Ready to merge", "success"),
        "failed" => ("Failed", "error"),
        "rejected" => ("Rejected", "ghost"),
        "cancelled" => ("Cancelled", "ghost"),
        _ => ("Unknown", "ghost"),
    }
}

fn agent_label(agent: &str) -> &'static str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        _ => "Agent",
    }
}

/// The gate a run is waiting at, if any.
fn waiting_gate(state: &str) -> Option<Gate> {
    match state {
        "awaiting_plan_approval" => Some(Gate::Plan),
        "awaiting_review_approval" => Some(Gate::Review),
        _ => None,
    }
}

/// The fields shared by a run summary and a run document.
fn shape_run(run: &Value) -> Option<Value> {
    let id = RunId::parse(run["id"].as_str()?)?;
    let state = text(run, "state");
    let (label, tone) = state_badge(&state);
    let provider = ProviderId::parse(&text(run, "provider"));
    let item_id = num_id(&run["item_id"]);
    let item_href = provider
        .as_ref()
        .zip(item_id.as_ref())
        .map(|(provider, item)| format!("/tasks/{}/items/{item}", provider.as_str()));
    let agent = text(run, "agent");
    let updated_at = Some(text(run, "updated_at"))
        .filter(|at| !at.is_empty())
        .or_else(|| {
            run["history"]
                .as_array()
                .and_then(|history| history.last())
                .map(|entry| text(entry, "at"))
        })
        .unwrap_or_default();
    Some(json!({
        "id": id.as_str(),
        "provider": provider.as_ref().map(ProviderId::as_str),
        "item_id": item_id,
        "item_href": item_href,
        "repo": text(run, "repo"),
        "agent_label": agent_label(&agent),
        "state": state,
        "label": label,
        "tone": tone,
        "updated_at": updated_at,
        "pr_url": http_url(&run["pr_url"]),
    }))
}

/// The phase stepper: each state a run passes through, marked done, active,
/// waiting (on the owner), stopped (where a terminal run ended) or pending.
fn steps(state: &str, plan_gate: bool, history: &[Value]) -> Vec<Value> {
    let index_of = |state: &str| STEPS.iter().position(|(key, _, _)| *key == state);
    let stopped = index_of(state).is_none();
    let position = index_of(state).or_else(|| {
        history
            .iter()
            .rev()
            .find_map(|entry| index_of(entry["state"].as_str()?))
    });
    STEPS
        .iter()
        .enumerate()
        .filter(|(_, (key, _, _))| plan_gate || *key != "awaiting_plan_approval")
        .map(|(index, (key, name, detail))| {
            let class = match position {
                Some(position) if index < position => "done",
                Some(position) if index == position => {
                    if stopped {
                        "stopped"
                    } else if *key == "ready_to_merge" {
                        "done"
                    } else if waiting_gate(key).is_some() {
                        "waiting"
                    } else {
                        "active"
                    }
                }
                _ => "pending",
            };
            let at = history
                .iter()
                .find(|entry| entry["state"].as_str() == Some(key))
                .map(|entry| text(entry, "at"))
                .unwrap_or_default();
            json!({"key": key, "name": name, "detail": detail, "class": class, "at": at})
        })
        .collect()
}

// ---- rendering -------------------------------------------------------------

fn plain(status: StatusCode, body: impl Into<String>) -> HttpResponse {
    HttpResponse::build(status)
        .content_type("text/plain; charset=utf-8")
        .insert_header(("Cache-Control", "no-store"))
        .body(body.into())
}

fn not_found() -> HttpResponse {
    plain(StatusCode::NOT_FOUND, "Not found.")
}

fn see_other(location: &str) -> HttpResponse {
    HttpResponse::SeeOther()
        .insert_header(("Location", location))
        .finish()
}

fn crumbs_root() -> Breadcrumb {
    Breadcrumb::link("Task Managers", "/tasks")
}

/// The one value of a header, or nothing when it is absent, repeated or not text.
fn sole_header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    let mut values = request.headers().get_all(name);
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// Where a request says it was sent from: its `Origin`, or the origin of its
/// `Referer` when it has no `Origin` at all.
fn request_origin(request: &HttpRequest) -> Option<HostPort> {
    let name = if request.headers().contains_key("origin") {
        "origin"
    } else {
        "referer"
    };
    HostPort::of_url(sole_header(request, name)?, true)
}

/// The owner gate. Everyone but the machine owner gets `Err(404)`, and so does
/// a request for a `Host` this machine is not known by (a DNS-rebinding page
/// reaches loopback under its own name). A request that changes something must
/// also come from a page of that same host, or it gets `Err(403)`.
async fn gate(request: &HttpRequest) -> Result<(Arc<Tasks>, Arc<Check>), HttpResponse> {
    let tasks = Tasks::for_request(request);
    let check = tasks.clone().check().await;
    let bound_port = request.app_config().local_addr().port();
    let Some(host) = sole_header(request, "host")
        .and_then(HostPort::parse)
        .filter(|host| tasks.knows_host(host, bound_port, &check))
    else {
        return Err(not_found());
    };
    let owner = Owner {
        user: tasks.service_user.clone(),
        login: check.owner_login.clone(),
    };
    let requester = OwnerCheck::who(request);
    // Behind `tailscale serve` or a local proxy every request is a loopback
    // one; only the login header tells the owner from anyone else.
    let headerless_loopback = requester.serve_login.is_none()
        && requester
            .peer
            .is_some_and(|peer| peer.to_canonical().is_loopback());
    if check.require_login_header && headerless_loopback {
        return Err(not_found());
    }
    let allowed = OwnerCheck::for_request(request)
        .allows(requester, owner)
        .await;
    if !allowed {
        return Err(not_found());
    }
    let reads = [Method::GET, Method::HEAD].contains(request.method());
    if !reads && request_origin(request).as_ref() != Some(&host) {
        return Err(plain(
            StatusCode::FORBIDDEN,
            "This action must be sent from the dashboard's own page.",
        ));
    }
    Ok((tasks, check))
}

/// One owner-gated page request with the feature on.
struct Page<'a> {
    request: &'a HttpRequest,
    session: &'a Session,
    registry: &'a UiRegistry,
    tasks: Arc<Tasks>,
    check: Arc<Check>,
}

impl<'a> Page<'a> {
    /// The gate, then the "not configured" page when the feature is off.
    async fn open(
        request: &'a HttpRequest,
        session: &'a Session,
        registry: &'a UiRegistry,
    ) -> Result<Page<'a>, HttpResponse> {
        let (tasks, check) = gate(request).await?;
        if !check.enabled {
            let mut context = Context::new();
            context.insert("problem", &check.problem);
            return Err(render(
                registry,
                request,
                session,
                VIEW_OFF,
                "Task Managers",
                context,
                vec![Breadcrumb::current("Task Managers")],
            )
            .await);
        }
        Ok(Page {
            request,
            session,
            registry,
            tasks,
            check,
        })
    }

    /// A provider the CLI lists, or nothing (a 404 for the caller).
    fn provider(&self, raw: &str) -> Option<(ProviderId, &Value)> {
        let id = ProviderId::parse(raw)?;
        let row = self.check.provider(&id)?;
        Some((id, row))
    }

    async fn show(
        &self,
        template: &'static str,
        title: &str,
        active: &str,
        mut context: Context,
        breadcrumbs: Vec<Breadcrumb>,
    ) -> HttpResponse {
        context.insert("tm_sources", &self.check.providers);
        context.insert("tm_active", active);
        render(
            self.registry,
            self.request,
            self.session,
            template,
            title,
            context,
            breadcrumbs,
        )
        .await
    }

    /// A read that failed: 404 as such, anything else as a page saying why.
    async fn failed(&self, active: &str, failure: Failure) -> HttpResponse {
        let (status, heading) = match failure.kind {
            FailureKind::NotFound => return not_found(),
            FailureKind::Off => (StatusCode::OK, "This source is not connected"),
            FailureKind::Upstream => (StatusCode::BAD_GATEWAY, "The task manager did not answer"),
            FailureKind::Usage | FailureKind::Broken => {
                (StatusCode::BAD_GATEWAY, "The task manager CLI failed")
            }
            FailureKind::TimedOut => (
                StatusCode::GATEWAY_TIMEOUT,
                "The task manager CLI timed out",
            ),
        };
        tracing::warn!(code=%failure.code, error=%failure.message, "task manager read failed");
        let mut context = Context::new();
        context.insert("heading", heading);
        context.insert("detail", &failure.message);
        let mut response = self
            .show(
                VIEW_PROBLEM,
                heading,
                active,
                context,
                vec![crumbs_root(), Breadcrumb::current("Problem")],
            )
            .await;
        if response.status() == StatusCode::OK {
            *response.status_mut() = status;
        }
        response
    }
}

macro_rules! open_page {
    ($request:expr, $session:expr, $registry:expr) => {
        match Page::open(&$request, &$session, &$registry).await {
            Ok(page) => page,
            Err(response) => return response,
        }
    };
}

// ---- pages -----------------------------------------------------------------

#[get("/public/task-managers.css")]
async fn tasks_stylesheet() -> impl Responder {
    asset("text/css; charset=utf-8", STYLESHEET)
}

#[get("/public/task-managers.js")]
async fn tasks_script() -> impl Responder {
    asset("text/javascript; charset=utf-8", SCRIPT)
}

fn asset(content_type: &'static str, body: &'static str) -> HttpResponse {
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(("Cache-Control", "public, max-age=300"))
        .body(body)
}

/// `GET /tasks`: every source the CLI knows, connected or not.
#[get("/tasks")]
async fn sources_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let connected = page
        .check
        .providers
        .iter()
        .filter(|row| row["connected"] == true)
        .count();
    let mut context = Context::new();
    context.insert("connected_count", &connected);
    context.insert("repos", &page.check.repos);
    page.show(
        VIEW_SOURCES,
        "Task Managers",
        "home",
        context,
        vec![Breadcrumb::current("Task Managers")],
    )
    .await
}

/// `GET /tasks/{provider}`: a connected source's projects, or how to connect it.
#[get("/tasks/{provider}")]
async fn source_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
    path: web::Path<String>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let Some((provider, source)) = page.provider(&path) else {
        return not_found();
    };
    let label = text(source, "label");
    let mut context = Context::new();
    context.insert("source", source);
    let mut projects = Vec::new();
    if source["connected"] == true {
        let document = match page.tasks.clone().projects(provider.clone()).await {
            Ok(document) => document,
            Err(failure) => return page.failed(provider.as_str(), failure).await,
        };
        projects = document["projects"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some(json!({
                            "id": num_id(&row["id"])?,
                            "name": text(row, "name"),
                            "url": http_url(&row["url"]),
                            "lists_count": row["lists_count"].as_u64().unwrap_or(0),
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    context.insert("projects", &projects);
    if source["connected"] == true {
        context.insert("open_section", &open::section(&page, &provider).await);
    }
    page.show(
        VIEW_SOURCE,
        &label,
        provider.as_str(),
        context,
        vec![crumbs_root(), Breadcrumb::current(label.clone())],
    )
    .await
}

/// `GET /tasks/{provider}/projects/{id}`: a project's lists and their repositories.
#[get("/tasks/{provider}/projects/{id}")]
async fn project_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let (raw_provider, raw_id) = path.into_inner();
    let (Some((provider, source)), Some(id)) =
        (page.provider(&raw_provider), NumId::parse(&raw_id))
    else {
        return not_found();
    };
    let document = match page.tasks.clone().lists(provider.clone(), id.clone()).await {
        Ok(document) => document,
        Err(failure) => return page.failed(provider.as_str(), failure).await,
    };
    let name = Some(text(&document["project"], "name"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Project {}", id.as_str()));
    let lists: Vec<Value> = document["lists"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let list = num_id(&row["id"])?;
                    Some(json!({
                        "repo": page.check.mapped_repo(&provider, &list),
                        "id": list,
                        "name": text(row, "name"),
                        "open_count": row["open_count"].as_u64().unwrap_or(0),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    let list_ids = lists
        .iter()
        .filter_map(|list| list["id"].as_str().map(str::to_owned));
    let associations =
        edit::associations(&request, &page.check, &provider, list_ids.collect()).await;
    let mut lists = lists;
    for list in &mut lists {
        let found = list["id"]
            .as_str()
            .and_then(|list| associations.as_ref()?.get(list));
        list["association"] = found.cloned().unwrap_or(Value::Null);
    }
    let label = text(source, "label");
    let mut context = Context::new();
    context.insert("source", source);
    context.insert("project", &json!({"id": id.as_str(), "name": name}));
    context.insert("lists", &lists);
    context.insert("can_associate", &associations.is_some());
    context.insert("repos", &page.check.repos);
    page.show(
        VIEW_PROJECT,
        &name,
        provider.as_str(),
        context,
        vec![
            crumbs_root(),
            Breadcrumb::link(label, format!("/tasks/{}", provider.as_str())),
            Breadcrumb::current(clip(&name, 60)),
        ],
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    status: Option<String>,
}

/// `GET /tasks/{provider}/lists/{id}`: a list's open (or `?status=done`) items.
#[get("/tasks/{provider}/lists/{id}")]
async fn list_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
    path: web::Path<(String, String)>,
    query: Option<web::Query<ListQuery>>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let (raw_provider, raw_id) = path.into_inner();
    let (Some((provider, source)), Some(id)) =
        (page.provider(&raw_provider), NumId::parse(&raw_id))
    else {
        return not_found();
    };
    let done = query.is_some_and(|query| query.status.as_deref() == Some("done"));
    let document = match page
        .tasks
        .clone()
        .todos(provider.clone(), id.clone(), done)
        .await
    {
        Ok(document) => document,
        Err(failure) => return page.failed(provider.as_str(), failure).await,
    };
    let name = Some(text(&document["list"], "name"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("List {}", id.as_str()));
    let project = named(&document["project"]);
    let items: Vec<Value> = document["items"]
        .as_array()
        .map(|rows| rows.iter().filter_map(shape_item).collect())
        .unwrap_or_default();
    let label = text(source, "label");
    let mut breadcrumbs = vec![
        crumbs_root(),
        Breadcrumb::link(label, format!("/tasks/{}", provider.as_str())),
    ];
    if let Some(project_id) = project["id"].as_str() {
        breadcrumbs.push(Breadcrumb::link(
            clip(&text(&project, "name"), 60),
            format!("/tasks/{}/projects/{project_id}", provider.as_str()),
        ));
    }
    breadcrumbs.push(Breadcrumb::current(clip(&name, 60)));
    let mut context = Context::new();
    context.insert("source", source);
    context.insert("project", &project);
    context.insert("list", &json!({"id": id.as_str(), "name": name}));
    context.insert("repo", &page.check.mapped_repo(&provider, id.as_str()));
    let associations = edit::associations(
        &request,
        &page.check,
        &provider,
        vec![id.as_str().to_owned()],
    )
    .await;
    context.insert(
        "association",
        &associations.as_ref().map(|found| found.get(id.as_str())),
    );
    context.insert("can_associate", &associations.is_some());
    context.insert("repos", &page.check.repos);
    context.insert("status", if done { "done" } else { "open" });
    context.insert("items", &items);
    page.show(VIEW_LIST, &name, provider.as_str(), context, breadcrumbs)
        .await
}

/// `GET /tasks/{provider}/items/{id}`: one item, with the delegate, comment
/// and complete forms.
#[get("/tasks/{provider}/items/{id}")]
async fn item_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
    path: web::Path<(String, String)>,
    query: Option<web::Query<edit::ItemQuery>>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let (raw_provider, raw_id) = path.into_inner();
    let (Some((provider, source)), Some(id)) =
        (page.provider(&raw_provider), NumId::parse(&raw_id))
    else {
        return not_found();
    };
    let full = edit::ItemQuery::full(query.as_ref());
    let read = if full {
        page.tasks
            .clone()
            .item_full(provider.clone(), id.clone())
            .await
    } else {
        page.tasks.clone().item(provider.clone(), id.clone()).await
    };
    let document = match read {
        Ok(document) => document,
        Err(failure) => return page.failed(provider.as_str(), failure).await,
    };
    let Some(item) = shape_item(&document["item"]) else {
        return not_found();
    };
    // A run already working on this item replaces the delegate form. The run
    // list is advisory here: when it cannot be read the form is shown.
    let active_run = match page.tasks.clone().run_list().await {
        Ok(runs) => runs["runs"].as_array().and_then(|rows| {
            rows.iter()
                .filter(|run| {
                    run["provider"].as_str() == Some(provider.as_str())
                        && num_id(&run["item_id"]).as_deref() == Some(id.as_str())
                        && !TERMINAL.contains(&run["state"].as_str().unwrap_or_default())
                })
                .find_map(shape_run)
        }),
        Err(_) => None,
    };
    let list_id = item["list"]["id"].as_str().map(str::to_owned);
    let up = num_id(&document["up"]).or_else(|| list_id.clone());
    let default_repo = up
        .as_deref()
        .and_then(|list| page.check.mapped_repo(&provider, list));
    let label = text(source, "label");
    let base = format!("/tasks/{}", provider.as_str());
    let mut breadcrumbs = vec![crumbs_root(), Breadcrumb::link(label, base.clone())];
    if let Some(project_id) = item["project"]["id"].as_str() {
        breadcrumbs.push(Breadcrumb::link(
            clip(&text(&item["project"], "name"), 40),
            format!("{base}/projects/{project_id}"),
        ));
    }
    if let Some(list) = up.as_deref() {
        breadcrumbs.push(Breadcrumb::link(
            clip(&text(&item["list"], "name"), 40),
            format!("{base}/lists/{list}"),
        ));
    }
    let title = text(&item, "title");
    breadcrumbs.push(Breadcrumb::current(clip(&title, 40)));
    let mut context = Context::new();
    context.insert("source", source);
    context.insert("item", &item);
    context.insert("prev", &num_id(&document["prev"]));
    context.insert("next", &num_id(&document["next"]));
    context.insert("up", &up);
    context.insert("repos", &page.check.repos);
    context.insert("default_repo", &default_repo);
    context.insert("active_run", &active_run);
    context.insert("full", &full);
    let panel = agent_pages::panel(&request, &page.check, &provider, &id, up.clone()).await;
    context.insert("agent_panel", &panel);
    page.show(VIEW_ITEM, &title, provider.as_str(), context, breadcrumbs)
        .await
}

/// `GET /tasks/runs`: every delegated run.
#[get("/tasks/runs")]
async fn runs_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let document = match page.tasks.clone().run_list().await {
        Ok(document) => document,
        Err(failure) => return page.failed("runs", failure).await,
    };
    let runs: Vec<Value> = document["runs"]
        .as_array()
        .map(|rows| rows.iter().filter_map(shape_run).collect())
        .unwrap_or_default();
    let waiting = runs.iter().filter(|run| run["tone"] == "warning").count();
    let mut context = Context::new();
    context.insert("runs", &runs);
    context.insert("waiting", &waiting);
    page.show(
        VIEW_RUNS,
        "Agent runs",
        "runs",
        context,
        vec![crumbs_root(), Breadcrumb::current("Agent runs")],
    )
    .await
}

/// `GET /tasks/runs/{run}`: one run's phases, plan, gates and pull request.
#[get("/tasks/runs/{run}")]
async fn run_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
    path: web::Path<String>,
) -> HttpResponse {
    let page = open_page!(request, session, registry);
    let Some(id) = RunId::parse(&path) else {
        return not_found();
    };
    let document = match page.tasks.clone().run_status(id.clone()).await {
        Ok(document) => document,
        Err(failure) => return page.failed("runs", failure).await,
    };
    let Some(mut run) = shape_run(&document) else {
        return not_found();
    };
    let state = text(&document, "state");
    let plan_gate = document["plan_gate"].as_bool().unwrap_or(true);
    let history: Vec<Value> = document["history"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|entry| {
                    json!({
                        "state": text(entry, "state"),
                        "at": text(entry, "at"),
                        "note": text(entry, "note"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let known = STEPS.iter().any(|(key, _, _)| *key == state);
    let waiting = waiting_gate(&state).map(Gate::word);
    let plan_text = ["plan_text", "plan"]
        .iter()
        .find_map(|key| document[*key].as_str())
        .map(str::trim)
        .filter(|plan| !plan.is_empty());
    run["plan_gate"] = json!(plan_gate);
    run["branch"] = json!(text(&document, "branch"));
    run["error"] = json!(scalar(&document["error"]));
    run["plan_text"] = json!(plan_text);
    // What each gate authorises: the paths the build may change, and the
    // reviewer's evidence. Both are the agent's words, shown escaped.
    let owned_paths: Vec<String> = document["owned_paths"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .take(OWNED_PATHS_SHOWN)
                .map(|path| clip(path, 400))
                .collect()
        })
        .unwrap_or_default();
    let review_evidence = match &document["review_evidence"] {
        Value::Null => None,
        Value::String(evidence) => Some(evidence.trim().to_owned()),
        other => Some(other.to_string()),
    }
    .filter(|evidence| !evidence.is_empty())
    .map(|evidence| clip(&evidence, EVIDENCE_SHOWN));
    run["owned_paths"] = json!(owned_paths);
    run["review_evidence"] = json!(review_evidence);
    run["gate"] = json!(waiting);
    run["can_cancel"] = json!(known && state != "ready_to_merge");
    // Pages for a run that is working refresh themselves; one that waits for
    // the owner must not, or a half-written reject reason would be lost.
    run["working"] = json!(known && waiting.is_none() && state != "ready_to_merge");
    let mut context = Context::new();
    context.insert("run", &run);
    context.insert("steps", &steps(&state, plan_gate, &history));
    context.insert("history", &history);
    page.show(
        VIEW_RUN,
        "Agent run",
        "runs",
        context,
        vec![
            crumbs_root(),
            Breadcrumb::link("Agent runs", "/tasks/runs"),
            Breadcrumb::current(clip(id.as_str(), 8)),
        ],
    )
    .await
}

// ---- actions ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CsrfForm {
    #[serde(default)]
    csrf_token: String,
}

#[derive(Debug, Deserialize)]
struct DelegateForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    agent: String,
    /// A checkbox: present when ticked.
    plan_gate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TextForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct ReasonForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    reason: String,
}

/// The gate for an action: host and owner first (404), then the page it was
/// sent from (403), then the feature (404), then the session token (403). A
/// form that could not be read has no token.
async fn act(
    request: &HttpRequest,
    session: &Session,
    csrf_token: Option<&str>,
) -> Result<(Arc<Tasks>, Arc<Check>), HttpResponse> {
    let (tasks, check) = gate(request).await?;
    if !check.enabled {
        return Err(not_found());
    }
    let valid =
        csrf_token.is_some_and(|token| arc_web::helpers::csrf::validate_csrf_token(session, token));
    if !valid {
        return Err(plain(
            StatusCode::FORBIDDEN,
            "The form expired; reload the page and try again.",
        ));
    }
    Ok((tasks, check))
}

/// Form text for the CLI: trimmed, bounded, and free of NUL.
fn form_text(raw: &str, required: bool) -> Result<String, HttpResponse> {
    let body = raw.trim();
    if required && body.is_empty() {
        return Err(plain(StatusCode::BAD_REQUEST, "Write something first."));
    }
    if body.chars().count() > TEXT_LIMIT || body.contains('\0') {
        return Err(plain(
            StatusCode::BAD_REQUEST,
            format!("The text is limited to {TEXT_LIMIT} characters."),
        ));
    }
    Ok(body.to_owned())
}

/// A finished action: back to a page, or the CLI's refusal as plain text.
fn acted(outcome: Outcome, location: &str) -> HttpResponse {
    match outcome {
        Ok(_) => see_other(location),
        Err(failure) => refused(&failure),
    }
}

fn refused(failure: &Failure) -> HttpResponse {
    tracing::warn!(code=%failure.code, error=%failure.message, "task manager action failed");
    let status = match failure.kind {
        FailureKind::NotFound => return not_found(),
        FailureKind::Usage => StatusCode::BAD_REQUEST,
        FailureKind::Off => StatusCode::CONFLICT,
        FailureKind::Upstream | FailureKind::Broken => StatusCode::BAD_GATEWAY,
        // The CLI was stopped partway, so the write or the run may exist.
        FailureKind::TimedOut => {
            return plain(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "The outcome is unknown: {}, so this action may or may not have gone \
                     through. Check the item and the runs list (/tasks/runs) before retrying.",
                    failure.message
                ),
            )
        }
    };
    plain(
        status,
        format!("The task manager CLI refused: {}", failure.message),
    )
}

/// A provider the CLI lists and an item id, or nothing (a 404 for the caller).
fn item_target(check: &Check, provider: &str, item: &str) -> Option<(ProviderId, NumId)> {
    let provider = ProviderId::parse(provider)?;
    check.provider(&provider)?;
    Some((provider, NumId::parse(item)?))
}

/// `POST /tasks/{provider}/items/{id}/delegate`: start a run, then show it.
#[post("/tasks/{provider}/items/{id}/delegate")]
async fn delegate(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<DelegateForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_item) = path.into_inner();
    let Some((provider, item)) = item_target(&check, &raw_provider, &raw_item) else {
        return not_found();
    };
    let Some(form) = form else {
        return plain(StatusCode::BAD_REQUEST, "The form could not be read.");
    };
    let (Some(repo), Some(agent)) = (TargetId::parse(&form.repo), Agent::parse(&form.agent)) else {
        return plain(StatusCode::BAD_REQUEST, "Choose a repository and an agent.");
    };
    if !check.repos.iter().any(|name| name == repo.as_str()) {
        return plain(StatusCode::BAD_REQUEST, "Choose a configured repository.");
    }
    let started = tasks
        .run_start(provider, item, repo, agent, form.plan_gate.is_some())
        .await;
    match started {
        Ok(document) => match document["run_id"].as_str().and_then(RunId::parse) {
            Some(run) => see_other(&format!("/tasks/runs/{}", run.as_str())),
            None => plain(
                StatusCode::BAD_GATEWAY,
                "The task manager CLI started a run without a usable id.",
            ),
        },
        Err(failure) if failure.code == "run_active" => plain(
            StatusCode::CONFLICT,
            "A run is already active for this item; see the runs list (/tasks/runs).",
        ),
        Err(failure) => refused(&failure),
    }
}

/// `POST /tasks/{provider}/items/{id}/comment`: post a comment on the item.
async fn comment(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<TextForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_item) = path.into_inner();
    let Some((provider, item)) = item_target(&check, &raw_provider, &raw_item) else {
        return not_found();
    };
    let body = match form_text(form.as_ref().map_or("", |form| form.text.as_str()), true) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let location = format!("/tasks/{}/items/{}", provider.as_str(), item.as_str());
    acted(tasks.comment(provider, item, body).await, &location)
}

/// `POST /tasks/{provider}/items/{id}/complete`: mark the item done upstream.
#[post("/tasks/{provider}/items/{id}/complete")]
async fn complete(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<CsrfForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_item) = path.into_inner();
    let Some((provider, item)) = item_target(&check, &raw_provider, &raw_item) else {
        return not_found();
    };
    let location = format!("/tasks/{}/items/{}", provider.as_str(), item.as_str());
    acted(tasks.complete(provider, item).await, &location)
}

/// `POST /tasks/runs/{run}/approve/{plan|review}`.
#[post("/tasks/runs/{run}/approve/{gate}")]
async fn run_approve(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<CsrfForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, _) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_run, raw_gate) = path.into_inner();
    let (Some(run), Some(gate)) = (RunId::parse(&raw_run), Gate::parse(&raw_gate)) else {
        return not_found();
    };
    let location = format!("/tasks/runs/{}", run.as_str());
    acted(tasks.run_approve(run, gate).await, &location)
}

/// `POST /tasks/runs/{run}/reject/{plan|review}`, with an optional reason.
async fn run_reject(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<ReasonForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, _) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_run, raw_gate) = path.into_inner();
    let (Some(run), Some(gate)) = (RunId::parse(&raw_run), Gate::parse(&raw_gate)) else {
        return not_found();
    };
    let reason = match form_text(form.as_ref().map_or("", |form| form.reason.as_str()), false) {
        Ok(reason) => reason,
        Err(response) => return response,
    };
    let location = format!("/tasks/runs/{}", run.as_str());
    acted(tasks.run_reject(run, gate, reason).await, &location)
}

/// `POST /tasks/runs/{run}/cancel`.
#[post("/tasks/runs/{run}/cancel")]
async fn run_cancel(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<CsrfForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, _) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let Some(run) = RunId::parse(&path) else {
        return not_found();
    };
    let location = format!("/tasks/runs/{}", run.as_str());
    acted(tasks.run_cancel(run).await, &location)
}

/// The task-manager routes. `/tasks/runs` is registered before
/// `/tasks/{provider}` so "runs" is never read as a provider.
pub fn config(cfg: &mut web::ServiceConfig) {
    let text_form = || web::FormConfig::default().limit(TEXT_FORM_BYTES);
    cfg.service(tasks_stylesheet)
        .service(tasks_script)
        .service(runs_page)
        .service(run_page)
        .service(run_approve)
        .service(
            web::resource("/tasks/runs/{run}/reject/{gate}")
                .app_data(text_form())
                .route(web::post().to(run_reject)),
        )
        .service(run_cancel);
    agent_pages::config(cfg);
    open::config(cfg);
    edit::config(cfg);
    cfg.service(sources_page)
        .service(source_page)
        .service(project_page)
        .service(list_page)
        .service(item_page)
        .service(delegate)
        .service(
            web::resource("/tasks/{provider}/items/{id}/comment")
                .app_data(text_form())
                .route(web::post().to(comment)),
        )
        .service(complete);
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_session::{storage::CookieSessionStore, SessionMiddleware};
    use actix_web::cookie::Key;
    use actix_web::{test as actix_test, App};
    use std::net::SocketAddr;

    const RUN: &str = "0123456789abcdef0123456789abcdef";
    const RUN_BUILDING: &str = "ffffffffffffffffffffffffffffffff";
    const RUN_REVIEW: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// A well-formed attach code no agent holds.
    const CODE: &str = "tm-abc123";

    /// A stand-in for `scripts/task_managers.py` that logs every argv it gets
    /// and the content and mode of any text file it is handed.
    const FAKE_CLI: &str = r##"
import json, os, sys
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
with open(os.path.join(here, "argv.log"), "a") as log:
    log.write(json.dumps(args) + "\n")
# The agent runtime calls the CLI without HARNESS_DB_ROOT; the bridge always sets it.
assert args[0] == "--db-root" and os.environ.get("HARNESS_DB_ROOT", args[1]) == args[1]
rest = args[2:]
ENABLED = __ENABLED__
RUN, RUN_BUILDING, RUN_REVIEW = "0123456789abcdef0123456789abcdef", "f" * 32, "a" * 32

def opt(name):
    return rest[rest.index(name) + 1] if name in rest else None

def out(doc, code=0):
    print(json.dumps(doc))
    sys.exit(code)

def keep(option):
    path = opt(option)
    with open(path) as handle:
        body = handle.read()
    with open(os.path.join(here, "text.log"), "a") as log:
        log.write(json.dumps({"path": path, "text": body, "mode": oct(os.stat(path).st_mode & 0o777)}) + "\n")

if rest[0] == "check":
    if not ENABLED:
        out({"enabled": False, "owner_login": None, "repos": [], "mappings": {}, "providers": []})
    out(dict({"enabled": True, "owner_login": "me@example.com", "repos": ["arc", "harness-template", "../bad"],
         "mappings": {"basecamp:20": "arc"}, "repo_paths": {"arc": os.path.join(here, "app")},
         "providers": [
             {"id": "basecamp", "label": "Basecamp", "connected": True, "account": "9999999", "token_expires_at": "2026-12-01T00:00:00Z"},
             {"id": "linear", "label": "Linear", "connected": False, "account": None, "token_expires_at": None},
             {"id": "Bad Id", "label": "Dropped", "connected": False}]}, **__EXTRA__))
if "--json" not in rest:
    sys.exit(2)
PROJECT = {"id": "10", "name": "Proj <b>bold</b>"}
LIST = {"id": "20", "name": "List A"}
ITEM = {"provider": "basecamp", "id": "30", "project": PROJECT, "list": LIST,
        "title": "<script>alert(1)</script> & co", "status": "open", "assignees": ["Ana <i>"],
        "due_on": "2026-12-01", "url": "javascript:alert(2)", "updated_at": "2026-11-01T10:00:00Z", "comments_count": 2}
if rest[0] == "projects":
    out({"projects": [{"id": "10", "name": PROJECT["name"], "url": "https://3.basecamp.com/1/projects/10", "lists_count": 1},
                      {"id": "../x", "name": "dropped-project", "url": "", "lists_count": 0}]})
if rest[0] == "lists":
    out({"project": PROJECT, "lists": [{"id": "20", "name": "List A", "open_count": 2}, {"id": 21, "name": "List B", "open_count": 0}]})
if rest[0] == "todos":
    out({"list": LIST, "project": PROJECT, "items": [ITEM] if opt("--status") == "open" else []})
if rest[0] == "get":
    if opt("--item") not in ("30", "31"):
        out({"error": {"code": "not_found", "message": "no such item"}}, 4)
    description = "FULL-START " + "y" * 5000 + " FULL-END" if "--full" in rest else "first line\n<img src=x onerror=alert(3)>"
    out({"item": dict(ITEM, id=opt("--item"), description_text=description), "prev": None, "next": "31", "up": "20"})
if rest[0] == "seed":
    out({"packet": "<<<TASK-MANAGER-UNTRUSTED sha256=x bytes=5>>>\nhello\n<<<END-TASK-MANAGER-UNTRUSTED>>>"})
if rest[0] == "list-create":
    keep("--name-file")
    if "--description-file" in rest:
        keep("--description-file")
    out({"list": {"id": "25", "name": "made", "url": "https://3.basecamp.com/1/lists/25"}})
if rest[0] == "todo-create":
    keep("--title-file")
    if "--description-file" in rest:
        keep("--description-file")
    out({"item": dict(ITEM, id="32")})
if rest[0] == "todo-update":
    for option in ("--title-file", "--description-file"):
        if option in rest:
            keep(option)
    out({"item": ITEM})
if rest[0] == "me":
    out({"person": {"id": "7", "name": "Ana"}})
if rest[0] == "open-walk":
    out({"items": [
        {"id": "40", "title": "<b>open one</b>", "project": PROJECT, "list": LIST, "status": "open",
         "assignees": [{"id": "7", "name": "Ana <i>"}], "due_on": "2020-01-01", "url": "https://3.basecamp.com/1/todos/40"},
        {"id": "41", "title": "second", "project": PROJECT, "list": {"id": "21", "name": "List B"}, "status": "open",
         "assignees": [{"id": "8", "name": "Bo"}], "due_on": "2999-01-01", "url": "javascript:alert(4)"},
        {"id": "42", "title": "third", "project": {"id": "11", "name": "Other"}, "list": {"id": "22", "name": "List C"},
         "status": "open", "assignees": [], "due_on": None, "url": None},
        {"id": "43", "title": "finished", "project": PROJECT, "list": LIST, "status": "done", "assignees": [], "due_on": None},
    ], "truncated": opt("--list") == "22", "limit": int(opt("--limit"))})
if rest[0] == "comment":
    keep("--text-file")
    out({"comment_id": "1", "url": "https://3.basecamp.com/c/1"})
if rest[0] == "complete":
    out({"status": "done"})
if rest[0] == "run":
    states = {RUN: "awaiting_plan_approval", RUN_BUILDING: "building", RUN_REVIEW: "awaiting_review_approval"}
    def doc(run):
        state = states[run]
        return {"id": run, "provider": "basecamp", "item_id": "31", "repo": "arc", "agent": "claude",
                "plan_gate": True, "state": state, "branch": "task/basecamp-30",
                "history": [{"state": "queued", "at": "2026-11-01T10:00:00Z", "note": "created <b>"},
                            {"state": state, "at": "2026-11-01T10:05:00Z", "note": ""}],
                "plan_text": "PLAN <b>deliverables</b>" if run == RUN else None,
                "owned_paths": ["src/<b>lib</b>.rs", "docs/a&b.md"] if run == RUN else [],
                "review_evidence": "EVIDENCE <i>3 checks</i> passed" if run == RUN_REVIEW else None,
                "pr_url": None if run == RUN else "https://github.com/example/arc/pull/7", "error": None}
    if rest[1] == "start":
        if opt("--item") == "77":
            import time
            time.sleep(30)
        if opt("--item") == "78":
            out({"error": {"code": "run_active", "message": "run " + RUN + " is active <b>"}}, 2)
        out({"run_id": RUN, "state": "queued"})
    if rest[1] == "list":
        out({"runs": [{"id": run, "provider": "basecamp", "item_id": doc(run)["item_id"], "repo": "arc", "agent": "claude",
                       "state": states[run], "updated_at": "2026-11-01T10:05:00Z", "pr_url": doc(run)["pr_url"]} for run in states]})
    if rest[1] == "status":
        if opt("--id") not in states:
            out({"error": {"code": "not_found", "message": "no such run"}}, 4)
        out(doc(opt("--id")))
    if rest[1] == "reject":
        keep("--reason-file")
        out({"state": "rejected"})
    if rest[1] in ("approve", "cancel"):
        out({"state": "building"})
sys.exit(2)
"##;

    struct Fixture {
        directory: PathBuf,
        tasks: Arc<Tasks>,
        /// A file-backed event database for the Task Managers state.
        database: String,
    }

    impl Fixture {
        fn new(enabled: bool) -> Self {
            Self::with(enabled, "{}", |_| ())
        }

        /// `extra` is a Python dict merged into the `check` document; `adjust`
        /// changes the bridge before it is shared.
        fn with(enabled: bool, extra: &str, adjust: impl FnOnce(&mut Tasks)) -> Self {
            let directory =
                std::env::temp_dir().join(format!("audit-tasks-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let script = directory.join("fake_task_managers.py");
            let enabled = if enabled { "True" } else { "False" };
            let source = FAKE_CLI
                .replace("__ENABLED__", enabled)
                .replace("__EXTRA__", extra);
            std::fs::write(&script, source).unwrap();
            let mut tasks = Tasks::new(script, &directory, "me");
            adjust(&mut tasks);
            Self::at(directory, Arc::new(tasks))
        }

        /// The fixture around a bridge: an event database and fake agent binaries.
        fn at(directory: PathBuf, tasks: Arc<Tasks>) -> Self {
            use diesel::connection::SimpleConnection;
            use diesel::Connection;
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(directory.join("bin")).unwrap();
            let database = directory.join("events.sqlite").display().to_string();
            diesel::SqliteConnection::establish(&database)
                .unwrap()
                .batch_execute(include_str!("../migrations/00000000000000_arc_base/up.sql"))
                .unwrap();
            let root = directory.display();
            for (name, body) in [
                (
                    "claude",
                    format!("for a; do printf '%s\\n' \"$a\"; done >> {root}/claude.args\necho --- >> {root}/claude.args\necho '{{\"type\":\"result\",\"is_error\":false}}'\n"),
                ),
                ("codex", "exit 1\n".to_owned()),
                (
                    "gh",
                    format!("echo \"$*\" >> {root}/gh.args\ncase \"$*\" in \"pr list\"*) echo '[]' ;; *) echo https://github.com/example/arc/pull/9 ;; esac\n"),
                ),
            ] {
                let path = directory.join("bin").join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self {
                directory,
                tasks,
                database,
            }
        }

        /// The agent runtime, on the fake CLI and the fake binaries.
        fn runtime(&self) -> crate::task_managers::agents::Runtime {
            crate::task_managers::agents::Runtime {
                tasks_cli: vec![
                    "python3".into(),
                    self.directory.join("fake_task_managers.py").into(),
                    "--db-root".into(),
                    self.directory.clone().into(),
                ],
                state_dir: self.directory.join("agent-state"),
                claude: self.directory.join("bin/claude").into(),
                codex: self.directory.join("bin/codex").into(),
                gh: self.directory.join("bin/gh").into(),
                git: "git".into(),
                ssh: None,
                takeover_wait: Duration::from_secs(5),
                session_wait: Duration::from_secs(5),
            }
        }

        /// A facade on the fixture's database, caught up.
        async fn state(&self) -> crate::task_managers::TaskManagers {
            let state = crate::task_managers::TaskManagers::open(&self.database)
                .await
                .unwrap();
            state.refresh().unwrap();
            state
        }

        fn log(&self, name: &str) -> Vec<Value> {
            std::fs::read_to_string(self.directory.join(name))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }

        /// Every argv the CLI received, without the leading `--db-root DIR`.
        fn calls(&self) -> Vec<Vec<String>> {
            self.log("argv.log")
                .iter()
                .map(|call| {
                    call.as_array()
                        .unwrap()
                        .iter()
                        .skip(2)
                        .map(|word| word.as_str().unwrap().to_owned())
                        .collect()
                })
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn registry() -> web::Data<UiRegistry> {
        web::Data::new(
            UiRegistry::build(
                Some(&crate::ui::host()),
                &[crate::ui::contribution(), contribution()],
            )
            .unwrap()
            .unwrap(),
        )
    }

    fn loopback() -> SocketAddr {
        "127.0.0.1:50000".parse().unwrap()
    }

    fn stranger() -> SocketAddr {
        "192.168.1.9:50000".parse().unwrap()
    }

    fn tailnet() -> SocketAddr {
        "100.64.0.7:50000".parse().unwrap()
    }

    macro_rules! service {
        ($fixture:expr, $check:expr) => {{
            let state = Arc::new($fixture.state().await);
            actix_test::init_service(
                App::new()
                    .wrap(SessionMiddleware::new(
                        CookieSessionStore::default(),
                        Key::generate(),
                    ))
                    .app_data(registry())
                    .app_data(web::Data::from($fixture.tasks.clone()))
                    .app_data(web::Data::new($check))
                    .app_data(web::Data::from(state))
                    .app_data(web::Data::new($fixture.runtime()))
                    .configure(config),
            )
            .await
        }};
        ($fixture:expr) => {
            service!($fixture, crate::owner::tests::check("me", None))
        };
    }

    /// The name the test service is browsed under: loopback on its bound port.
    const HOST: &str = "localhost:8080";
    const ORIGIN: &str = "http://localhost:8080";

    /// A GET as a browser on this machine sends it.
    fn page(peer: SocketAddr, uri: &str) -> actix_test::TestRequest {
        actix_test::TestRequest::get()
            .uri(uri)
            .peer_addr(peer)
            .insert_header(("Host", HOST))
    }

    /// A POST with nothing but its peer: the caller adds `Host` and `Origin`.
    fn bare_post(peer: SocketAddr, uri: &str) -> actix_test::TestRequest {
        actix_test::TestRequest::post().uri(uri).peer_addr(peer)
    }

    /// A POST as the dashboard's own page sends it.
    fn action(peer: SocketAddr, uri: &str) -> actix_test::TestRequest {
        bare_post(peer, uri)
            .insert_header(("Host", HOST))
            .insert_header(("Origin", ORIGIN))
    }

    /// `(status, Location, body)` for a prepared request.
    macro_rules! send {
        ($app:expr, $request:expr) => {{
            let response = actix_test::call_service(&$app, $request.to_request()).await;
            let status = response.status();
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let body = actix_test::read_body(response).await;
            (
                status,
                location,
                String::from_utf8_lossy(&body).into_owned(),
            )
        }};
    }

    /// `(status, body)` for a GET from a peer.
    macro_rules! get {
        ($app:expr, $peer:expr, $uri:expr) => {{
            let (status, _, body) = send!($app, page($peer, $uri));
            (status, body)
        }};
    }

    /// The session cookie and its token, read from the item page as the owner.
    macro_rules! signed_in {
        ($app:expr) => {{
            let response = actix_test::call_service(
                &$app,
                page(loopback(), "/tasks/basecamp/items/30").to_request(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let cookie = response.response().cookies().next().unwrap().into_owned();
            let html = String::from_utf8(actix_test::read_body(response).await.to_vec()).unwrap();
            let token = html
                .split("name=\"csrf_token\" value=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap()
                .to_owned();
            assert!(!token.is_empty());
            (cookie, token)
        }};
    }

    /// `(status, Location, body)` for a form POST from a peer.
    macro_rules! post {
        ($app:expr, $peer:expr, $uri:expr, $cookie:expr, $form:expr) => {{
            post!(action($peer, $uri), $app, $cookie, $form)
        }};
        ($request:expr, $app:expr, $cookie:expr, $form:expr) => {{
            let mut request = $request;
            if let Some(cookie) = $cookie {
                request = request.cookie(cookie);
            }
            let form: Option<Vec<(&str, &str)>> = $form;
            if let Some(form) = form {
                request = request.set_form(form);
            }
            send!($app, request)
        }};
    }

    fn pages() -> Vec<String> {
        vec![
            "/tasks".to_owned(),
            "/tasks/basecamp".to_owned(),
            "/tasks/linear".to_owned(),
            "/tasks/basecamp/projects/10".to_owned(),
            "/tasks/basecamp/lists/20".to_owned(),
            "/tasks/basecamp/lists/20?status=done".to_owned(),
            "/tasks/basecamp/items/30".to_owned(),
            "/tasks/runs".to_owned(),
            format!("/tasks/runs/{RUN}"),
            "/tasks/basecamp/items/30?full=1".to_owned(),
            "/tasks/basecamp/open".to_owned(),
            "/tasks/basecamp/open?project=10&overdue=on".to_owned(),
            "/tasks/agents".to_owned(),
        ]
    }

    fn actions() -> Vec<String> {
        vec![
            "/tasks/basecamp/items/30/delegate".to_owned(),
            "/tasks/basecamp/items/30/comment".to_owned(),
            "/tasks/basecamp/items/30/complete".to_owned(),
            format!("/tasks/runs/{RUN}/approve/plan"),
            format!("/tasks/runs/{RUN}/approve/review"),
            format!("/tasks/runs/{RUN}/reject/plan"),
            format!("/tasks/runs/{RUN}/reject/review"),
            format!("/tasks/runs/{RUN}/cancel"),
            "/tasks/basecamp/projects/10/lists".to_owned(),
            "/tasks/basecamp/lists/20/todos".to_owned(),
            "/tasks/basecamp/items/30/edit".to_owned(),
            "/tasks/basecamp/lists/20/associate".to_owned(),
            "/tasks/basecamp/lists/20/unassociate".to_owned(),
            "/tasks/basecamp/open/presets".to_owned(),
            "/tasks/basecamp/open/presets/mine/delete".to_owned(),
            "/tasks/basecamp/open/presets/mine/default".to_owned(),
            "/tasks/basecamp/open/default/clear".to_owned(),
            "/tasks/basecamp/items/30/agent".to_owned(),
            format!("/tasks/agents/{CODE}/stop"),
            format!("/tasks/agents/{CODE}/handback"),
            format!("/tasks/agents/{CODE}/done"),
        ]
    }

    /// A form every action accepts once its token is valid.
    fn full_form(token: &str) -> Vec<(&str, &str)> {
        vec![
            ("csrf_token", token),
            ("repo", "arc"),
            ("agent", "claude"),
            ("plan_gate", "on"),
            ("text", "a comment"),
            ("reason", "a reason"),
            ("name", "a list"),
            ("title", "a to-do"),
            ("due", "2026-12-01"),
            ("preset", "mine"),
            ("assignee", "me"),
        ]
    }

    fn only_checks(calls: &[Vec<String>]) -> bool {
        calls.iter().all(|call| call == &["check", "--json"])
    }

    #[test]
    fn ids_are_restricted_before_they_can_be_argv() {
        for valid in ["basecamp", "a", "linear_2", &"a".repeat(32)] {
            assert!(ProviderId::parse(valid).is_some(), "{valid}");
        }
        for invalid in [
            "",
            "Basecamp",
            "1abc",
            "_a",
            "base-camp",
            "a b",
            "a/b",
            "--json",
            "é",
            &"a".repeat(33),
        ] {
            assert!(ProviderId::parse(invalid).is_none(), "{invalid:?}");
        }
        for valid in ["0", "30", &"9".repeat(20)] {
            assert!(NumId::parse(valid).is_some(), "{valid}");
        }
        for invalid in [
            "",
            "-1",
            "1.5",
            "1 ",
            "abc",
            "1;rm",
            "../1",
            "١",
            &"9".repeat(21),
        ] {
            assert!(NumId::parse(invalid).is_none(), "{invalid:?}");
        }
        assert!(RunId::parse(RUN).is_some());
        for invalid in [
            "",
            "XYZ",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcde/",
        ] {
            assert!(RunId::parse(invalid).is_none(), "{invalid:?}");
        }
        assert_eq!(Agent::parse("claude"), Some(Agent::Claude));
        assert_eq!(Agent::parse("codex"), Some(Agent::Codex));
        assert!(Agent::parse("gpt").is_none() && Agent::parse("Claude").is_none());
        assert_eq!(Gate::parse("plan"), Some(Gate::Plan));
        assert_eq!(Gate::parse("review"), Some(Gate::Review));
        assert!(Gate::parse("merge").is_none());
    }

    #[test]
    fn links_out_are_http_only_and_ids_from_the_cli_are_checked() {
        assert_eq!(
            http_url(&json!("https://3.basecamp.com/1")).as_deref(),
            Some("https://3.basecamp.com/1")
        );
        for bad in [
            json!("javascript:alert(1)"),
            json!("data:text/html,x"),
            json!("//evil.example"),
            json!("https://a b"),
            json!(null),
            json!(7),
        ] {
            assert!(http_url(&bad).is_none(), "{bad}");
        }
        assert_eq!(num_id(&json!("30")).as_deref(), Some("30"));
        assert_eq!(num_id(&json!(30)).as_deref(), Some("30"));
        assert!(num_id(&json!("../x")).is_none() && num_id(&json!(null)).is_none());
        assert!(shape_run(&json!({"id": "nope", "state": "queued"})).is_none());
        assert!(shape_item(&json!({"id": "x", "title": "t"})).is_none());
    }

    #[test]
    fn the_stepper_follows_the_run_state() {
        let classes = |state: &str, plan_gate: bool, history: &[Value]| -> Vec<(String, String)> {
            steps(state, plan_gate, history)
                .iter()
                .map(|step| (text(step, "key"), text(step, "class")))
                .collect()
        };
        let waiting = classes("awaiting_plan_approval", true, &[]);
        assert_eq!(waiting.len(), 9);
        assert_eq!(waiting[1], ("planning".to_owned(), "done".to_owned()));
        assert_eq!(waiting[2].1, "waiting");
        assert_eq!(waiting[3].1, "pending");
        // Without the plan gate that step is not drawn at all.
        let building = classes("building", false, &[]);
        assert_eq!(building.len(), 8);
        assert!(building
            .iter()
            .all(|(key, _)| key != "awaiting_plan_approval"));
        assert_eq!(building[2], ("building".to_owned(), "active".to_owned()));
        let ready = classes("ready_to_merge", true, &[]);
        assert!(ready.iter().all(|(_, class)| class == "done"));
        // A terminal run stops where its history last was.
        let history = [
            json!({"state": "queued"}),
            json!({"state": "verifying"}),
            json!({"state": "failed"}),
        ];
        let failed = classes("failed", true, &history);
        assert_eq!(failed[4], ("verifying".to_owned(), "stopped".to_owned()));
        assert_eq!(failed[3].1, "done");
        assert_eq!(failed[5].1, "pending");
        assert!(classes("cancelled", true, &[])
            .iter()
            .all(|(_, class)| class == "pending"));
    }

    #[test]
    fn hosts_are_parsed_strictly() {
        let parsed = |raw: &str| HostPort::parse(raw).map(|host| (host.host, host.port));
        assert_eq!(
            parsed("LocalHost:8080"),
            Some(("localhost".to_owned(), Some(8080)))
        );
        assert_eq!(
            parsed("box.tailnet.example"),
            Some(("box.tailnet.example".to_owned(), None))
        );
        assert_eq!(
            parsed("[::1]:18080"),
            Some(("[::1]".to_owned(), Some(18080)))
        );
        assert_eq!(parsed("[::1]"), Some(("[::1]".to_owned(), None)));
        for bad in [
            "",
            ":8080",
            "localhost:",
            "localhost:99999",
            "localhost:+80",
            "localhost:80:80",
            "::1",
            "[::1",
            "[::1]x",
            "[]",
            "user@localhost",
            "local host",
            "localhost/path",
            "localhost,evil.example",
        ] {
            assert!(HostPort::parse(bad).is_none(), "{bad:?}");
        }
        let origin = |url: &str| HostPort::of_url(url, true).map(|host| (host.host, host.port));
        assert_eq!(
            origin("https://Box.tailnet.example/tasks?x=1"),
            Some(("box.tailnet.example".to_owned(), None))
        );
        for bad in [
            "null",
            "localhost:8080",
            "ftp://localhost",
            "file:///x",
            "http://",
        ] {
            assert!(origin(bad).is_none(), "{bad:?}");
        }
        // `APP_URL` is a bare bind host in this service, or a full address.
        assert!(HostPort::of_url("0.0.0.0", false).is_some());
        assert!(HostPort::of_url("https://audit.example:8443/", false).is_some());
        assert!(HostPort::of_url("ftp://audit.example", false).is_none());
        // Missing keys in `check` read as no extra hosts and no header rule.
        let check = Check::from_json(&json!({"enabled": true}));
        assert!(check.allowed_hosts.is_empty() && !check.require_login_header);
        let check = Check::from_json(
            &json!({"allowed_hosts": ["a.example", "b.example:9000", "bad host", 7], "require_login_header": true}),
        );
        assert_eq!(check.allowed_hosts.len(), 2);
        assert!(check.require_login_header);
        assert_eq!(RUN_START_TIMEOUT, Duration::from_secs(90));
        assert_eq!(WRITE_TIMEOUT, Duration::from_secs(120));
        assert_eq!(RUN_TIMEOUT, Duration::from_secs(10));
        assert_eq!(READ_TIMEOUT, Duration::from_secs(20));
    }

    #[actix_web::test]
    async fn a_foreign_host_gets_404_even_from_the_loopback_owner() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let before = fixture.calls().len();
        // The rebinding page's own name, a loopback name on another port, a
        // missing Host, a repeated one and one that is not a host at all.
        let hosts: [&[&str]; 9] = [
            &["evil.example"],
            &["evil.example:8080"],
            &["localhost.evil.example:8080"],
            &["localhost:9999"],
            &["127.0.0.1:1"],
            &[],
            &["localhost:8080", "localhost:8080"],
            &["localhost:8080, evil.example"],
            &["user@localhost:8080"],
        ];
        for host in hosts {
            for uri in pages() {
                let mut request = actix_test::TestRequest::get()
                    .uri(&uri)
                    .peer_addr(loopback());
                for value in host {
                    request = request.append_header(("Host", *value));
                }
                let (status, _, body) = send!(app, request);
                assert_eq!(status, StatusCode::NOT_FOUND, "{host:?} {uri}");
                assert!(!body.contains("csrf_token"), "{host:?} {uri}");
            }
            for uri in actions() {
                // Same-origin for the rebinding page, with the owner's session and token.
                let mut request = bare_post(loopback(), &uri);
                for value in host {
                    request = request
                        .append_header(("Host", *value))
                        .insert_header(("Origin", format!("http://{value}")));
                }
                let (status, _, _) =
                    post!(request, app, Some(cookie.clone()), Some(full_form(&token)));
                assert_eq!(status, StatusCode::NOT_FOUND, "{host:?} {uri}");
            }
        }
        assert!(only_checks(&fixture.calls()[before..]));
        // Every loopback name is known, bare or on the bound port.
        for host in [
            "localhost",
            "LOCALHOST:8080",
            "127.0.0.1",
            "127.0.0.1:8080",
            "[::1]",
            "[::1]:8080",
        ] {
            let request = actix_test::TestRequest::get()
                .uri("/tasks")
                .peer_addr(loopback())
                .insert_header(("Host", host));
            assert_eq!(send!(app, request).0, StatusCode::OK, "{host}");
        }
    }

    #[actix_web::test]
    async fn configured_hosts_are_admitted() {
        let fixture = Fixture::with(
            true,
            r#"{"allowed_hosts": ["box.tailnet.example", "100.64.0.1:18080"]}"#,
            |tasks| {
                tasks.app_host = HostPort::of_url("audit.example", false);
                tasks.app_port = Some(18080);
            },
        );
        let app = service!(fixture);
        let get = |host: &'static str| {
            actix_test::TestRequest::get()
                .uri("/tasks")
                .peer_addr(loopback())
                .insert_header(("Host", host))
        };
        // A bare entry admits the host on any port; one with a port only that port.
        for host in [
            "box.tailnet.example",
            "Box.Tailnet.example:443",
            "100.64.0.1:18080",
            "audit.example",
            "audit.example:8443",
            "localhost:18080",
        ] {
            assert_eq!(send!(app, get(host)).0, StatusCode::OK, "{host}");
        }
        for host in [
            "100.64.0.1",
            "100.64.0.1:8080",
            "evil.box.tailnet.example",
            "box.tailnet.example.evil.example",
        ] {
            assert_eq!(send!(app, get(host)).0, StatusCode::NOT_FOUND, "{host}");
        }
        // An action under an admitted name, sent from that same name.
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/tasks/basecamp/items/30")
                .peer_addr(loopback())
                .insert_header(("Host", "box.tailnet.example"))
                .to_request(),
        )
        .await;
        let cookie = response.response().cookies().next().unwrap().into_owned();
        let html = String::from_utf8(actix_test::read_body(response).await.to_vec()).unwrap();
        let token = html
            .split("name=\"csrf_token\" value=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap()
            .to_owned();
        let mark_done = |origin: &'static str| {
            bare_post(loopback(), "/tasks/basecamp/items/30/complete")
                .insert_header(("Host", "box.tailnet.example"))
                .insert_header(("Origin", origin))
        };
        let form = || Some(vec![("csrf_token", token.as_str())]);
        let before = fixture.calls().len();
        // Another admitted name is still another origin.
        for origin in [
            "https://audit.example",
            "http://localhost:8080",
            "https://box.tailnet.example:444",
        ] {
            let (status, _, _) = post!(mark_done(origin), app, Some(cookie.clone()), form());
            assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
        }
        assert!(only_checks(&fixture.calls()[before..]));
        let (status, location, _) = post!(
            mark_done("https://box.tailnet.example"),
            app,
            Some(cookie.clone()),
            form()
        );
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, "/tasks/basecamp/items/30");
    }

    #[actix_web::test]
    async fn actions_need_an_origin_that_is_the_request_host() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let before = fixture.calls().len();
        let post = |uri: &str, headers: &[(&'static str, &'static str)]| {
            let mut request = bare_post(loopback(), uri).insert_header(("Host", HOST));
            for header in headers {
                request = request.append_header(*header);
            }
            request
        };
        let refused: [&[(&str, &str)]; 12] = [
            &[],
            &[("Origin", "null")],
            &[("Origin", "http://evil.example")],
            &[("Origin", "http://evil.example:8080")],
            &[("Origin", "http://localhost")],
            &[("Origin", "http://localhost:9999")],
            &[("Origin", "http://127.0.0.1:8080")],
            &[("Origin", "ftp://localhost:8080")],
            &[("Origin", "localhost:8080")],
            &[("Origin", ORIGIN), ("Origin", ORIGIN)],
            // A Referer is read only when there is no Origin at all.
            &[
                ("Origin", "http://evil.example"),
                ("Referer", "http://localhost:8080/tasks"),
            ],
            &[("Referer", "http://evil.example/http://localhost:8080/")],
        ];
        for headers in refused {
            for uri in actions() {
                let (status, _, body) = post!(
                    post(&uri, headers),
                    app,
                    Some(cookie.clone()),
                    Some(full_form(&token))
                );
                assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?} {uri}");
                assert!(body.contains("dashboard's own page"), "{headers:?} {uri}");
            }
        }
        assert!(only_checks(&fixture.calls()[before..]));
        // The owner is still judged first: a stranger learns nothing from the 403.
        let (status, _, _) = send!(
            app,
            bare_post(stranger(), "/tasks/basecamp/items/30/complete")
                .insert_header(("Host", HOST))
        );
        assert_eq!(status, StatusCode::NOT_FOUND);
        for headers in [
            [("Origin", ORIGIN)],
            [(
                "Referer",
                "http://localhost:8080/tasks/basecamp/items/30?x=1",
            )],
        ] {
            let (status, location, _) = post!(
                post("/tasks/basecamp/items/30/complete", &headers),
                app,
                Some(cookie.clone()),
                Some(vec![("csrf_token", token.as_str())])
            );
            assert_eq!(status, StatusCode::SEE_OTHER, "{headers:?}");
            assert_eq!(location, "/tasks/basecamp/items/30");
        }
    }

    #[actix_web::test]
    async fn requiring_the_login_header_denies_plain_loopback() {
        let fixture = Fixture::with(true, r#"{"require_login_header": True}"#, |_| ());
        let app = service!(
            fixture,
            crate::owner::tests::check("me", Some("me@example.com"))
        );
        for uri in pages() {
            assert_eq!(
                get!(app, loopback(), &uri).0,
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
        for uri in actions() {
            let (status, _, _) = post!(app, loopback(), &uri, None, Some(full_form("x")));
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        for (login, expected) in [
            ("me@example.com", StatusCode::OK),
            ("ME@example.com", StatusCode::OK),
            ("eve@example.com", StatusCode::NOT_FOUND),
            ("", StatusCode::NOT_FOUND),
        ] {
            let request = page(loopback(), "/tasks").insert_header(("Tailscale-User-Login", login));
            assert_eq!(send!(app, request).0, expected, "{login:?}");
        }
        // A tailnet device is still judged by whois, not by a header it could set.
        assert_eq!(get!(app, tailnet(), "/tasks").0, StatusCode::OK);
        assert!(only_checks(&fixture.calls()));
        // Without the setting, plain loopback is the owner.
        let fixture = Fixture::with(true, r#"{"require_login_header": False}"#, |_| ());
        let app = service!(fixture);
        assert_eq!(get!(app, loopback(), "/tasks").0, StatusCode::OK);
    }

    #[actix_web::test]
    async fn the_gates_show_what_they_authorise() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (_, html) = get!(app, loopback(), &format!("/tasks/runs/{RUN}"));
        assert!(html.contains("These paths are what the build may change"));
        assert!(
            html.contains("src&#x2F;&lt;b&gt;lib&lt;&#x2F;b&gt;.rs"),
            "{html}"
        );
        assert!(html.contains("docs&#x2F;a&amp;b.md"));
        assert!(!html.contains("<b>lib</b>"));
        assert!(!html.contains("data-tm-review-evidence"));
        // The paths sit in the plan gate, before its approve button.
        let paths = html.find("data-tm-owned-paths").unwrap();
        assert!(html.find("data-tm-gate=\"plan\"").unwrap() < paths);
        assert!(paths < html.find("/approve/plan").unwrap());

        let (_, html) = get!(app, loopback(), &format!("/tasks/runs/{RUN_REVIEW}"));
        assert!(
            html.contains("EVIDENCE &lt;i&gt;3 checks&lt;&#x2F;i&gt; passed"),
            "{html}"
        );
        assert!(!html.contains("<i>3 checks</i>"));
        assert!(!html.contains("data-tm-owned-paths"));
        let evidence = html.find("data-tm-review-evidence").unwrap();
        assert!(html.find("data-tm-gate=\"review\"").unwrap() < evidence);
        assert!(evidence < html.find("/approve/review").unwrap());

        // A run that is not waiting shows neither.
        let (_, html) = get!(app, loopback(), &format!("/tasks/runs/{RUN_BUILDING}"));
        assert!(!html.contains("data-tm-owned-paths") && !html.contains("data-tm-review-evidence"));
    }

    #[actix_web::test]
    async fn a_delegate_that_times_out_or_is_already_running_says_so() {
        let fixture = Fixture::with(true, "{}", |tasks| {
            tasks.start_timeout = Duration::from_millis(300);
        });
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let form = || {
            Some(vec![
                ("csrf_token", token.as_str()),
                ("repo", "arc"),
                ("agent", "claude"),
            ])
        };
        let started = Instant::now();
        let (status, _, body) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/77/delegate",
            Some(cookie.clone()),
            form()
        );
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(body.contains("The outcome is unknown"), "{body}");
        assert!(body.contains("may or may not have gone through"), "{body}");
        assert!(body.contains("Check the item and the runs list"), "{body}");
        assert!(body.contains("before retrying"), "{body}");

        let (status, _, body) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/78/delegate",
            Some(cookie.clone()),
            form()
        );
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("already active"), "{body}");
        assert!(body.len() < 120 && !body.contains("<b>"), "{body}");
        // Any other refusal with exit 2 stays a 400.
        let failure = Failure {
            kind: FailureKind::Usage,
            code: "bad_state".to_owned(),
            message: "no".to_owned(),
        };
        assert_eq!(refused(&failure).status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn pages_render_with_the_feature_off() {
        let fixture = Fixture::new(false);
        let app = service!(fixture);
        for uri in pages() {
            let (status, html) = get!(app, loopback(), &uri);
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert!(html.contains("not configured"), "{uri}");
            assert!(html.contains("docs/task-managers.md"), "{uri}");
            assert!(html.contains("href=\"/public/task-managers.css\""), "{uri}");
        }
        // Nothing can be acted on while the feature is off.
        for uri in actions() {
            let (status, _, _) = post!(app, loopback(), &uri, None, Some(full_form("x")));
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        assert!(only_checks(&fixture.calls()));
        for uri in ["/public/task-managers.css", "/public/task-managers.js"] {
            let (status, body) = get!(app, stranger(), uri);
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert!(body.contains("tm-"), "{uri}");
        }
    }

    #[actix_web::test]
    async fn a_missing_cli_reads_as_not_configured() {
        let directory = std::env::temp_dir().join(format!("audit-tasks-{}", uuid::Uuid::new_v4()));
        let tasks = Arc::new(Tasks::new(directory.join("missing.py"), &directory, "me"));
        let fixture = Fixture::at(directory, tasks);
        let app = service!(fixture);
        let (status, html) = get!(app, loopback(), "/tasks");
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("not configured"));
        assert!(html.contains("could not be read"));
        // With no owner login known, only the machine owner on loopback gets even that.
        assert_eq!(get!(app, tailnet(), "/tasks").0, StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn everyone_but_the_owner_gets_404_on_every_route() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let before = fixture.calls().len();
        for uri in pages() {
            assert_eq!(
                get!(app, stranger(), &uri).0,
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
        for uri in actions() {
            // Even with the owner's own session and token, and with no form at all.
            let (status, _, _) = post!(
                app,
                stranger(),
                &uri,
                Some(cookie.clone()),
                Some(full_form(&token))
            );
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            let (status, _, _) = post!(app, stranger(), &uri, None, None);
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        // Through `tailscale serve`, the login it names must be the owner's.
        for uri in pages() {
            let response = actix_test::call_service(
                &app,
                page(loopback(), &uri)
                    .insert_header(("Tailscale-User-Login", "eve@example.com"))
                    .to_request(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
        let response = actix_test::call_service(
            &app,
            page(loopback(), "/tasks")
                .insert_header(("Tailscale-User-Login", "Me@Example.com"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(only_checks(&fixture.calls()[before..]));

        // On the tailnet, whois decides: someone else's device gets 404 everywhere.
        let other = Fixture::new(true);
        let app = service!(
            other,
            crate::owner::tests::check("me", Some("eve@example.com"))
        );
        for uri in pages() {
            assert_eq!(get!(app, tailnet(), &uri).0, StatusCode::NOT_FOUND, "{uri}");
        }
        for uri in actions() {
            let (status, _, _) = post!(app, tailnet(), &uri, None, Some(full_form("x")));
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        assert!(only_checks(&other.calls()));
        let mine = Fixture::new(true);
        let app = service!(
            mine,
            crate::owner::tests::check("me", Some("me@example.com"))
        );
        for uri in pages() {
            assert_eq!(get!(app, tailnet(), &uri).0, StatusCode::OK, "{uri}");
        }
    }

    #[actix_web::test]
    async fn bad_ids_never_reach_the_cli() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let before = fixture.calls().len();
        let long = format!("/tasks/basecamp/projects/{}", "9".repeat(21));
        for uri in [
            "/tasks/Basecamp",
            "/tasks/base-camp/projects/10",
            "/tasks/unlisted",
            "/tasks/unlisted/items/30",
            "/tasks/basecamp/projects/abc",
            long.as_str(),
            "/tasks/basecamp/lists/1%3Brm",
            "/tasks/basecamp/lists/--refresh",
            "/tasks/basecamp/items/-1",
            "/tasks/basecamp/items/..%2Fx",
            "/tasks/basecamp/items/30%20--json",
            "/tasks/runs/XYZ",
            "/tasks/runs/0123456789ABCDEF0123456789abcdef",
            "/tasks/runs/--id",
            "/tasks/runs/projects/10",
        ] {
            assert_eq!(get!(app, loopback(), uri).0, StatusCode::NOT_FOUND, "{uri}");
        }
        let form = || Some(full_form(&token));
        for uri in [
            "/tasks/basecamp/items/abc/delegate".to_owned(),
            "/tasks/Basecamp/items/30/comment".to_owned(),
            "/tasks/unlisted/items/30/complete".to_owned(),
            "/tasks/basecamp/items/--item/complete".to_owned(),
            "/tasks/runs/XYZ/cancel".to_owned(),
            "/tasks/runs/XYZ/approve/plan".to_owned(),
            format!("/tasks/runs/{RUN}/approve/merge"),
            format!("/tasks/runs/{RUN}/reject/--force"),
        ] {
            let (status, _, _) = post!(app, loopback(), &uri, Some(cookie.clone()), form());
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
        for (repo, agent) in [
            ("../etc", "claude"),
            ("--repo", "claude"),
            ("", "claude"),
            ("unlisted", "claude"),
            ("arc", "gpt"),
            ("arc", "--agent"),
            ("arc", ""),
        ] {
            let (status, _, _) = post!(
                app,
                loopback(),
                "/tasks/basecamp/items/30/delegate",
                Some(cookie.clone()),
                Some(vec![
                    ("csrf_token", token.as_str()),
                    ("repo", repo),
                    ("agent", agent)
                ])
            );
            assert_eq!(status, StatusCode::BAD_REQUEST, "{repo:?} {agent:?}");
        }
        let calls = fixture.calls();
        assert!(only_checks(&calls[before..]), "{:?}", &calls[before..]);
    }

    #[actix_web::test]
    async fn actions_without_the_session_token_are_refused() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, _token) = signed_in!(app);
        let before = fixture.calls().len();
        for uri in actions() {
            let (status, _, _) = post!(
                app,
                loopback(),
                &uri,
                Some(cookie.clone()),
                Some(full_form("forged"))
            );
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} forged");
            let (status, _, _) = post!(
                app,
                loopback(),
                &uri,
                Some(cookie.clone()),
                Some(full_form(""))
            );
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} empty");
            let (status, _, _) = post!(app, loopback(), &uri, Some(cookie.clone()), None);
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} no form");
            // A token from someone else's session is no better.
            let (status, _, _) = post!(app, loopback(), &uri, None, Some(full_form("forged")));
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} no session");
        }
        assert!(only_checks(&fixture.calls()[before..]));
    }

    #[actix_web::test]
    async fn delegating_starts_a_run_and_redirects_to_it() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let (status, location, _) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/30/delegate",
            Some(cookie.clone()),
            Some(vec![
                ("csrf_token", token.as_str()),
                ("repo", "arc"),
                ("agent", "codex"),
                ("plan_gate", "on"),
            ])
        );
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, format!("/tasks/runs/{RUN}"));
        assert_eq!(
            fixture.calls().last().unwrap(),
            &[
                "run",
                "start",
                "--provider",
                "basecamp",
                "--item",
                "30",
                "--repo",
                "arc",
                "--agent",
                "codex",
                "--json"
            ]
        );
        // Unticking the plan gate is passed on.
        let (status, location, _) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/30/delegate",
            Some(cookie.clone()),
            Some(vec![
                ("csrf_token", token.as_str()),
                ("repo", "harness-template"),
                ("agent", "claude"),
            ])
        );
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, format!("/tasks/runs/{RUN}"));
        assert_eq!(
            fixture.calls().last().unwrap(),
            &[
                "run",
                "start",
                "--provider",
                "basecamp",
                "--item",
                "30",
                "--repo",
                "harness-template",
                "--agent",
                "claude",
                "--no-plan-gate",
                "--json"
            ]
        );
        // The other actions answer 303 too, each back to its page.
        for (uri, back, call) in [
            (
                "/tasks/basecamp/items/30/complete".to_owned(),
                "/tasks/basecamp/items/30".to_owned(),
                vec![
                    "complete",
                    "--provider",
                    "basecamp",
                    "--item",
                    "30",
                    "--json",
                ],
            ),
            (
                format!("/tasks/runs/{RUN}/approve/plan"),
                format!("/tasks/runs/{RUN}"),
                vec!["run", "approve", "--id", RUN, "--gate", "plan", "--json"],
            ),
            (
                format!("/tasks/runs/{RUN_REVIEW}/approve/review"),
                format!("/tasks/runs/{RUN_REVIEW}"),
                vec![
                    "run", "approve", "--id", RUN_REVIEW, "--gate", "review", "--json",
                ],
            ),
            (
                format!("/tasks/runs/{RUN}/cancel"),
                format!("/tasks/runs/{RUN}"),
                vec!["run", "cancel", "--id", RUN, "--json"],
            ),
        ] {
            let (status, location, _) = post!(
                app,
                loopback(),
                &uri,
                Some(cookie.clone()),
                Some(vec![("csrf_token", token.as_str())])
            );
            assert_eq!(status, StatusCode::SEE_OTHER, "{uri}");
            assert_eq!(location, back, "{uri}");
            assert_eq!(fixture.calls().last().unwrap(), &call, "{uri}");
        }
    }

    #[actix_web::test]
    async fn form_text_travels_in_a_private_file_never_in_argv() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (cookie, token) = signed_in!(app);
        let body = "hello <b>world</b> --item 99\nsecond line; $(id)";
        let (status, location, _) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/30/comment",
            Some(cookie.clone()),
            Some(vec![("csrf_token", token.as_str()), ("text", body)])
        );
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, "/tasks/basecamp/items/30");
        let reason = "not this way: --gate review";
        let (status, location, _) = post!(
            app,
            loopback(),
            &format!("/tasks/runs/{RUN}/reject/plan"),
            Some(cookie.clone()),
            Some(vec![("csrf_token", token.as_str()), ("reason", reason)])
        );
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, format!("/tasks/runs/{RUN}"));

        let calls = fixture.calls();
        let comment = &calls[calls.len() - 2];
        assert_eq!(
            comment[..6],
            [
                "comment",
                "--provider",
                "basecamp",
                "--item",
                "30",
                "--text-file"
            ]
        );
        assert_eq!(comment.len(), 8);
        assert_eq!(comment[7], "--json");
        let reject = &calls[calls.len() - 1];
        assert_eq!(
            reject[..7],
            [
                "run",
                "reject",
                "--id",
                RUN,
                "--gate",
                "plan",
                "--reason-file"
            ]
        );
        assert_eq!((reject.len(), reject[8].as_str()), (9, "--json"));
        for call in &calls {
            for word in call {
                assert!(
                    !word.contains("hello") && !word.contains("second line"),
                    "{word}"
                );
                assert!(!word.contains("not this way"), "{word}");
            }
        }
        let texts = fixture.log("text.log");
        assert_eq!(texts.len(), 2);
        for (entry, path, expected) in [
            (&texts[0], &comment[6], body),
            (&texts[1], &reject[7], reason),
        ] {
            assert_eq!(entry["text"], expected);
            assert_eq!(entry["mode"], "0o600");
            assert_eq!(entry["path"], path.as_str());
            assert!(Path::new(path).is_absolute());
            assert!(
                !Path::new(path).exists(),
                "the file is removed after the call"
            );
        }

        // The limit is in characters, so a full-length comment of wide characters fits…
        let before = fixture.calls().len();
        let wide = "é".repeat(TEXT_LIMIT);
        let (status, _, _) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/30/comment",
            Some(cookie.clone()),
            Some(vec![
                ("csrf_token", token.as_str()),
                ("text", wide.as_str())
            ])
        );
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(fixture.log("text.log")[2]["text"], wide.as_str());
        // …and one more character, or nothing at all, never reaches the CLI.
        let before_refused = fixture.calls().len();
        assert_eq!(before_refused, before + 1);
        let long = "a".repeat(TEXT_LIMIT + 1);
        for (uri, field, value) in [
            (
                "/tasks/basecamp/items/30/comment".to_owned(),
                "text",
                long.as_str(),
            ),
            ("/tasks/basecamp/items/30/comment".to_owned(), "text", "   "),
            (
                format!("/tasks/runs/{RUN}/reject/plan"),
                "reason",
                long.as_str(),
            ),
        ] {
            let (status, _, _) = post!(
                app,
                loopback(),
                &uri,
                Some(cookie.clone()),
                Some(vec![("csrf_token", token.as_str()), (field, value)])
            );
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {}", value.len());
        }
        assert!(only_checks(&fixture.calls()[before_refused..]));
    }

    #[actix_web::test]
    async fn task_manager_text_is_escaped_on_every_page() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (status, html) = get!(app, loopback(), "/tasks/basecamp/items/30");
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;&#x2F;script&gt; &amp; co"),
            "{html}"
        );
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&lt;img src=x onerror=alert(3)&gt;"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("Ana &lt;i&gt;"));
        assert!(html.contains("Proj &lt;b&gt;bold&lt;&#x2F;b&gt;"));
        assert!(!html.contains("<b>bold</b>"));
        // A link out that is not http(s) is dropped, not escaped.
        assert!(!html.contains("javascript:"));
        // The forms: the list's mapped repository preselected, the plan gate ticked.
        assert!(html.contains("action=\"/tasks/basecamp/items/30/delegate\""));
        assert!(html.contains("<option value=\"arc\" selected>arc</option>"));
        assert!(html.contains("<option value=\"harness-template\">harness-template</option>"));
        assert!(!html.contains("../bad"));
        assert!(html.contains("name=\"plan_gate\" value=\"on\" checked"));
        assert!(html.contains("action=\"/tasks/basecamp/items/30/comment\""));
        assert!(html.contains("action=\"/tasks/basecamp/items/30/complete\" data-tm-confirm="));
        assert!(html.contains("href=\"/tasks/basecamp/items/31\" data-tm-nav=\"next\""));
        assert!(!html.contains("data-tm-nav=\"prev\""));
        assert!(html.contains("data-tm-up=\"/tasks/basecamp/lists/20\""));
        assert!(html.contains("Delegate and prepare PR"));
        // An item a run is already working on links to the run instead.
        let (status, html) = get!(app, loopback(), "/tasks/basecamp/items/31");
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&format!("href=\"/tasks/runs/{RUN}\"")));
        assert!(!html.contains("Delegate and prepare PR"));
        assert!(!html.contains("/items/31/delegate"));

        let (status, html) = get!(app, loopback(), "/tasks/basecamp/lists/20");
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("href=\"/tasks/basecamp/items/30\""));
        assert!(html.contains("&lt;script&gt;alert(1)"));
        assert!(!html.contains("<script>alert(1)"));
        let (_, html) = get!(app, loopback(), "/tasks/basecamp/lists/20?status=done");
        assert!(html.contains("No completed to-dos"));

        let (status, html) = get!(app, loopback(), "/tasks/basecamp/projects/10");
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Proj &lt;b&gt;bold"));
        assert!(html.contains("href=\"/tasks/basecamp/lists/20\""));
        assert!(html.contains("href=\"/tasks/basecamp/lists/21\""));

        let (status, html) = get!(app, loopback(), "/tasks/basecamp");
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("href=\"/tasks/basecamp/projects/10\""));
        assert!(html.contains("Proj &lt;b&gt;bold"));
        assert!(!html.contains("dropped-project"));

        let (status, html) = get!(app, loopback(), "/tasks");
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("href=\"/tasks/basecamp\"") && html.contains("href=\"/tasks/linear\"")
        );
        assert!(html.contains("1 of 2 connected"));
        assert!(!html.contains("Dropped"));
        let (status, html) = get!(app, loopback(), "/tasks/linear");
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Linear is not connected"));
        // A missing item is a 404 from the CLI's exit code.
        assert_eq!(
            get!(app, loopback(), "/tasks/basecamp/items/404").0,
            StatusCode::NOT_FOUND
        );
    }

    #[actix_web::test]
    async fn run_pages_offer_approval_only_while_a_run_waits() {
        let fixture = Fixture::new(true);
        let app = service!(fixture);
        let (status, html) = get!(app, loopback(), "/tasks/runs");
        assert_eq!(status, StatusCode::OK);
        for run in [RUN, RUN_BUILDING, RUN_REVIEW] {
            assert!(
                html.contains(&format!("href=\"/tasks/runs/{run}\"")),
                "{run}"
            );
        }
        assert!(html.contains("2 runs are waiting for you"));
        assert!(html.contains("Nothing merges on its own"));

        let (status, html) = get!(app, loopback(), &format!("/tasks/runs/{RUN}"));
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&format!("action=\"/tasks/runs/{RUN}/approve/plan\"")));
        assert!(html.contains(&format!("action=\"/tasks/runs/{RUN}/reject/plan\"")));
        assert!(!html.contains("/approve/review"));
        assert!(html.contains(&format!("action=\"/tasks/runs/{RUN}/cancel\"")));
        assert!(html.contains("PLAN &lt;b&gt;deliverables&lt;&#x2F;b&gt;"));
        assert!(html.contains("created &lt;b&gt;"));
        assert!(html.contains("Nothing merges on its own"));
        assert!(html.contains("<li class=\"waiting\">"));
        assert!(
            !html.contains("data-tm-refresh"),
            "a waiting run never reloads"
        );
        assert!(html.contains("href=\"/tasks/basecamp/items/31\""));

        let (status, html) = get!(app, loopback(), &format!("/tasks/runs/{RUN_BUILDING}"));
        assert_eq!(status, StatusCode::OK);
        assert!(!html.contains("/approve/") && !html.contains("/reject/"));
        assert!(html.contains("data-tm-refresh=\"5\""));
        // Tera escapes the slashes of a value; the browser reads the same address.
        assert!(html.contains(
            "href=\"https:&#x2F;&#x2F;github.com&#x2F;example&#x2F;arc&#x2F;pull&#x2F;7\""
        ));
        assert!(html.contains("<li class=\"active\">"));

        let (status, html) = get!(app, loopback(), &format!("/tasks/runs/{RUN_REVIEW}"));
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&format!(
            "action=\"/tasks/runs/{RUN_REVIEW}/approve/review\""
        )));
        assert!(html.contains(&format!(
            "action=\"/tasks/runs/{RUN_REVIEW}/reject/review\""
        )));
        assert!(!html.contains("/approve/plan"));

        let unknown = format!("/tasks/runs/{}", "c".repeat(32));
        assert_eq!(get!(app, loopback(), &unknown).0, StatusCode::NOT_FOUND);
    }

    /// Create, edit, associations, open items and agents.
    mod manage;
}
