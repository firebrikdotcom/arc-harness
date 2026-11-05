//! Issue agents: one read-only Analyze job per root cause (the innermost
//! in-app frame of an issue on one card). Jobs, diagnoses, locks, quota and
//! worktrees all live under `<db>/targets/<id>/signals/`; nothing is written in
//! the project except its git worktree metadata. See docs/issue-agents.md.

pub mod actions;
pub mod agent;
pub mod cli;
pub mod difit;
pub mod herdr;
pub mod observe;
pub mod packet;
pub mod process;
pub mod runner;
pub mod settings;
pub mod worktree;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;

use crate::model::scrub;
use crate::store::{read_json, write_private, Target};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EVENT_LIMIT: usize = 300;
const EVENTS_KEPT: usize = 60;
const QUOTA_DAYS_KEPT: usize = 7;

/// `sha256(target \n card \n path \n line)`: card-specific, stable across issue ids and messages.
pub fn fingerprint(target: &str, card: &str, path: &str, line: u64) -> String {
    hex::encode(Sha256::digest(
        format!("{target}\n{card}\n{path}\n{line}").as_bytes(),
    ))
}

pub fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Diagnosed,
    Stopped,
    Failed,
    /// The runner lost control of the agent: it could not verify or stop the
    /// process, so the agent may still be running. Nothing signals it again.
    Detached,
    /// Observation saw the agent end (gone, or idle after its last reply)
    /// without a diagnosis; the owner is kept.
    Ended,
}

impl Status {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Diagnosed | Self::Stopped | Self::Failed | Self::Detached | Self::Ended
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Runner,
    You,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Launcher {
    Herdr,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootFrame {
    pub path: String,
    pub line: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRef {
    pub path: String,
    pub branch: String,
    pub base_branch: String,
    /// The commit the branch was created at; reuse requires the branch to still
    /// point there, or to commits made only on it since.
    #[serde(default)]
    pub base_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrTab {
    pub tab_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Difit {
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub at: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub summary: String,
}

/// Job schema v1. No prompts, stack payloads, secrets or transcripts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub version: u32,
    pub fingerprint: String,
    pub target_id: String,
    pub card_id: String,
    pub issue_key: String,
    pub issue_title: String,
    pub issue_url: Option<String>,
    pub root_frame: RootFrame,
    pub mode: String,
    pub status: Status,
    pub owner: Owner,
    pub created_at: String,
    pub updated_at: String,
    pub agent: AgentKind,
    pub session_id: Option<String>,
    pub worktree: WorktreeRef,
    pub machine: String,
    pub launcher: Launcher,
    pub herdr_tab: Option<HerdrTab>,
    pub pid: Option<u32>,
    /// The session transcript's last write, else the supervisor's heartbeat.
    pub last_activity_at: Option<String>,
    /// When a runner tick last looked at the job.
    #[serde(default)]
    pub observed_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub diagnosis_path: String,
    pub difit: Difit,
    pub error: Option<String>,
    /// Set while a Stop interrupts a Herdr agent: the supervisor lets go and the
    /// stop decides the final status once the agent is idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopping: Option<String>,
    /// A fresh token each time the runner starts the job. A supervisor writes
    /// only while the job still carries the token it was started with, so one
    /// left over from an earlier run cannot touch a newer run of the same job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub events: Vec<Event>,
}

/// One line, scrubbed of identifiers, at most `limit` characters.
pub fn bounded(text: &str, limit: usize) -> String {
    let flat: String = scrub(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= limit {
        return flat.to_owned();
    }
    let mut cut: String = flat.chars().take(limit.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

impl Job {
    pub fn event(&mut self, kind: &str, summary: &str) {
        let at = now();
        self.events.push(Event {
            at: at.clone(),
            kind: kind.to_owned(),
            summary: bounded(summary, EVENT_LIMIT),
        });
        if self.events.len() > EVENTS_KEPT {
            let extra = self.events.len() - EVENTS_KEPT;
            self.events.drain(..extra);
        }
        self.updated_at = at;
    }

    pub fn fail(&mut self, error: &str) {
        let error = bounded(error, EVENT_LIMIT);
        self.status = Status::Failed;
        self.finished_at = Some(now());
        self.pid = None;
        self.error = Some(error.clone());
        self.event("failed", &error);
    }

    /// The runner can no longer verify or stop the agent; say so and let go.
    pub fn detach(&mut self, why: &str) {
        let why = bounded(why, EVENT_LIMIT);
        self.status = Status::Detached;
        self.finished_at = Some(now());
        self.pid = None;
        self.stopping = None;
        self.error = Some(why.clone());
        self.event("detached", &why);
    }

    /// Observation saw the agent end without a diagnosis.
    pub fn end(&mut self, how: &str) {
        self.status = Status::Ended;
        self.finished_at = Some(now());
        self.pid = None;
        self.event("ended", how);
    }

    pub fn fp8(&self) -> &str {
        &self.fingerprint[..8]
    }

    pub fn herdr_name(&self) -> String {
        format!("issue-{}", self.fp8())
    }

    /// The structural rules the schema alone cannot express.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.version == 1,
            "unsupported job version {}",
            self.version
        );
        anyhow::ensure!(valid_fingerprint(&self.fingerprint), "invalid fingerprint");
        anyhow::ensure!(self.mode == "analyze", "unsupported mode {:?}", self.mode);
        anyhow::ensure!(crate::config::slug(&self.target_id), "invalid target id");
        anyhow::ensure!(self.events.len() <= EVENTS_KEPT, "too many events");
        if let Some(session) = &self.session_id {
            anyhow::ensure!(settings::safe_token(session), "invalid session id");
        }
        Ok(())
    }
}

/// Typed failures; the CLI maps each to an exit code and the dashboard to an HTTP status.
#[derive(Debug)]
pub enum JobError {
    NotFound(String),
    Conflict(String),
    Invalid(String),
    Quota(String),
    Failed(String),
}

impl JobError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Invalid(_) => "invalid",
            Self::Quota(_) => "quota",
            Self::Failed(_) => "failed",
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound(_) => 3,
            Self::Conflict(_) => 4,
            Self::Invalid(_) => 5,
            Self::Quota(_) => 6,
            Self::Failed(_) => 1,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m)
            | Self::Conflict(m)
            | Self::Invalid(m)
            | Self::Quota(m)
            | Self::Failed(m) => m,
        }
    }
}

impl From<anyhow::Error> for JobError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(format!("{error:#}"))
    }
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

