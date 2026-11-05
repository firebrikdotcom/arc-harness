//! Observation: every runner tick (and once at service start) looks at each
//! running job, whoever owns it. It records the session transcript's last
//! write as `last_activity_at` and the look itself as `observed_at`.
//!
//! A job the runner supervises is left to its supervisor (and `recover`). For
//! any other running job (taken over, owner `you`) observation closes the loop
//! when the agent is over: `diagnosed` when the transcript holds a valid
//! diagnosis, else `ended`. It only reads: it never signals a process, stops,
//! closes a tab or resumes a session. A recorded process whose identity cannot
//! be verified only ever moves the job to `detached`.

use super::herdr::{self, Herdr};
use super::settings::Settings;
use super::{
    agent, diagnosis_path, list, mutate_observed, now, packet, process, read_procs, write_procs,
    Job, Launcher, Owner, Status,
};
use crate::store::{write_private, Target};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How much of a transcript's end is read for its last replies.
const TRANSCRIPT_TAIL: u64 = 8 << 20;

/// The end of a file, starting at a line boundary, at most `limit` bytes.
pub fn tail(path: &Path, limit: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let skip = len.saturating_sub(limit);
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(skip)).ok()?;
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if skip > 0 {
        text.split_once('\n')
            .map_or(String::new(), |(_, rest)| rest.to_owned())
    } else {
        text
    })
}

/// What the job's session transcript shows.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub modified: SystemTime,
    /// The last event is a finished reply with no tool call open.
    pub boundary: bool,
    /// `None` when no reply carries the diagnosis markers.
    pub diagnosis: Option<Result<packet::Diagnosis, String>>,
}

pub fn transcript_path(settings: &Settings, job: &Job) -> Option<PathBuf> {
    let session = job.session_id.as_deref()?;
    agent::transcript(
        job.agent,
        &settings.home,
        Path::new(&job.worktree.path),
        session,
    )
}

pub fn read_transcript(settings: &Settings, job: &Job) -> Option<Transcript> {
    let path = transcript_path(settings, job)?;
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let text = tail(&path, TRANSCRIPT_TAIL)?;
    let replies = agent::replies(&text);
    let diagnosis = replies
        .iter()
        .any(|r| r.contains(packet::START))
        .then(|| packet::parse(&replies).map_err(|e| format!("{e:#}")));
    Some(Transcript {
        modified,
        boundary: agent::step_boundary(&text),
        diagnosis,
    })
}

pub fn stamp(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn parse_stamp(at: Option<&str>) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(at?)
        .ok()
        .map(SystemTime::from)
}

/// Where the job's agent is, as far as reading can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// A verified live agent process, or a Herdr agent in this state.
    Alive { settled: bool },
    /// The agent is gone; how that was seen.
    Gone(String),
    /// Nothing runs for the job: a taken-over background agent was stopped.
    NoAgent,
    /// It cannot be told this time; nothing changes.
    Unknown(String),
    /// A recorded process is alive but not verifiably the one started.
    Unverified(String),
}

#[derive(Debug, Clone)]
pub enum Verdict {
    Running,
    Ended(String),
    Diagnosed(packet::Diagnosis, String),
    Detach(String),
}

fn minutes(quiet: Duration) -> u64 {
    (quiet.as_secs() / 60).max(1)
}

/// The decision, given what was observed. `quiet` is how long the session has
/// not been written to (or, without a transcript, since the job's last activity).
pub fn decide(
    presence: &Presence,
    transcript: Option<&Transcript>,
    quiet: Option<Duration>,
    idle_end: Duration,
) -> Verdict {
    let diagnosis = transcript.and_then(|t| t.diagnosis.clone());
    let finished = transcript.is_some_and(|t| t.boundary);
    let long_quiet = quiet.filter(|q| *q >= idle_end);
    match presence {
        Presence::Unverified(why) => Verdict::Detach(format!(
            "observation cannot verify the agent process ({why}), so nothing was signalled; it may still be running"
        )),
        Presence::Unknown(_) => Verdict::Running,
        Presence::Gone(how) => match diagnosis {
            Some(Ok(d)) => Verdict::Diagnosed(
                d,
                format!("{how}; its diagnosis was read from the session transcript"),
            ),
            Some(Err(why)) => Verdict::Ended(format!(
                "{how}; the diagnosis in its transcript is not valid ({why})"
            )),
            None => Verdict::Ended(format!("{how}; the session holds no diagnosis")),
        },
        Presence::Alive { settled: false } => Verdict::Running,
        Presence::Alive { settled: true } | Presence::NoAgent => {
            let idle = if *presence == Presence::NoAgent {
                "no agent process runs for this job"
            } else {
                "the agent is idle in Herdr"
            };
            if let (true, Some(Ok(d))) = (finished, &diagnosis) {
                return Verdict::Diagnosed(
                    d.clone(),
                    format!("{idle} after a reply with a diagnosis; it was read from the session transcript"),
                );
            }
            let ready = finished || *presence == Presence::NoAgent;
            match long_quiet {
                Some(q) if ready => Verdict::Ended(format!(
                    "{idle} and its session has had no activity for {} min",
                    minutes(q)
                )),
                _ => Verdict::Running,
            }
        }
    }
}

