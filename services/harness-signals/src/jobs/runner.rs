//! The runner. `run_once` scans tracked targets under one global lock: it
//! recovers jobs whose supervisor is gone, then starts the oldest queued
//! runner-owned job of each project that has none running, within the global
//! cap and each project's daily cap. Each started job gets its own detached
//! supervisor process (`jobs supervise`), which creates the worktree, launches
//! the agent, heartbeats, enforces the timeout and records the outcome.

use super::agent::{self, background_args, interactive_args};
use super::herdr::{finish, Herdr};
use super::observe;
use super::settings::{uuid_v4, Settings};
use super::{
    count_start, diagnosis_path, difit, jobs_dir, list, load, log_line, log_path, mutate, now,
    packet, process, read_procs, starts_today, state_lock, worktree, write_procs, AgentKind,
    FileLock, HerdrTab, Job, Launcher, Owner, Proc, Status,
};
use crate::config::{Config, Project};
use crate::store::{resolve, write_private, Target};
use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 8 << 20;
const TRANSCRIPT_WAIT: Duration = Duration::from_secs(10);
/// Environment a launched agent must not inherit from whatever started the service.
const SCRUBBED_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CLAUDE_ENV_FILE",
    "CLAUDE_PROJECT_DIR",
    "CODEX_THREAD_ID",
    "HARNESS_SESSION_ID",
    "HARNESS_RUN_ID",
];

pub struct Context<'a> {
    pub db_root: &'a Path,
    pub config: Option<&'a Config>,
    pub settings: &'a Settings,
}

/// Starts the supervisor of one job and reports the process to record.
pub type Spawner<'a> = &'a dyn Fn(&Target, &str) -> Result<Proc>;

/// Configured projects whose targets resolve now.
pub fn tracked(ctx: &Context) -> Vec<(Target, Option<Project>)> {
    let mut out: Vec<(Target, Option<Project>)> = Vec::new();
    for project in ctx.config.iter().flat_map(|c| c.projects.iter()) {
        if let Ok(target) = resolve(ctx.db_root, &project.target) {
            if !out.iter().any(|(t, _)| t.id == target.id) {
                out.push((target, Some(project.clone())));
            }
        }
    }
    out
}

pub fn project_for(ctx: &Context, target: &Target) -> Option<Project> {
    tracked(ctx)
        .into_iter()
        .find(|(t, _)| t.id == target.id)
        .and_then(|(_, p)| p)
}

fn ours(job: &Job) -> bool {
    job.status == Status::Running && job.owner == Owner::Runner && job.stopping.is_none()
}

/// Whether the job is still the run a supervisor was started for.
fn same_run(job: &Job, run: Option<&str>) -> bool {
    job.run.as_deref() == run
}

/// `ours`, for the supervisor of this run only.
fn ours_run(job: &Job, run: Option<&str>) -> bool {
    ours(job) && same_run(job, run)
}

/// A Stop that started interrupting a Herdr agent and never finished (its
/// process died) is given up after this long.
const STALE_STOP: Duration = Duration::from_secs(300);

/// One scan. Returns what it did, for the CLI and the service log.
pub fn run_once(ctx: &Context, spawn: Spawner) -> Result<Value> {
    let Some(_global) = FileLock::try_acquire(&ctx.db_root.join("issue-agents/runner.lock"))?
    else {
        return Ok(json!({"skipped": "another runner holds the lock"}));
    };
    let targets = tracked(ctx);
    let (recovered, observed) = reconcile_targets(&targets, ctx.settings);
    let mut running: u64 = targets
        .iter()
        .map(|(t, _)| list(t).iter().filter(|j| ours(j)).count() as u64)
        .sum();
    let mut started = Vec::new();
    let mut waiting = Vec::new();
    for (target, _) in &targets {
        if running >= ctx.settings.global_cap {
            break;
        }
        let Some(_lock) = FileLock::try_acquire(&jobs_dir(target).join("runner.lock"))? else {
            continue;
        };
        let jobs = list(target);
        if jobs.iter().any(ours) {
            continue;
        }
        let Some(next) = jobs
            .iter()
            .find(|j| j.status == Status::Queued && j.owner == Owner::Runner)
        else {
            continue;
        };
        match start(target, &next.fingerprint, ctx.settings, spawn) {
            Ok(true) => {
                running += 1;
                started.push(json!({"target": target.id, "job": next.fingerprint}));
            }
            Ok(false) => waiting.push(json!({"target": target.id, "job": next.fingerprint})),
            Err(error) => crate::poll::log(&format!(
                "{}: could not start job {}: {error:#}",
                target.id,
                next.fp8()
            )),
        }
    }
    Ok(
        json!({"started": started, "waiting": waiting, "recovered": recovered, "observed": observed, "running": running}),
    )
}

fn reconcile_targets(
    targets: &[(Target, Option<Project>)],
    settings: &Settings,
) -> (Vec<String>, Vec<Value>) {
    let mut recovered = Vec::new();
    let mut observed = Vec::new();
    for (target, _) in targets {
        recovered.extend(recover(target, settings));
        observed.extend(observe::observe(target, settings).into_iter().map(|mut o| {
            o["target"] = json!(target.id);
            o
        }));
    }
    (recovered, observed)
}

