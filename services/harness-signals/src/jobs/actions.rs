//! What the dashboard and a person can do to jobs: Analyze (enqueue), Stop,
//! Take over, Hand back, cleanup, and the read views. Each change re-reads the
//! job under the state lock and writes it atomically.

use super::runner::{self, project_for, Context};
use super::{
    diagnosis_path, difit, load, mutate, now, process, read_procs, settings, starts_today,
    state_lock, worktree, write_procs, AgentKind, Job, JobError, JobResult, Launcher, Owner,
    RootFrame, Status,
};
use crate::heat::root_frame;
use crate::model::clean_title;
use crate::store::{resolve, Target};
use serde_json::{json, Value};

fn target(ctx: &Context, id: &str) -> JobResult<Target> {
    if !crate::config::slug(id) || id.starts_with(['.', '-']) {
        return Err(JobError::NotFound("unknown project".into()));
    }
    let target =
        resolve(ctx.db_root, id).map_err(|_| JobError::NotFound("unknown project".into()))?;
    if target.id != id {
        return Err(JobError::NotFound("unknown project".into()));
    }
    Ok(target)
}

fn job(target: &Target, fp: &str) -> JobResult<Job> {
    if !super::valid_fingerprint(fp) {
        return Err(JobError::NotFound("unknown job".into()));
    }
    load(target, fp)?.ok_or_else(|| JobError::NotFound("unknown job".into()))
}

fn text_arg(value: &str, name: &str) -> JobResult<()> {
    if value.is_empty() || value.len() > 300 || value.chars().any(char::is_control) {
        return Err(JobError::Invalid(format!("invalid {name}")));
    }
    Ok(())
}

fn quota(ctx: &Context, target: &Target) -> JobResult<()> {
    let started = starts_today(target);
    if started >= ctx.settings.daily_cap {
        return Err(JobError::Quota(format!(
            "{started} jobs already started for this project today; the daily cap is {} (issue_agents.daily_cap)",
            ctx.settings.daily_cap
        )));
    }
    Ok(())
}