pub type JobResult<T> = std::result::Result<T, JobError>;

// ---------- storage ----------

pub fn jobs_dir(target: &Target) -> PathBuf {
    target.signals().join("jobs")
}

pub fn analyses_dir(target: &Target) -> PathBuf {
    target.signals().join("analyses")
}

pub fn worktrees_dir(target: &Target) -> PathBuf {
    target.signals().join("worktrees")
}

pub fn job_path(target: &Target, fp: &str) -> PathBuf {
    jobs_dir(target).join(format!("{fp}.json"))
}

pub fn diagnosis_path(target: &Target, fp: &str) -> PathBuf {
    analyses_dir(target).join(format!("{fp}.md"))
}

/// A stored job, strictly parsed: a malformed file is an error, never a default.
pub fn load(target: &Target, fp: &str) -> Result<Option<Job>> {
    if !valid_fingerprint(fp) {
        return Ok(None);
    }
    let path = job_path(target, fp);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let job: Job =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    job.validate()
        .with_context(|| format!("validating {}", path.display()))?;
    anyhow::ensure!(
        job.fingerprint == fp && job.target_id == target.id,
        "{} does not match its name",
        path.display()
    );
    Ok(Some(job))
}

pub fn save(target: &Target, job: &Job) -> Result<()> {
    job.validate()?;
    write_private(
        &job_path(target, &job.fingerprint),
        &serde_json::to_vec_pretty(job)?,
    )
}

/// Every stored job of a target; unreadable files are skipped and logged.
pub fn list(target: &Target) -> Vec<Job> {
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(jobs_dir(target))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(fp) = name.strip_suffix(".json") else {
            continue;
        };
        if !valid_fingerprint(fp) {
            continue;
        }
        match load(target, fp) {
            Ok(Some(job)) => jobs.push(job),
            Ok(None) => {}
            Err(error) => crate::poll::log(&format!("{}: skipping job {fp}: {error:#}", target.id)),
        }
    }
    jobs.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.fingerprint.cmp(&b.fingerprint))
    });
    jobs
}

/// Re-read a job under the target's state lock, change it, and write it back atomically.
pub fn mutate<T>(
    target: &Target,
    fp: &str,
    change: impl FnOnce(&mut Job) -> JobResult<T>,
) -> JobResult<(Job, T)> {
    rewrite(target, fp, true, change)
}

/// Like `mutate`, but `updated_at` moves only when the change records an event,
/// so a tick that merely looked at a job does not make it look updated.
pub fn mutate_observed<T>(
    target: &Target,
    fp: &str,
    change: impl FnOnce(&mut Job) -> JobResult<T>,
) -> JobResult<(Job, T)> {
    rewrite(target, fp, false, change)
}

fn rewrite<T>(
    target: &Target,
    fp: &str,
    touch: bool,
    change: impl FnOnce(&mut Job) -> JobResult<T>,
) -> JobResult<(Job, T)> {
    let _lock = state_lock(target)?;
    let mut job = load(target, fp)?
        .ok_or_else(|| JobError::NotFound(format!("no job {fp} for {}", target.id)))?;
    let value = change(&mut job)?;
    if touch {
        job.updated_at = now();
    }
    save(target, &job)?;
    Ok((job, value))
}

// ---------- locks ----------

#[derive(Debug, Serialize, Deserialize)]
struct LockOwner {
    pid: u32,
    started_at: String,
    token: String,
}