/// Recover and observe every tracked job without starting any: run once when
/// the service starts, so a restart never leaves a job looking alive.
pub fn reconcile(ctx: &Context) -> Result<Value> {
    let Some(_global) = FileLock::try_acquire(&ctx.db_root.join("issue-agents/runner.lock"))?
    else {
        return Ok(json!({"skipped": "another runner holds the lock"}));
    };
    let (recovered, observed) = reconcile_targets(&tracked(ctx), ctx.settings);
    Ok(json!({"recovered": recovered, "observed": observed}))
}

fn tick_path(db_root: &Path) -> PathBuf {
    db_root.join("issue-agents/runner.json")
}

/// Note that the runner ticked, for the dashboard's stale-service banner.
pub fn record_tick(db_root: &Path) {
    let record = json!({"ticked_at": now(), "pid": std::process::id()});
    if let Err(error) = write_private(&tick_path(db_root), record.to_string().as_bytes()) {
        crate::poll::log(&format!(
            "issue agents: could not record the tick: {error:#}"
        ));
    }
}

/// `{ticked_at, pid}` of the last runner tick on this machine, or null.
pub fn last_tick(db_root: &Path) -> Value {
    crate::store::read_json::<Value>(&tick_path(db_root))
        .filter(|v| v["ticked_at"].is_string())
        .map_or(
            Value::Null,
            |v| json!({"ticked_at": v["ticked_at"], "pid": v["pid"]}),
        )
}

/// How a job whose supervisor is gone ends, once its agent has been dealt with.
enum Ending {
    Fail(String),
    Detach(String),
}

/// Stop the agent of a job nobody supervises any more, without trusting a pid
/// it cannot verify or claiming a Herdr agent stopped while it still works.
fn end_orphan(settings: &Settings, job: &Job, agent: Option<&Proc>) -> Ending {
    const GONE: &str = "the runner stopped supervising this job before it finished";
    if job.launcher == Launcher::Herdr && job.herdr_tab.is_some() {
        let tab = job.herdr_tab.as_ref().map_or("", |t| t.tab_id.as_str());
        return match halt_herdr(settings, job) {
            Halt::Idle => Ending::Fail(format!(
                "{GONE}; the agent was interrupted and is idle in Herdr tab {tab}, so its session can be resumed"
            )),
            Halt::TabClosed => Ending::Fail(format!(
                "{GONE}; the agent did not go idle after an interrupt, so Herdr tab {tab} was closed; the session resumes up to its last saved message"
            )),
            Halt::StillWorking(why) => Ending::Detach(format!(
                "{GONE} and could not stop the agent ({why}); it may still be working in Herdr tab {tab}"
            )),
        };
    }
    match agent.map(|a| process::terminate(a, settings.grace)) {
        Some(process::Terminated::Refused(why)) => Ending::Detach(format!(
            "{GONE}; its agent process was not signalled because {why}; check it and stop it yourself if it still runs"
        )),
        _ => Ending::Fail(GONE.to_owned()),
    }
}

fn stale_stop(job: &Job) -> bool {
    job.status == Status::Running
        && job.stopping.as_deref().is_some_and(|at| {
            chrono::DateTime::parse_from_rfc3339(at).map_or(true, |at| {
                chrono::Utc::now().signed_duration_since(at)
                    > chrono::Duration::from_std(STALE_STOP).unwrap_or_default()
            })
        })
}

/// A background job taken over and waiting for its agent's step boundary.
pub fn taking_over(job: &Job) -> bool {
    job.status == Status::Running
        && job.owner == Owner::You
        && job.launcher == Launcher::Background
        && job.stopping.is_some()
}