/// Queue an Analyze job for one issue on one card. An existing job for the same
/// root cause is returned as is; a finished one is reset only with `reanalyze`.
/// Returns the job and whether this call queued it.
pub fn analyze(
    ctx: &Context,
    target_id: &str,
    issue_key: &str,
    card_id: &str,
    reanalyze: bool,
) -> JobResult<(Job, bool)> {
    let target = target(ctx, target_id)?;
    text_arg(issue_key, "issue")?;
    text_arg(card_id, "card")?;
    ctx.config.ok_or_else(|| {
        JobError::Invalid("no signals configuration on this machine (signals.json)".into())
    })?;
    let project = project_for(ctx, &target)
        .ok_or_else(|| JobError::Invalid("this project is not tracked in signals.json".into()))?;
    let issue = target
        .issues()
        .remove(issue_key)
        .ok_or_else(|| JobError::NotFound("unknown issue".into()))?;
    let board = target
        .board()
        .ok_or_else(|| JobError::Invalid("the project has no architecture board".into()))?;
    let known_card = board["nodes"]
        .as_array()
        .is_some_and(|nodes| nodes.iter().any(|n| n["id"] == card_id));
    if !known_card {
        return Err(JobError::NotFound("unknown card".into()));
    }
    let (path, line) = root_frame(&issue, &board, &project.path_map, &target.root).ok_or_else(|| {
        JobError::Invalid("the issue has no in-app stack frame with a repository path and line, so it has no root cause to analyze".into())
    })?;
    let fp = super::fingerprint(&target.id, card_id, &path, line);
    let _lock = state_lock(&target)?;
    if let Some(mut existing) = load(&target, &fp)? {
        if !existing.status.terminal() || !reanalyze {
            return Ok((existing, false));
        }
        if existing.status == Status::Detached {
            return Err(JobError::Conflict(
                "the runner lost control of this job's agent (see its events): make sure it no longer runs, then run jobs cleanup before reanalyzing".into(),
            ));
        }
        quota(ctx, &target)?;
        let procs = read_procs(&target, &fp);
        if super::proc_maybe_alive(procs.agent.as_ref()) {
            return Err(JobError::Conflict(
                "the job's agent process may still be running; stop it before reanalyzing".into(),
            ));
        }
        if difit::running(&target, &existing) {
            return Err(JobError::Conflict(
                "stop the job's diff viewer before reanalyzing".into(),
            ));
        }
        let _ = std::fs::remove_file(diagnosis_path(&target, &fp));
        existing.status = Status::Queued;
        existing.owner = Owner::Runner;
        existing.agent = ctx.settings.pick_agent();
        existing.launcher = launcher(ctx);
        existing.session_id = None;
        existing.herdr_tab = None;
        existing.pid = None;
        existing.last_activity_at = None;
        existing.observed_at = None;
        existing.started_at = None;
        existing.finished_at = None;
        existing.error = None;
        existing.issue_key = issue_key.to_owned();
        existing.issue_title = clean_title(&issue.title);
        existing.issue_url = super::packet::bare_url(issue.url.as_deref());
        existing.event("queued", "reanalyze requested");
        super::save(&target, &existing)?;
        return Ok((existing, true));
    }
    quota(ctx, &target)?;
    let base = worktree::base_branch(&target).map_err(|e| JobError::Invalid(format!("{e:#}")))?;
    let at = now();
    let mut job = Job {
        version: 1,
        fingerprint: fp.clone(),
        target_id: target.id.clone(),
        card_id: card_id.to_owned(),
        issue_key: issue_key.to_owned(),
        issue_title: clean_title(&issue.title),
        issue_url: super::packet::bare_url(issue.url.as_deref()),
        root_frame: RootFrame { path, line },
        mode: "analyze".into(),
        status: Status::Queued,
        owner: Owner::Runner,
        created_at: at.clone(),
        updated_at: at,
        agent: ctx.settings.pick_agent(),
        session_id: None,
        worktree: worktree::planned(&target, &fp, base),
        machine: settings::machine(),
        launcher: launcher(ctx),
        herdr_tab: None,
        pid: None,
        last_activity_at: None,
        observed_at: None,
        started_at: None,
        finished_at: None,
        diagnosis_path: diagnosis_path(&target, &fp).display().to_string(),
        difit: super::Difit::default(),
        error: None,
        stopping: None,
        run: None,
        events: Vec::new(),
    };
    job.event(
        "queued",
        &format!(
            "Analyze queued for {}:{}",
            job.root_frame.path, job.root_frame.line
        ),
    );
    super::save(&target, &job)?;
    Ok((job, true))
}

fn launcher(ctx: &Context) -> Launcher {
    if ctx.settings.herdr_workspace.is_some() {
        Launcher::Herdr
    } else {
        Launcher::Background
    }
}

/// Stop the job's background agent by its verified identity. A process that
/// cannot be verified is never signalled: its supervisor, which holds the
/// child's handle, stops it, or the job is detached when there is none.
fn stop_background(ctx: &Context, target: &Target, fp: &str) -> JobResult<()> {
    let procs = read_procs(target, fp);
    let Some(agent) = procs.agent else {
        return Ok(());
    };
    let result = process::terminate(&agent, ctx.settings.grace);
    let supervised = super::proc_alive(procs.supervisor.as_ref());
    mutate(target, fp, |job| {
        match &result {
            process::Terminated::Stopped | process::Terminated::Gone => {
                job.pid = None;
                write_procs(target, fp, |p| p.agent = None)?;
                if result == process::Terminated::Stopped {
                    job.event(
                        "agent",
                        "agent process stopped (TERM, then KILL after the grace period)",
                    );
                }
            }
            process::Terminated::Refused(why) if supervised => job.event(
                "agent",
                &format!("the agent process was not signalled here because {why}; its supervisor stops it through its own process handle"),
            ),
            process::Terminated::Refused(why) => job.detach(&format!(
                "the agent process was not signalled because {why}; it may still be running, so check it and stop it yourself"
            )),
        }
        Ok(())
    })?;
    Ok(())
}

