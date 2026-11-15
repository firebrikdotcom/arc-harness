//! Attachable interactive agents on task-manager items.
//!
//! An agent starts as one headless, read-only turn (`claude --permission-mode
//! plan`, `codex exec --sandbox read-only`) in its own session (setsid), in a
//! supplied directory or a fresh git worktree beside the repository. The owner
//! then attaches to the session (edits happen only after that), hands it back,
//! stops it, or finishes it with `done` (commit, push, draft pull request).
//! Item text reaches the agent only inside the fenced packet, a 0600 file.

pub mod attach;
pub mod done;
pub mod launch;
pub mod supervise;
pub mod worktree;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::store::AgentRecord;
use super::{TaskError, TaskManagers, PROVIDER};
use crate::domain::task_agent::commands::AgentChange;
use crate::domain::task_agent::events::{AgentKind, AgentOwner, AgentState};

pub use attach::attach;
pub use done::done;
pub use launch::{start, StartRequest};
pub use supervise::supervise_once;

/// Environment a launched agent must not inherit from a parent agent session.
const SCRUBBED_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CLAUDE_ENV_FILE",
    "CLAUDE_PROJECT_DIR",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
];
const STDOUT_LIMIT: u64 = 4 << 20;
const STDERR_LIMIT: u64 = 64 << 10;
const CLI_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Where the runtime finds its tools and keeps its files. Production values
/// come from [`Runtime::from_env`]; tests build one directly.
#[derive(Debug, Clone)]
pub struct Runtime {
    /// argv prefix of the task-manager CLI, e.g. `python3 task_managers.py --db-root DIR`.
    pub tasks_cli: Vec<OsString>,
    /// Per-agent packets and turn logs: `<state_dir>/<item>/<code>/`.
    pub state_dir: PathBuf,
    pub claude: OsString,
    pub codex: OsString,
    pub gh: OsString,
    pub git: OsString,
    /// SSH destination shown in a second resume command.
    pub ssh: Option<String>,
    /// How long attach waits for the headless turn to end.
    pub takeover_wait: Duration,
    /// How long start waits for Codex to report its thread id.
    pub session_wait: Duration,
}