/// Finish a take-over nobody supervises: stop the agent now by its verified
/// identity, or detach the job when that identity cannot be confirmed.
pub fn stop_taken_over(settings: &Settings, target: &Target, fp: &str) -> Result<()> {
    let procs = read_procs(target, fp);
    let result = procs
        .agent
        .as_ref()
        .map(|a| process::terminate(a, settings.grace));
    let note = step_stop_note(settings, &StepStop::Unsupervised);
    mutate(target, fp, |job| {
        if !taking_over(job) {
            return Ok(());
        }
        match &result {
            Some(process::Terminated::Refused(why)) => job.detach(&format!(
                "taken over without a live supervisor; the agent process was not signalled because {why}; it may still be running, so check it and stop it yourself"
            )),
            _ => {
                job.stopping = None;
                job.pid = None;
                write_procs(target, fp, |p| {
                    p.agent = None;
                    p.supervisor = None;
                })?;
                job.event("takeover", &note);
            }
        }
        Ok(())
    })
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// End running jobs whose supervisor is gone and clear dead diff viewers.
/// Only processes whose OS identity still matches their record are signalled.
pub fn recover(target: &Target, settings: &Settings) -> Vec<String> {
    let mut recovered = Vec::new();
    for job in list(target) {
        let fp = job.fingerprint.clone();
        let procs = read_procs(target, &fp);
        if taking_over(&job) && !super::proc_alive(procs.supervisor.as_ref()) {
            if stop_taken_over(settings, target, &fp).is_ok() {
                recovered.push(fp.clone());
            }
            continue;
        }
        let ending = if stale_stop(&job) && job.owner == Owner::Runner {
            Some(Ending::Detach(
                "a Stop began interrupting the agent but never finished; the agent may still be working in its Herdr tab".to_owned(),
            ))
        } else if ours(&job) {
            match procs.supervisor.as_ref().map(process::check) {
                Some(process::Check::Same) => None,
                Some(process::Check::Unverified(why)) => Some(Ending::Detach(format!(
                    "the runner cannot verify this job's supervisor ({why}), so it no longer manages the agent"
                ))),
                Some(process::Check::Gone) | None => {
                    Some(end_orphan(settings, &job, procs.agent.as_ref()))
                }
            }
        } else {
            None
        };
        if let Some(ending) = ending {
            // An agent that wrote its diagnosis before its supervisor went is not failed.
            let harvested = match &ending {
                Ending::Fail(_) => observe::read_transcript(settings, &job)
                    .and_then(|t| t.diagnosis)
                    .and_then(Result::ok),
                Ending::Detach(_) => None,
            };
            let result = mutate(target, &fp, |current| {
                if current.status == Status::Running && current.owner == Owner::Runner {
                    match (&ending, &harvested) {
                        (Ending::Fail(why), Some(diagnosis)) => {
                            let at = now();
                            write_private(
                                &diagnosis_path(target, &fp),
                                packet::render(current, diagnosis, &at).as_bytes(),
                            )?;
                            current.status = Status::Diagnosed;
                            current.finished_at = Some(at);
                            current.pid = None;
                            current.event(
                                "diagnosed",
                                &format!(
                                    "{why}, but its session transcript holds a valid diagnosis"
                                ),
                            );
                        }
                        (Ending::Fail(why), None) => current.fail(why),
                        (Ending::Detach(why), _) => current.detach(why),
                    }
                    current.stopping = None;
                }
                // A detached job keeps the identity of what may still run, so
                // cleanup can tell when it is really gone.
                if matches!(ending, Ending::Fail(_)) {
                    write_procs(target, &fp, |p| {
                        p.supervisor = None;
                        p.agent = None;
                    })?;
                }
                Ok(())
            });
            if result.is_ok() {
                recovered.push(fp.clone());
            }
        }
        if job.difit.pid.is_some() && !difit::running(target, &job) {
            let _ = mutate(target, &fp, |job| {
                job.difit = super::Difit::default();
                write_procs(target, &fp, |p| p.difit = None)?;
                job.event("difit", "the diff viewer is no longer running");
                Ok(())
            });
        }
    }
    recovered
}

/// Move one queued job to running and start its supervisor. `false` when it must wait.
fn start(target: &Target, fp: &str, settings: &Settings, spawn: Spawner) -> Result<bool> {
    let run = {
        let _lock = state_lock(target)?;
        let Some(mut job) = load(target, fp)? else {
            return Ok(false);
        };
        if job.status != Status::Queued || job.owner != Owner::Runner {
            return Ok(false);
        }
        let resume = job.session_id.is_some();
        if !resume && starts_today(target) >= settings.daily_cap {
            if job.events.last().is_none_or(|e| e.kind != "waiting") {
                job.event(
                    "waiting",
                    &format!(
                        "daily cap of {} jobs for this project reached; it starts tomorrow (UTC)",
                        settings.daily_cap
                    ),
                );
                super::save(target, &job)?;
            }
            return Ok(false);
        }
        job.launcher = if job.herdr_tab.is_some() || settings.herdr_workspace.is_some() {
            Launcher::Herdr
        } else {
            Launcher::Background
        };
        job.status = Status::Running;
        job.run = Some(super::settings::random_hex(8));
        let at = now();
        if !resume || job.started_at.is_none() {
            job.started_at = Some(at.clone());
        }
        job.last_activity_at = Some(at);
        job.finished_at = None;
        job.error = None;
        job.pid = None;
        job.event(
            if resume { "resumed" } else { "started" },
            if resume {
                "runner resumes the session"
            } else {
                "runner started the job"
            },
        );
        if !resume {
            count_start(target)?;
        }
        super::save(target, &job)?;
        job.run
    };
    match spawn(target, fp) {
        Ok(proc) => {
            let _lock = state_lock(target)?;
            // A supervisor that already finished has cleared its record; do not bring it back.
            if load(target, fp)?
                .is_some_and(|j| j.status == Status::Running && same_run(&j, run.as_deref()))
            {
                write_procs(target, fp, |p| p.supervisor = Some(proc))?;
            }
        }
        Err(error) => {
            let message = format!("could not start the job's supervisor: {error:#}");
            let _ = mutate(target, fp, |job| {
                job.fail(&message);
                Ok(())
            });
        }
    }
    Ok(true)
}

static CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// Reap supervisors that have exited, so a long-lived service leaves no zombies.
pub fn reap() {
    let mut children = CHILDREN.lock().unwrap_or_else(PoisonError::into_inner);
    children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
}

/// The production spawner: this executable, detached in its own process group.
pub fn process_spawner(
    db_root: PathBuf,
    config_path: PathBuf,
) -> impl Fn(&Target, &str) -> Result<Proc> {
    move |target: &Target, fp: &str| {
        let exe = std::env::current_exe().context("finding this executable")?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path(target, fp))?;
        let mut command = Command::new(&exe);
        command
            .arg("--db-root")
            .arg(&db_root)
            .arg("--config")
            .arg(&config_path)
            .args(["jobs", "supervise", "--target", &target.id, "--job", fp])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().context("starting the supervisor")?;
        let proc = process::capture(child.id());
        CHILDREN
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(child);
        Ok(proc)
    }
}