/// Stop a running Herdr job: the supervisor lets go, the agent is interrupted
/// and must go idle (else its tab is closed) before the job is called stopped.
fn stop_herdr(ctx: &Context, target: &Target, fp: &str) -> JobResult<Job> {
    let (job, begun) = mutate(target, fp, |job| {
        if job.status != Status::Running || job.stopping.is_some() {
            return Ok(false);
        }
        job.stopping = Some(now());
        job.event(
            "stopping",
            "stop requested: interrupting the agent and waiting for it to go idle",
        );
        Ok(true)
    })?;
    if !begun {
        return Ok(job);
    }
    let halt = runner::halt_herdr(ctx.settings, &job);
    let grace = ctx.settings.grace.as_secs_f32();
    mutate(target, fp, |job| {
        job.stopping = None;
        if job.status != Status::Running {
            return Ok(());
        }
        let tab = job
            .herdr_tab
            .as_ref()
            .map_or(String::new(), |t| t.tab_id.clone());
        match &halt {
            runner::Halt::Idle => {
                job.status = Status::Stopped;
                job.finished_at = Some(now());
                job.event("stopped", &format!("stopped by request: the agent was interrupted and is idle in Herdr tab {tab}; the tab is left open so the session can be resumed"));
            }
            runner::Halt::TabClosed => {
                job.status = Status::Stopped;
                job.finished_at = Some(now());
                job.herdr_tab = None;
                job.event("stopped", &format!("stopped by request: the agent did not go idle within {grace}s of the interrupt, so Herdr tab {tab} was closed; the session resumes up to its last saved message"));
            }
            runner::Halt::StillWorking(why) => job.detach(&format!(
                "stop requested, but the agent could not be stopped ({why}); it may still be working in Herdr tab {tab}"
            )),
        }
        Ok(())
    })?;
    difit::stop(target, fp, ctx.settings.grace)
}

/// Stop a queued or running job; session, worktree and diagnosis are kept.
pub fn stop(ctx: &Context, target_id: &str, fp: &str) -> JobResult<Job> {
    let target = target(ctx, target_id)?;
    let current = job(&target, fp)?;
    if current.status == Status::Running
        && current.launcher == Launcher::Herdr
        && current.herdr_tab.is_some()
    {
        return stop_herdr(ctx, &target, fp);
    }
    let (_, changed) = mutate(&target, fp, |job| {
        if job.status.terminal() || job.stopping.is_some() {
            return Ok(false);
        }
        job.status = Status::Stopped;
        job.finished_at = Some(now());
        job.event("stopped", "stopped by request");
        Ok(true)
    })?;
    if changed {
        stop_background(ctx, &target, fp)?;
    }
    difit::stop(&target, fp, ctx.settings.grace)
}

/// Shell-quote one word for a POSIX shell.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The commands a person runs to continue the job's session.
pub fn resume_commands(ctx: &Context, job: &Job) -> Value {
    let Some(session) = &job.session_id else {
        return Value::Null;
    };
    let tree = &job.worktree.path;
    let local = match job.agent {
        AgentKind::Claude => format!("cd {} && claude --resume {}", quote(tree), quote(session)),
        AgentKind::Codex => format!("codex resume {} --cd {}", quote(session), quote(tree)),
    };
    let ssh = ctx
        .settings
        .ssh
        .as_ref()
        .map(|dest| format!("ssh -t {} {}", quote(dest), quote(&local)));
    let herdr = match (&job.herdr_tab, job.launcher) {
        (Some(tab), Launcher::Herdr) => json!({
            "tab_id": tab.tab_id,
            "command": format!("herdr agent focus {}", quote(&job.herdr_name())),
            "note": "The agent is already in this Herdr tab: switch to it, or run the local command in any terminal on this machine.",
        }),
        _ => json!({
            "tab_id": null,
            "command": local,
            "note": "This job ran in the background: open a Herdr tab on this machine and run the command there.",
        }),
    };
    json!({"local": local, "ssh": ssh, "herdr": herdr})
}