/// Where the agent of a job nobody supervises is: its recorded processes by
/// verified identity, else Herdr, else nothing.
fn presence(settings: &Settings, target: &Target, job: &Job) -> Presence {
    let procs = read_procs(target, &job.fingerprint);
    let mut gone = None;
    for (role, proc) in [("supervisor", &procs.supervisor), ("agent", &procs.agent)] {
        let Some(proc) = proc else { continue };
        match process::check(proc) {
            process::Check::Unverified(why) => return Presence::Unverified(why),
            process::Check::Same if role == "agent" => return Presence::Alive { settled: false },
            process::Check::Same => {
                return Presence::Unknown("its supervisor is still letting go".into())
            }
            process::Check::Gone if role == "agent" => {
                gone = Some(format!("the agent process {} has exited", proc.pid));
            }
            process::Check::Gone => {}
        }
    }
    if let (Launcher::Herdr, Some(tab)) = (job.launcher, &job.herdr_tab) {
        let Some(argv) = settings.herdr.as_ref() else {
            return Presence::Unknown("herdr is not installed on this machine".into());
        };
        return match (Herdr { argv }).presence(&job.herdr_name(), &tab.pane_id, job.agent) {
            herdr::Presence::Present(state) => Presence::Alive {
                settled: matches!(state.as_str(), "idle" | "done"),
            },
            herdr::Presence::Gone(how) => Presence::Gone(how),
            herdr::Presence::Unknown(why) => Presence::Unknown(why),
        };
    }
    gone.map_or(Presence::NoAgent, Presence::Gone)
}

/// The runner, not observation, decides how a job it supervises ends.
fn closable(job: &Job) -> bool {
    job.status == Status::Running && job.owner == Owner::You && job.stopping.is_none()
}

/// Look at every running job of a target. Returns the jobs whose status changed.
pub fn observe(target: &Target, settings: &Settings) -> Vec<Value> {
    let mut changed = Vec::new();
    for job in list(target) {
        if job.status != Status::Running {
            continue;
        }
        let fp = job.fingerprint.clone();
        let transcript = read_transcript(settings, &job);
        let verdict = if closable(&job) {
            let presence = presence(settings, target, &job);
            let since = transcript
                .as_ref()
                .map(|t| t.modified)
                .or_else(|| parse_stamp(job.last_activity_at.as_deref()))
                .or_else(|| parse_stamp(Some(&job.updated_at)));
            let quiet = since.and_then(|t| t.elapsed().ok());
            decide(&presence, transcript.as_ref(), quiet, settings.idle_end)
        } else {
            Verdict::Running
        };
        let result = mutate_observed(target, &fp, |current| {
            if current.status != Status::Running {
                return Ok(None);
            }
            if let Some(t) = &transcript {
                current.last_activity_at = Some(stamp(t.modified));
            }
            current.observed_at = Some(now());
            if !closable(current) || current.owner != job.owner {
                return Ok(None);
            }
            let status = match &verdict {
                Verdict::Running => return Ok(None),
                Verdict::Ended(how) => {
                    current.end(how);
                    "ended"
                }
                Verdict::Detach(why) => {
                    current.detach(why);
                    "detached"
                }
                Verdict::Diagnosed(diagnosis, how) => {
                    let at = now();
                    write_private(
                        &diagnosis_path(target, &fp),
                        packet::render(current, diagnosis, &at).as_bytes(),
                    )?;
                    current.status = Status::Diagnosed;
                    current.finished_at = Some(at);
                    current.pid = None;
                    current.error = None;
                    current.event("diagnosed", how);
                    "diagnosed"
                }
            };
            if status != "detached" {
                write_procs(target, &fp, |p| {
                    let gone = |p: &Option<super::Proc>| {
                        p.as_ref()
                            .is_some_and(|p| process::check(p) == process::Check::Gone)
                    };
                    if gone(&p.agent) {
                        p.agent = None;
                    }
                    if gone(&p.supervisor) {
                        p.supervisor = None;
                    }
                })?;
            }
            Ok(Some(status))
        });
        match result {
            Ok((_, Some(status))) => changed.push(json!({"job": fp, "status": status})),
            Ok(_) => {}
            Err(error) => crate::poll::log(&format!(
                "{}: could not observe job {}: {error}",
                target.id,
                &fp[..8]
            )),
        }
    }
    changed
}