fn log_report(report: &Value) {
    let busy = ["started", "recovered", "observed"]
        .iter()
        .any(|key| report[*key].as_array().is_some_and(|a| !a.is_empty()));
    if busy {
        crate::poll::log(&format!("issue agents: {report}"));
    }
}

/// The service tick: reap finished supervisors, then one scan.
pub fn tick(db_root: &Path, config_path: &Path, config: Option<&Config>) {
    reap();
    let settings = Settings::from_config(config);
    let ctx = Context {
        db_root,
        config,
        settings: &settings,
    };
    let spawner = process_spawner(db_root.to_owned(), config_path.to_owned());
    match run_once(&ctx, &spawner) {
        Ok(report) => log_report(&report),
        Err(error) => crate::poll::log(&format!("issue agents: {error:#}")),
    }
    record_tick(db_root);
}

/// At service start: recover and observe every job before anything else.
pub fn start_up(db_root: &Path, config: Option<&Config>) {
    let settings = Settings::from_config(config);
    let ctx = Context {
        db_root,
        config,
        settings: &settings,
    };
    match reconcile(&ctx) {
        Ok(report) => log_report(&report),
        Err(error) => crate::poll::log(&format!("issue agents: reconcile at start: {error:#}")),
    }
    record_tick(db_root);
}

// ---------- the supervisor ----------

enum Outcome {
    Diagnosed(packet::Diagnosis),
    /// Stopped or taken over while running: whoever did it owns the job now.
    Released,
    /// The agent could not be stopped; the runner lets go and says so.
    Detached(String),
}

/// Supervise one running job to its end.
pub fn supervise(ctx: &Context, target_id: &str, fp: &str) -> Result<()> {
    let target = resolve(ctx.db_root, target_id)?;
    let Some(job) = load(&target, fp)? else {
        bail!("no job {fp}");
    };
    if !ours(&job) {
        return Ok(());
    }
    let run = job.run.clone();
    let run = run.as_deref();
    let project = project_for(ctx, &target);
    let outcome = drive(ctx.settings, &target, project.as_ref(), &job);
    let finished = now();
    let result = match outcome {
        Ok(Outcome::Released) => Ok(()),
        Ok(Outcome::Detached(why)) => {
            log_line(&target, fp, &format!("job detached: {why}"));
            mutate(&target, fp, |job| {
                if ours_run(job, run) {
                    job.detach(&why);
                }
                Ok(())
            })
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
        }
        Ok(Outcome::Diagnosed(diagnosis)) => {
            let markdown = packet::render(&job, &diagnosis, &finished);
            write_private(&diagnosis_path(&target, fp), markdown.as_bytes()).and_then(|()| {
                mutate(&target, fp, |job| {
                    if ours_run(job, run) {
                        job.status = Status::Diagnosed;
                        job.finished_at = Some(finished.clone());
                        job.pid = None;
                        job.event("diagnosed", "diagnosis written");
                    }
                    Ok(())
                })
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!(e.to_string()))
            })
        }
        Err(error) => {
            let message = format!("{error:#}");
            log_line(&target, fp, &format!("job failed: {message}"));
            mutate(&target, fp, |job| {
                if ours_run(job, run) {
                    job.fail(&message);
                }
                Ok(())
            })
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
        }
    };
    let current = load(&target, fp)?.filter(|j| same_run(j, run));
    if current
        .as_ref()
        .is_some_and(|j| j.status.terminal() || j.owner == Owner::You)
    {
        let _ = difit::stop(&target, fp, ctx.settings.grace);
    }
    let _lock = state_lock(&target)?;
    // A newer run's processes are its own supervisor's to clear.
    if load(&target, fp)?.is_some_and(|j| same_run(&j, run)) {
        write_procs(&target, fp, |p| {
            p.supervisor = None;
            p.agent = None;
        })?;
    }
    result
}

/// The packet goes through a private temporary file in the job directory
/// (outside the worktree), removed after launch.
struct PacketFile(PathBuf);