/// Give the job to the person. A running background agent is marked stopping:
/// its supervisor lets it finish the current step (at most the takeover wait)
/// and then stops it. A Herdr agent is only released.
pub fn takeover(ctx: &Context, target_id: &str, fp: &str) -> JobResult<(Job, Value)> {
    let target = target(ctx, target_id)?;
    job(&target, fp)?;
    let wait = ctx.settings.takeover_wait.as_secs();
    let (_, changed) = mutate(&target, fp, |job| {
        if job.owner == Owner::You {
            return Ok(false);
        }
        if job.session_id.is_none() {
            return Err(JobError::Conflict(
                "the agent has no session yet; wait until it has started, or stop the job".into(),
            ));
        }
        if job.stopping.is_some() {
            return Err(JobError::Conflict(
                "the job is being stopped; take it over once the stop has finished".into(),
            ));
        }
        job.owner = Owner::You;
        let note = if job.launcher == Launcher::Herdr {
            "taken over: released from the runner; nothing was stopped, the agent keeps running in its Herdr tab".to_owned()
        } else if job.status == Status::Running {
            job.stopping = Some(now());
            format!("taken over: the background agent stops after its current step (at most {wait}s), then its session is yours to resume")
        } else {
            "taken over".to_owned()
        };
        job.event("takeover", &note);
        Ok(true)
    })?;
    if changed {
        let current = job(&target, fp)?;
        if runner::taking_over(&current)
            && !super::proc_alive(read_procs(&target, fp).supervisor.as_ref())
        {
            runner::stop_taken_over(ctx.settings, &target, fp)
                .map_err(|e| JobError::Conflict(format!("{e:#}")))?;
        }
    }
    let job = difit::stop(&target, fp, ctx.settings.grace)?;
    let commands = resume_commands(ctx, &job);
    Ok((job, commands))
}

/// Return the job to the runner. An unfinished job is queued again to continue
/// the same session; a finished one is not restarted.
pub fn handback(ctx: &Context, target_id: &str, fp: &str) -> JobResult<Job> {
    let target = target(ctx, target_id)?;
    job(&target, fp)?;
    let (job, _) = mutate(&target, fp, |job| {
        if job.owner == Owner::Runner {
            return Ok(());
        }
        if job.stopping.is_some() {
            return Err(JobError::Conflict(
                "the agent is finishing its current step; hand it back once it has stopped".into(),
            ));
        }
        job.owner = Owner::Runner;
        if matches!(job.status, Status::Running | Status::Queued) {
            job.status = Status::Queued;
            job.pid = None;
            let note = match &job.session_id {
                Some(session) => format!("handed back; the runner continues session {session}"),
                None => "handed back; the runner starts it".to_owned(),
            };
            job.event("handback", &note);
        } else {
            job.event(
                "handback",
                "handed back; finished jobs are not restarted without reanalyze",
            );
        }
        Ok(())
    })?;
    Ok(job)
}

/// Remove the worktree (and its branch when it has no commits of its own).
/// A detached job is cleaned up only once every process recorded for it is
/// confirmed gone, or with `force_detached` after the person checked it ended.
pub fn cleanup(
    ctx: &Context,
    target_id: &str,
    fp: &str,
    force_detached: bool,
) -> JobResult<(Job, Vec<String>)> {
    let target = target(ctx, target_id)?;
    let current = job(&target, fp)?;
    let procs = read_procs(&target, fp);
    let live = (current.status == Status::Running && current.owner == Owner::Runner)
        || current.stopping.is_some()
        || difit::running(&target, &current);
    if live {
        return Err(JobError::Conflict(
            "the job's agent or diff viewer is still running; stop it first".into(),
        ));
    }
    let recorded = [procs.agent.as_ref(), procs.supervisor.as_ref()];
    if recorded.into_iter().any(super::proc_alive) {
        return Err(JobError::Conflict(
            "a process recorded for this job is still running; stop it (or wait for it to end) before cleanup".into(),
        ));
    }
    let detached = current.status == Status::Detached;
    let unconfirmed: Vec<String> = recorded
        .into_iter()
        .flatten()
        .filter(|p| process::check(p) != process::Check::Gone)
        .map(|p| p.pid.to_string())
        .collect();
    let herdr_unconfirmed =
        detached && current.launcher == Launcher::Herdr && !herdr_agent_gone(ctx, &current);
    if !force_detached && (!unconfirmed.is_empty() || herdr_unconfirmed) {
        let what = if unconfirmed.is_empty() {
            "the Herdr agent may still be working in its tab".to_owned()
        } else {
            format!(
                "process {} recorded for this job may still be running and its identity cannot be confirmed",
                unconfirmed.join(", ")
            )
        };
        return Err(JobError::Conflict(if detached {
            format!("{what}; once you have checked it has ended, run cleanup again with --force-detached")
        } else {
            format!("{what}; wait for it to end before cleanup")
        }));
    }
    if force_detached && !detached && !unconfirmed.is_empty() {
        return Err(JobError::Conflict(
            "--force-detached applies only to a detached job".into(),
        ));
    }
    let done =
        worktree::cleanup(&target, &current).map_err(|e| JobError::Conflict(format!("{e:#}")))?;
    let (job, ()) = mutate(&target, fp, |job| {
        job.event("cleanup", &done.join("; "));
        if job.status == Status::Detached {
            job.status = Status::Stopped;
            let how = if force_detached {
                "cleaned up with --force-detached after the person checked the agent had ended; it can be analyzed again"
            } else {
                "cleaned up once the agent the runner lost control of was confirmed gone; it can be analyzed again"
            };
            job.event("stopped", how);
        }
        write_procs(&target, fp, |p| {
            p.agent = None;
            p.supervisor = None;
        })?;
        Ok(())
    })?;
    Ok((job, done))
}