/// A lock file made with `create_new`. It is reclaimed only when its owner process is gone.
pub struct FileLock {
    path: PathBuf,
    token: String,
}

impl FileLock {
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let dir = path.parent().context("lock path has no parent")?;
        crate::store::private_dir(dir)?;
        for _ in 0..2 {
            let token = settings::random_hex(8);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let owner = LockOwner {
                        pid: std::process::id(),
                        started_at: now(),
                        token: token.clone(),
                    };
                    file.write_all(&serde_json::to_vec(&owner)?)?;
                    return Ok(Some(Self {
                        path: path.to_owned(),
                        token,
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !Self::abandoned(path) {
                        return Ok(None);
                    }
                    crate::poll::log(&format!(
                        "reclaiming {} from a process that is gone",
                        path.display()
                    ));
                    let _ = std::fs::remove_file(path);
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("creating {}", path.display()))
                }
            }
        }
        Ok(None)
    }

    /// Wait up to `timeout` for the lock.
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lock) = Self::try_acquire(path)? {
                return Ok(lock);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "{} is held by another process",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// The owner's process is demonstrably gone (or an empty file was left long ago).
    fn abandoned(path: &Path) -> bool {
        match read_json::<LockOwner>(path) {
            Some(owner) => !process::alive(owner.pid),
            None => std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(30)),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if read_json::<LockOwner>(&self.path).is_some_and(|owner| owner.token == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Held briefly around every read-modify-write of a target's job files.
pub fn state_lock(target: &Target) -> Result<FileLock> {
    FileLock::acquire(
        &jobs_dir(target).join("state.lock"),
        Duration::from_secs(10),
    )
}

// ---------- quota ----------

fn quota_path(target: &Target) -> PathBuf {
    jobs_dir(target).join("quota.json")
}

/// Jobs started for this target today (UTC).
pub fn starts_today(target: &Target) -> u64 {
    read_json::<BTreeMap<String, u64>>(&quota_path(target))
        .and_then(|days| days.get(&today()).copied())
        .unwrap_or(0)
}

/// Count one start for today; call with the state lock held.
pub fn count_start(target: &Target) -> Result<u64> {
    let path = quota_path(target);
    let mut days: BTreeMap<String, u64> = read_json(&path).unwrap_or_default();
    let day = today();
    let count = days.get(&day).copied().unwrap_or(0) + 1;
    days.insert(day, count);
    while days.len() > QUOTA_DAYS_KEPT {
        let oldest = days.keys().next().cloned().unwrap_or_default();
        days.remove(&oldest);
    }
    write_private(&path, &serde_json::to_vec_pretty(&days)?)?;
    Ok(count)
}

// ---------- process records ----------

/// A process this job started and its identity as the OS reported it right
/// after the start. Nothing signals the pid unless all of it still matches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    /// The process group; the runner only signals groups it leads (pgid == pid).
    #[serde(default)]
    pub pgid: u32,
    /// The OS start time (macOS: seconds.microseconds; Linux: clock ticks since boot).
    #[serde(default)]
    pub os_start: String,
    /// The exact executable path the OS reports for the process.
    pub exe: String,
    pub started_at: String,
}

/// `<jobs>/<fp>.pid`: the supervisor, agent and difit processes of one job.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Procs {
    pub supervisor: Option<Proc>,
    pub agent: Option<Proc>,
    pub difit: Option<Proc>,
}

pub fn procs_path(target: &Target, fp: &str) -> PathBuf {
    jobs_dir(target).join(format!("{fp}.pid"))
}

pub fn read_procs(target: &Target, fp: &str) -> Procs {
    read_json(&procs_path(target, fp)).unwrap_or_default()
}

/// Change the process record; call with the state lock held.
pub fn write_procs(target: &Target, fp: &str, change: impl FnOnce(&mut Procs)) -> Result<()> {
    let mut procs = read_procs(target, fp);
    change(&mut procs);
    write_private(&procs_path(target, fp), &serde_json::to_vec_pretty(&procs)?)
}

pub fn log_path(target: &Target, fp: &str) -> PathBuf {
    jobs_dir(target).join(format!("{fp}.log"))
}

/// The recorded process is alive and verifiably the one that was started.
pub fn proc_alive(proc: Option<&Proc>) -> bool {
    proc.is_some_and(|p| process::check(p) == process::Check::Same)
}

/// The recorded process may still be running: it is verifiably ours, or its
/// pid is alive but its identity cannot be confirmed either way.
pub fn proc_maybe_alive(proc: Option<&Proc>) -> bool {
    proc.is_some_and(|p| process::check(p) != process::Check::Gone)
}

/// Append one line to a job's log (never prompts or transcripts).
pub fn log_line(target: &Target, fp: &str, line: &str) {
    use std::io::Write;
    let path = log_path(target, fp);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(path) {
        let _ = writeln!(file, "{} {}", now(), bounded(line, 600));
    }
}