impl PacketFile {
    fn write(target: &Target, job: &Job, packet: &str) -> Result<Self> {
        let path = jobs_dir(target).join(format!(
            "{}.packet-{}",
            job.fingerprint,
            super::settings::random_hex(6)
        ));
        write_private(&path, packet.as_bytes())?;
        Ok(Self(path))
    }
    fn read(&self) -> Result<String> {
        std::fs::read_to_string(&self.0).context("reading the context packet")
    }
}

impl Drop for PacketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn drive(
    settings: &Settings,
    target: &Target,
    project: Option<&Project>,
    job: &Job,
) -> Result<Outcome> {
    let ensured = worktree::ensure(target, job)?;
    if job.worktree.base_commit.as_deref() != Some(ensured.base_commit.as_str()) {
        let run = job.run.as_deref();
        mutate(target, &job.fingerprint, |job| {
            if same_run(job, run) {
                job.worktree.base_commit = Some(ensured.base_commit.clone());
            }
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    let tree = ensured.path;
    let resume = job.session_id.is_some();
    let packet_file = PacketFile::write(target, job, &packet::build(target, project, job))?;
    let prompt = if resume {
        packet::continuation()
    } else {
        packet::prompt(&packet_file.read()?)
    };
    match job.launcher {
        Launcher::Background => {
            background(settings, target, job, &tree, resume, &prompt, packet_file)
        }
        Launcher::Herdr => herdr(settings, target, job, &tree, resume, &prompt, packet_file),
    }
}

/// Heartbeat and check whether the job is still the runner's. `false` once
/// someone stopped it or took it over. Activity is the transcript's last
/// write; until there is a transcript, the heartbeat itself.
fn heartbeat(settings: &Settings, target: &Target, fp: &str, run: Option<&str>) -> bool {
    let written = load(target, fp)
        .ok()
        .flatten()
        .and_then(|job| observe::transcript_path(settings, &job))
        .and_then(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
        .map(observe::stamp);
    mutate(target, fp, |job| {
        if ours_run(job, run) {
            job.last_activity_at = Some(written.clone().unwrap_or_else(now));
        }
        Ok(ours_run(job, run))
    })
    .map(|(_, still)| still)
    .unwrap_or(true)
}

fn still_ours(target: &Target, fp: &str, run: Option<&str>) -> bool {
    load(target, fp)
        .ok()
        .flatten()
        .is_some_and(|j| ours_run(&j, run))
}

fn persist_session(target: &Target, fp: &str, run: Option<&str>, session: &str) {
    let _ = mutate(target, fp, |job| {
        if same_run(job, run) && job.session_id.as_deref() != Some(session) {
            job.session_id = Some(session.to_owned());
            job.event("session", &format!("session {session}"));
        }
        Ok(())
    });
}

/// What a monitored child process ended with.
enum Watched {
    Exited(std::process::ExitStatus),
    Released,
    TimedOut,
}

/// After Stop, the agent was already signalled: wait for it, then make sure.
fn wait_then_kill(settings: &Settings, child: &mut Child, kill: &dyn Fn(&mut Child)) {
    let deadline = Instant::now() + settings.grace * 2;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(settings.poll);
    }
    kill(child);
    let _ = child.wait();
}

/// Wait for `child`, heartbeating; on timeout `kill` it, on release hand it to
/// `release`. `on_tick` runs every poll. `job` is the run being supervised.
fn watch(
    settings: &Settings,
    target: &Target,
    job: &Job,
    child: &mut Child,
    kill: &dyn Fn(&mut Child),
    release: &mut dyn FnMut(&mut Child),
    mut on_tick: impl FnMut(),
) -> Result<Watched> {
    let started = Instant::now();
    let mut beat = Instant::now();
    loop {
        on_tick();
        if let Some(status) = child.try_wait()? {
            return Ok(Watched::Exited(status));
        }
        if started.elapsed() >= settings.timeout {
            kill(child);
            let _ = child.wait();
            return Ok(Watched::TimedOut);
        }
        if beat.elapsed() >= settings.heartbeat {
            beat = Instant::now();
            if !heartbeat(settings, target, &job.fingerprint, job.run.as_deref()) {
                release(child);
                return Ok(Watched::Released);
            }
        }
        std::thread::sleep(settings.poll);
    }
}

/// The newest part of a session transcript read for a step boundary.
const TRANSCRIPT_TAIL: u64 = 1 << 20;

/// Whether the job's session transcript has been quiet at a step boundary.
fn at_step_boundary(settings: &Settings, target: &Target, fp: &str, tree: &Path) -> bool {
    let Some(job) = load(target, fp).ok().flatten() else {
        return false;
    };
    let Some(path) = job
        .session_id
        .as_deref()
        .and_then(|s| agent::transcript(job.agent, &settings.home, tree, s))
    else {
        return false;
    };
    let quiet = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= settings.step_quiet);
    quiet && observe::tail(&path, TRANSCRIPT_TAIL).is_some_and(|text| agent::step_boundary(&text))
}

/// How a taken-over background agent ended.
pub enum StepStop {
    Boundary,
    Exited,
    NoBoundary,
    Unsupervised,
}

/// The take-over event for how the agent ended.
pub fn step_stop_note(settings: &Settings, how: &StepStop) -> String {
    let wait = settings.takeover_wait.as_secs();
    match how {
        StepStop::Boundary => "the background agent stopped after its current step; its session is yours to resume".to_owned(),
        StepStop::Exited => "the background agent finished on its own; its session is yours to resume".to_owned(),
        StepStop::NoBoundary => format!("the background agent did not reach a step boundary within {wait}s, so it was stopped immediately, mid-step; its session can be resumed up to its last saved message"),
        StepStop::Unsupervised => "the job had no live supervisor to wait for a step boundary, so the background agent was stopped immediately; its session can be resumed up to its last saved message".to_owned(),
    }
}

/// Take over: let the agent finish its current step (bounded by the takeover
/// wait), then stop it through the supervisor's own unreaped child handle,
/// whose pid cannot have been reused.
fn stop_at_step(
    settings: &Settings,
    target: &Target,
    fp: &str,
    run: Option<&str>,
    child: &mut Child,
    tree: &Path,
) {
    let deadline = Instant::now() + settings.takeover_wait;
    let how = loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break StepStop::Exited;
        }
        if at_step_boundary(settings, target, fp, tree) {
            break StepStop::Boundary;
        }
        if Instant::now() >= deadline {
            break StepStop::NoBoundary;
        }
        std::thread::sleep(settings.poll);
    };
    process::terminate_child(child, settings.grace);
    let note = step_stop_note(settings, &how);
    let _ = mutate(target, fp, |job| {
        if same_run(job, run) && job.stopping.is_some() {
            job.stopping = None;
            job.pid = None;
            job.event("takeover", &note);
        }
        Ok(())
    });
}