/// Herdr confirms the detached agent's pane no longer exists.
fn herdr_agent_gone(ctx: &Context, job: &Job) -> bool {
    let Some(tab) = &job.herdr_tab else {
        return true;
    };
    ctx.settings
        .herdr
        .as_ref()
        .is_some_and(|argv| !super::herdr::Herdr { argv }.pane_exists(&tab.pane_id))
}

fn diagnosis(target: &Target, job: &Job) -> Value {
    let path = diagnosis_path(target, &job.fingerprint);
    let Ok(markdown) = std::fs::read_to_string(&path) else {
        return Value::Null;
    };
    let updated = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
    json!({"markdown": markdown, "updated_at": updated, "sections": super::packet::sections(&markdown)})
}

pub fn show(ctx: &Context, target_id: &str, fp: &str) -> JobResult<Value> {
    let target = target(ctx, target_id)?;
    let job = job(&target, fp)?;
    Ok(json!({
        "job": job,
        "diagnosis": diagnosis(&target, &job),
        "difit_running": difit::running(&target, &job),
        "resume_commands": resume_commands(ctx, &job),
        "takeover_wait_seconds": ctx.settings.takeover_wait.as_secs(),
        "unresolved_processes": unresolved(&target, &job),
    }))
}

/// The processes a detached job may still have running, for the person to check.
fn unresolved(target: &Target, job: &Job) -> Value {
    if job.status != Status::Detached {
        return json!([]);
    }
    let procs = read_procs(target, &job.fingerprint);
    let roles = [("agent", procs.agent), ("supervisor", procs.supervisor)];
    Value::Array(
        roles
            .into_iter()
            .filter_map(|(role, p)| p.map(|p| (role, p)))
            .filter(|(_, p)| process::check(p) != process::Check::Gone)
            .map(|(role, p)| json!({"role": role, "pid": p.pid, "started_at": p.started_at}))
            .collect(),
    )
}

/// Every job on this machine, newest first, optionally for one target.
pub fn list_all(ctx: &Context, only: Option<&str>) -> JobResult<Value> {
    let mut jobs = Vec::new();
    let targets: Vec<Target> = match only {
        Some(id) => vec![target(ctx, id)?],
        None => std::fs::read_dir(ctx.db_root.join("targets"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let id = e.file_name().to_string_lossy().into_owned();
                resolve(ctx.db_root, &id).ok().filter(|t| t.id == id)
            })
            .collect(),
    };
    for target in &targets {
        for job in super::list(target) {
            let running = difit::running(target, &job);
            let mut row = serde_json::to_value(&job).map_err(anyhow::Error::from)?;
            row["difit_running"] = json!(running);
            row["has_diagnosis"] = json!(diagnosis_path(target, &job.fingerprint).is_file());
            jobs.push(row);
        }
    }
    jobs.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    Ok(json!({"jobs": jobs, "generated_at": now(), "runner": runner::last_tick(ctx.db_root)}))
}

pub fn difit_start(ctx: &Context, target_id: &str, fp: &str) -> JobResult<Job> {
    let target = target(ctx, target_id)?;
    job(&target, fp)?;
    difit::start(&target, fp, ctx.settings.difit.as_ref())
}

pub fn difit_stop(ctx: &Context, target_id: &str, fp: &str) -> JobResult<Job> {
    let target = target(ctx, target_id)?;
    job(&target, fp)?;
    difit::stop(&target, fp, ctx.settings.grace)
}