fn env_path(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

impl Runtime {
    /// Environment overrides: `HARNESS_TASKS_PYTHON`, `HARNESS_TASKS_SCRIPT`,
    /// `HARNESS_DB_ROOT`, `HARNESS_TASKS_CLAUDE`, `HARNESS_TASKS_CODEX`,
    /// `HARNESS_TASKS_GH`, `HARNESS_TASKS_GIT`, `HARNESS_TASKS_SSH`,
    /// `HARNESS_TASKS_TAKEOVER_WAIT` (seconds, 5..120, default 30).
    pub fn from_env() -> Self {
        let checkout = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let script = env_path("HARNESS_TASKS_SCRIPT")
            .unwrap_or_else(|| checkout.join("scripts/task_managers.py").into());
        let db_root = env_path("HARNESS_DB_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| checkout.join(".harness-db"));
        let wait = std::env::var("HARNESS_TASKS_TAKEOVER_WAIT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(5, 120);
        Self {
            tasks_cli: vec![
                env_path("HARNESS_TASKS_PYTHON").unwrap_or_else(|| "python3".into()),
                script,
                "--db-root".into(),
                db_root.clone().into(),
            ],
            state_dir: db_root.join("task-managers").join("agents"),
            claude: env_path("HARNESS_TASKS_CLAUDE").unwrap_or_else(|| "claude".into()),
            codex: env_path("HARNESS_TASKS_CODEX").unwrap_or_else(|| "codex".into()),
            gh: env_path("HARNESS_TASKS_GH").unwrap_or_else(|| "gh".into()),
            git: env_path("HARNESS_TASKS_GIT").unwrap_or_else(|| "git".into()),
            ssh: std::env::var("HARNESS_TASKS_SSH")
                .ok()
                .filter(|dest| valid_ssh(dest)),
            takeover_wait: Duration::from_secs(wait),
            session_wait: Duration::from_secs(20),
        }
    }

    /// Fill the SSH destination from task-managers.json when the environment
    /// set none (best effort), so every command shows the same resume command.
    /// One `check` call: for a command's start, not for the supervisor's loop.
    pub fn resolve_ssh(&mut self) {
        if self.ssh.is_none() {
            self.ssh = self.config().ok().and_then(|config| config.ssh);
        }
    }

    /// Run the task-manager CLI with `--json`; exits map 2 usage, 3 not
    /// connected (usage), 4 not found, other failures internal.
    pub fn tasks(&self, args: &[&str]) -> Result<Value, TaskError> {
        let mut command = Command::new(&self.tasks_cli[0]);
        command.args(&self.tasks_cli[1..]).args(args).arg("--json");
        // The argv names the database; an inherited value must not disagree with it.
        command.env_remove("HARNESS_DB_ROOT");
        let output = run(command, CLI_TIMEOUT)?;
        let document = serde_json::from_str::<Value>(output.stdout.trim()).ok();
        if output.status == Some(0) {
            return document.ok_or_else(|| {
                TaskError::Internal("the task manager CLI printed invalid JSON".into())
            });
        }
        let message = document
            .as_ref()
            .and_then(|doc| doc["error"]["message"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("the task manager CLI failed: {}", output.stderr.trim()));
        Err(match output.status {
            Some(2) => TaskError::Usage(message),
            Some(3) => TaskError::Usage(format!("not connected: {message}")),
            Some(4) => TaskError::NotFound(message),
            _ => TaskError::Internal(message),
        })
    }

    /// `check`: allowed repos, their paths, list mappings, and an SSH destination.
    pub fn config(&self) -> Result<Config, TaskError> {
        let check = self.tasks(&["check"])?;
        if check["enabled"] == false {
            return Err(TaskError::Usage("task managers are off".into()));
        }
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let map = |value: &Value| -> BTreeMap<String, String> {
            value
                .as_object()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(Config {
            repos: strings(&check["repos"]),
            repo_paths: map(&check["repo_paths"])
                .into_iter()
                .map(|(name, path)| (name, PathBuf::from(path)))
                .filter(|(_, path)| path.is_absolute())
                .collect(),
            mappings: map(&check["mappings"]),
            ssh: self.ssh.clone().or_else(|| {
                check["ssh"]
                    .as_str()
                    .filter(|dest| valid_ssh(dest))
                    .map(str::to_string)
            }),
        })
    }
}

/// What `check` reports that agents need.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub repos: Vec<String>,
    pub repo_paths: BTreeMap<String, PathBuf>,
    /// `basecamp:<list id>` -> repo.
    pub mappings: BTreeMap<String, String>,
    pub ssh: Option<String>,
}

impl Config {
    /// The configured default repo of a list, when it is an allowed repo.
    pub fn default_repo(&self, list_id: &str) -> Option<String> {
        self.mappings
            .get(&format!("{PROVIDER}:{list_id}"))
            .filter(|repo| self.repos.contains(repo))
            .cloned()
    }
}

/// `user@host` or `host`: letters, digits and `@._-` only.
pub fn valid_ssh(dest: &str) -> bool {
    (1..=200).contains(&dest.len())
        && !dest.starts_with('-')
        && dest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'-'))
}

/// Shell-quote one word for a POSIX shell.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The commands that continue an agent's session interactively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeCommands {
    /// Run on this machine.
    pub local: String,
    /// The same through `ssh -t DEST`, when an SSH destination is configured.
    pub ssh: Option<String>,
}

pub fn resume_commands(record: &AgentRecord, ssh: Option<&str>) -> ResumeCommands {
    let local = match record.agent_kind {
        AgentKind::Claude => format!(
            "cd {} && claude --resume {}",
            quote(&record.worktree),
            quote(&record.session_id)
        ),
        AgentKind::Codex => format!(
            "codex resume {} --cd {}",
            quote(&record.session_id),
            quote(&record.worktree)
        ),
    };
    let ssh = ssh.map(|dest| format!("ssh -t {} {}", quote(dest), quote(&local)));
    ResumeCommands { local, ssh }
}

/// An agent as callers show it.
#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    /// `basecamp:<item id>`.
    pub agent_id: String,
    #[serde(flatten)]
    pub record: AgentRecord,
    /// Whether the recorded headless process (its process group) still runs.
    pub process_alive: bool,
    pub resume: ResumeCommands,
}

pub fn view(record: AgentRecord, ssh: Option<&str>) -> AgentView {
    AgentView {
        agent_id: format!("{PROVIDER}:{}", record.item_id),
        process_alive: record.state.is_live() && process::alive(record.pid),
        resume: resume_commands(&record, ssh),
        record,
    }
}

/// The agent directory: `<state_dir>/<item>/<code>/`.
pub fn agent_dir(rt: &Runtime, item_id: &str, code: &str) -> PathBuf {
    rt.state_dir.join(item_id).join(code)
}

/// Every agent generation, live first.
pub fn list(tm: &TaskManagers, rt: &Runtime) -> Result<Vec<AgentView>, TaskError> {
    tm.refresh()?;
    let ssh = rt.ssh.as_deref();
    Ok(tm
        .store()
        .agents()
        .map_err(|e| TaskError::Internal(e.to_string()))?
        .into_iter()
        .map(|record| view(record, ssh))
        .collect())
}