fn background(
    settings: &Settings,
    target: &Target,
    job: &Job,
    tree: &Path,
    resume: bool,
    prompt: &str,
    packet_file: PacketFile,
) -> Result<Outcome> {
    let fp = job.fingerprint.as_str();
    let run = job.run.as_deref();
    let exe = settings.exe(job.agent).with_context(|| {
        let other = if job.agent == AgentKind::Claude { "codex" } else { "claude" };
        format!("{} is not installed on this machine: install it, or set issue_agents.agent to {other} in signals.json", job.agent.name())
    })?;
    let session = match (job.agent, &job.session_id) {
        (_, Some(session)) => session.clone(),
        (AgentKind::Claude, None) => {
            let session = uuid_v4();
            persist_session(target, fp, run, &session);
            session
        }
        (AgentKind::Codex, None) => String::new(),
    };
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(super::log_path(target, fp))?;
    let mut command = Command::new(&exe[0]);
    command
        .args(&exe[1..])
        .args(background_args(job.agent, tree, &session, resume, prompt))
        .current_dir(tree)
        .env(
            "PATH",
            super::settings::child_path(Path::new(&exe[0]).parent()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log);
    for name in SCRUBBED_ENV {
        command.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", job.agent.name()))?;
    drop(packet_file);
    let pid = child.id();
    let agent_proc = process::capture(pid);
    mutate(target, fp, |job| {
        if !same_run(job, run) {
            return Ok(());
        }
        job.pid = Some(pid);
        write_procs(target, fp, |p| p.agent = Some(agent_proc.clone()))?;
        job.event(
            "agent",
            &format!("{} started in the background (pid {pid})", job.agent.name()),
        );
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let (tx, rx) = mpsc::channel::<String>();
    let codex = job.agent == AgentKind::Codex && session.is_empty();
    let stdout = child.stdout.take().context("agent stdout")?;
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        let mut sent = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if codex && !sent {
                if let Some(id) = agent::thread_started(&line) {
                    let _ = tx.send(id);
                    sent = true;
                }
            }
            if output.len() + line.len() < OUTPUT_LIMIT {
                output.push_str(&line);
                output.push('\n');
            }
        }
        output
    });
    let grace = settings.grace;
    let kill = move |child: &mut Child| process::terminate_child(child, grace);
    let mut release = |child: &mut Child| {
        let taking_over = load(target, fp)
            .ok()
            .flatten()
            .is_some_and(|j| same_run(&j, run) && j.owner == Owner::You && j.stopping.is_some());
        if taking_over {
            stop_at_step(settings, target, fp, run, child, tree);
        } else {
            wait_then_kill(settings, child, &kill);
        }
    };
    let watched = watch(
        settings,
        target,
        job,
        &mut child,
        &kill,
        &mut release,
        || {
            if let Ok(id) = rx.try_recv() {
                persist_session(target, fp, run, &id);
            }
        },
    )?;
    let output = reader.join().unwrap_or_default();
    if let Ok(id) = rx.try_recv() {
        persist_session(target, fp, run, &id);
    }
    let _ = mutate(target, fp, |job| {
        if same_run(job, run) {
            job.pid = None;
            write_procs(target, fp, |p| p.agent = None)?;
        }
        Ok(())
    });
    match watched {
        Watched::Released => return Ok(Outcome::Released),
        Watched::TimedOut => bail!(
            "timed out after {}s; the agent was stopped",
            settings.timeout.as_secs()
        ),
        Watched::Exited(_) if !still_ours(target, fp, run) => return Ok(Outcome::Released),
        Watched::Exited(status) if !status.success() => {
            bail!(
                "{} exited with {}; see {}",
                job.agent.name(),
                status
                    .code()
                    .map_or("a signal".to_owned(), |c| format!("code {c}")),
                super::log_path(target, fp).display()
            )
        }
        Watched::Exited(_) => {}
    }
    Ok(Outcome::Diagnosed(packet::parse(&agent::replies(&output))?))
}

/// A launch failed after its Herdr tab was created: close that tab so no
/// orphan is left, and say in the job's events whether it worked.
fn close_failed_launch(
    herdr: &Herdr,
    target: &Target,
    fp: &str,
    run: Option<&str>,
    tab: &HerdrTab,
) {
    let closed = herdr.close_tab(&tab.tab_id).and_then(|()| {
        anyhow::ensure!(
            !herdr.pane_exists(&tab.pane_id),
            "its pane {} is still there",
            tab.pane_id
        );
        Ok(())
    });
    let _ = mutate(target, fp, |job| {
        if !same_run(job, run) {
            return Ok(());
        }
        match &closed {
            Ok(()) => {
                if job.herdr_tab.as_ref() == Some(tab) {
                    job.herdr_tab = None;
                }
                job.event(
                    "herdr",
                    &format!("the launch failed after Herdr tab {} was created, so the tab was closed", tab.tab_id),
                );
            }
            Err(why) => job.event(
                "herdr",
                &format!("the launch failed after Herdr tab {} was created, and the tab could not be closed ({why:#}); close it yourself", tab.tab_id),
            ),
        }
        Ok(())
    });
}

fn herdr(
    settings: &Settings,
    target: &Target,
    job: &Job,
    tree: &Path,
    resume: bool,
    prompt: &str,
    packet_file: PacketFile,
) -> Result<Outcome> {
    let fp = job.fingerprint.as_str();
    let run = job.run.as_deref();
    let argv = settings
        .herdr
        .as_ref()
        .context("issue_agents.herdr is enabled in signals.json but herdr is not installed")?;
    let workspace = settings
        .herdr_workspace
        .as_deref()
        .context("issue_agents.herdr.workspace is not set")?;
    let herdr = Herdr { argv };
    let name = job.herdr_name();
    let mut prompt = prompt.to_owned();
    let mut created: Option<HerdrTab> = None;
    let mut launch = || -> Result<Child> {
        if job.herdr_tab.is_none() || !resume {
            let tab = herdr.create_tab(workspace, &tree.display().to_string(), &name)?;
            created = Some(tab.clone());
            let session = match job.agent {
                AgentKind::Claude => Some(job.session_id.clone().unwrap_or_else(uuid_v4)),
                AgentKind::Codex => None,
            };
            let (_, recorded) = mutate(target, fp, |job| {
                if !same_run(job, run) {
                    return Ok(false);
                }
                job.herdr_tab = Some(tab.clone());
                if let Some(session) = &session {
                    job.session_id = Some(session.clone());
                }
                job.event("herdr", &format!("Herdr tab {} created", tab.tab_id));
                Ok(true)
            })
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            anyhow::ensure!(
                recorded,
                "the job was started again while this run launched"
            );
            herdr.start_agent(
                &name,
                job.agent,
                &tab.pane_id,
                &interactive_args(job.agent, session.as_deref()),
            )?;
            if job.agent == AgentKind::Codex {
                prompt = format!("{}\n\n{prompt}", packet::policy());
            }
            let _ = mutate(target, fp, |job| {
                if same_run(job, run) {
                    job.event(
                        "agent",
                        &format!("{} started in Herdr tab {}", job.agent.name(), tab.tab_id),
                    );
                }
                Ok(())
            });
        }
        if let Ok((_, Some(session))) = herdr.get(&name) {
            persist_session(target, fp, run, &session);
        }
        herdr.spawn(&Herdr::prompt_args(&name, &prompt, settings.timeout))
    };
    let launched = launch();
    let mut child = match launched {
        Ok(child) => child,
        Err(error) => {
            if let Some(tab) = created {
                close_failed_launch(&herdr, target, fp, run, &tab);
            }
            return Err(error);
        }
    };
    drop(packet_file);
    // The waiter is only `herdr agent prompt --wait`; the agent itself is stopped by `halted`.
    let kill = |child: &mut Child| {
        let _ = child.kill();
    };
    let mut release = |child: &mut Child| wait_then_kill(settings, child, &kill);
    let watched = watch(
        settings,
        target,
        job,
        &mut child,
        &kill,
        &mut release,
        || {},
    )?;
    match watched {
        Watched::Released => return Ok(Outcome::Released),
        Watched::TimedOut => {
            return halted(
                settings,
                target,
                fp,
                run,
                &format!("timed out after {}s", settings.timeout.as_secs()),
            )
        }
        Watched::Exited(_) if !still_ours(target, fp, run) => return Ok(Outcome::Released),
        Watched::Exited(_) => {}
    }
    let (ok, _, stderr) = finish(child, Duration::from_secs(5))?;
    let (state, session) = herdr.get(&name)?;
    if let Some(session) = &session {
        persist_session(target, fp, run, session);
    }
    match state.as_str() {
        "done" | "idle" if ok => {}
        "blocked" => bail!(
            "the agent is blocked waiting for input in Herdr tab {name}; take over to answer it"
        ),
        other => {
            let why = if ok {
                format!("herdr reports the agent as {other:?}, not done")
            } else {
                format!(
                    "herdr agent prompt failed ({}) with the agent {other:?}",
                    super::bounded(&stderr, 200)
                )
            };
            return halted(settings, target, fp, run, &why);
        }
    }
    let session = load(target, fp)?
        .and_then(|j| j.session_id)
        .context("herdr reported no session id for the agent")?;
    let home = &settings.home;
    let deadline = Instant::now() + TRANSCRIPT_WAIT;
    let mut last_error = None;
    loop {
        if let Some(path) = agent::transcript(job.agent, home, tree, &session) {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            match packet::parse(&agent::replies(&text)) {
                Ok(diagnosis) => return Ok(Outcome::Diagnosed(diagnosis)),
                Err(error) => last_error = Some(error),
            }
        }
        if Instant::now() >= deadline {
            return Err(last_error.unwrap_or_else(|| {
                anyhow::anyhow!(
                    "no {} transcript for session {session}; the diagnosis cannot be read",
                    job.agent.name()
                )
            }));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// What interrupting a Herdr agent achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Halt {
    /// Idle or done: the tab stays open and the session resumable.
    Idle,
    /// It did not settle, so its tab was closed.
    TabClosed,
    /// It may still be working; nothing could stop it.
    StillWorking(String),
}

fn settled(state: &str) -> bool {
    matches!(state, "idle" | "done")
}

/// Interrupt a Herdr agent: `esc`, then wait up to the grace period for it to
/// be idle or done. If it does not settle, close its tab and confirm the pane
/// is gone. Never reports the agent stopped while Herdr says it still works.
pub fn halt_herdr(settings: &Settings, job: &Job) -> Halt {
    let Some(argv) = settings.herdr.as_ref() else {
        return Halt::StillWorking(
            "herdr is not installed on this machine, so the agent could not be interrupted".into(),
        );
    };
    let herdr = Herdr { argv };
    let name = job.herdr_name();
    if herdr.get(&name).is_ok_and(|(state, _)| settled(&state)) {
        return Halt::Idle;
    }
    let interrupted = herdr.send_keys(&name, &["esc"]);
    let _ = herdr.wait(&name, &["idle", "done"], settings.grace);
    if herdr.get(&name).is_ok_and(|(state, _)| settled(&state)) {
        return Halt::Idle;
    }
    let Some(tab) = &job.herdr_tab else {
        return Halt::StillWorking("no Herdr tab is recorded for the job".into());
    };
    if let Err(error) = herdr.close_tab(&tab.tab_id) {
        let sent = match interrupted {
            Ok(()) => "an interrupt was sent".to_owned(),
            Err(e) => format!("the interrupt failed: {e:#}"),
        };
        return Halt::StillWorking(format!(
            "{sent}, it did not go idle, and closing Herdr tab {} failed: {error:#}",
            tab.tab_id
        ));
    }
    if herdr.pane_exists(&tab.pane_id) {
        return Halt::StillWorking(format!(
            "Herdr tab {} was closed but its pane {} is still there",
            tab.tab_id, tab.pane_id
        ));
    }
    Halt::TabClosed
}

/// End a supervised Herdr job that has to stop (timeout, or Herdr gave up
/// waiting): fail it once the agent is idle or its tab closed, else detach it.
fn halted(
    settings: &Settings,
    target: &Target,
    fp: &str,
    run: Option<&str>,
    why: &str,
) -> Result<Outcome> {
    let job = load(target, fp)?.context("the job disappeared")?;
    if !ours_run(&job, run) {
        return Ok(Outcome::Released);
    }
    let tab = job.herdr_tab.as_ref().map_or("", |t| t.tab_id.as_str());
    match halt_herdr(settings, &job) {
        Halt::Idle => bail!(
            "{why}; the agent was interrupted and is idle in Herdr tab {tab}, so its session can be resumed"
        ),
        Halt::TabClosed => bail!(
            "{why}; the agent did not go idle within {}s of an interrupt, so Herdr tab {tab} was closed; the session resumes up to its last saved message",
            settings.grace.as_secs_f32()
        ),
        Halt::StillWorking(problem) => Ok(Outcome::Detached(format!(
            "{why}, but the agent could not be stopped ({problem}); it may still be working in Herdr tab {tab}"
        ))),
    }
}

/// Settings and the queue kick shared by the CLI actions.
pub fn kick(ctx: &Context, config_path: &Path) {
    let spawner = process_spawner(ctx.db_root.to_owned(), config_path.to_owned());
    if let Err(error) = run_once(ctx, &spawner) {
        crate::poll::log(&format!("issue agents: {error:#}"));
    }
}