/// The agent holding `code` (live first, else the latest that held it).
pub fn status(tm: &TaskManagers, rt: &Runtime, code: &str) -> Result<AgentView, TaskError> {
    if !super::valid_code(code) {
        return Err(TaskError::Usage(
            "codes are tm- and 6 lowercase hex digits".into(),
        ));
    }
    tm.refresh()?;
    tm.store()
        .agent_by_code(code)
        .map_err(|e| TaskError::Internal(e.to_string()))?
        .map(|record| view(record, rt.ssh.as_deref()))
        .ok_or_else(|| TaskError::NotFound(format!("no agent with code {code}")))
}

/// Return an attached agent to the service. It then waits; the service does
/// not resume it on its own.
pub async fn handback(tm: &TaskManagers, rt: &Runtime, code: &str) -> Result<AgentView, TaskError> {
    let record = tm.change_agent(code, AgentChange::HandBack).await?;
    Ok(view(record, rt.ssh.as_deref()))
}

/// Stop the agent. A service-owned headless process group is terminated; an
/// agent you hold is only marked stopped, never signalled. The worktree stays.
pub async fn stop(tm: &TaskManagers, rt: &Runtime, code: &str) -> Result<AgentView, TaskError> {
    tm.refresh()?;
    let record = tm.live_by_code(code)?;
    if record.owner == Some(AgentOwner::Service) && record.state != AgentState::Waiting {
        process::terminate_group(record.pid, Duration::from_secs(5));
    }
    let record = tm.change_agent(code, AgentChange::Stop).await?;
    Ok(view(record, rt.ssh.as_deref()))
}

/// Captured output of a bounded subprocess.
pub(crate) struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

fn drain(pipe: Option<impl Read + Send + 'static>, limit: u64) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(limit).read_to_end(&mut out);
        }
        out
    })
}

/// Run a command with no stdin, bounded output and a deadline.
pub(crate) fn run(mut command: Command, timeout: Duration) -> Result<Output, TaskError> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| TaskError::Internal(format!("could not start {program}: {error}")))?;
    let stdout = drain(child.stdout.take(), STDOUT_LIMIT);
    let stderr = drain(child.stderr.take(), STDERR_LIMIT);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    let Some(status) = status else {
        return Err(TaskError::Internal(format!(
            "{program} did not finish within {}s",
            timeout.as_secs()
        )));
    };
    Ok(Output {
        status: status.code(),
        stdout,
        stderr,
    })
}

/// Run and require exit 0; the error carries the tail of stderr.
pub(crate) fn run_ok(command: Command, what: &str) -> Result<String, TaskError> {
    let output = run(command, GIT_TIMEOUT)?;
    if output.status == Some(0) {
        return Ok(output.stdout);
    }
    let tail: String = output
        .stderr
        .trim()
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Err(TaskError::Internal(format!("{what} failed: {tail}")))
}

/// Process-group handling without a libc dependency (the symbols are in the
/// C library std already links).
pub mod process {
    use std::time::{Duration, Instant};

    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
        fn getpgid(pid: i32) -> i32;
        fn setsid() -> i32;
    }
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    const EPERM: i32 = 1;

    fn is_zombie(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')')
                    .map(|(_, rest)| rest.trim_start().starts_with('Z'))
            })
            .unwrap_or(false)
    }

    /// The recorded headless process still runs as its own process-group
    /// leader (agents start with setsid). A reused pid that leads no group
    /// of its own is treated as gone.
    pub fn alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 1 {
            return false;
        }
        // SAFETY: kill with signal 0 only checks for existence; getpgid reads.
        let exists = unsafe { kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(EPERM);
        exists && unsafe { getpgid(pid) } == pid && !is_zombie(pid as u32)
    }

    /// SIGTERM the process group, then SIGKILL it after `grace`.
    pub fn terminate_group(pid: u32, grace: Duration) {
        if !alive(pid) {
            return;
        }
        let group = -(pid as i32);
        // SAFETY: signals only the agent's own process group (pgid == pid).
        unsafe { kill(group, SIGTERM) };
        let deadline = Instant::now() + grace;
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if alive(pid) {
            unsafe { kill(group, SIGKILL) };
        }
    }

    /// Make the child a session leader so a service restart does not take it down.
    pub fn detach(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no parent state.
        unsafe {
            command.pre_exec(|| {
                if setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
}

pub(crate) fn scrub(command: &mut Command) {
    for name in SCRUBBED_ENV {
        command.env_remove(name);
    }
}

pub(crate) fn path_str(path: &Path) -> Result<String, TaskError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| TaskError::Usage("paths must be valid UTF-8".into()))
}
