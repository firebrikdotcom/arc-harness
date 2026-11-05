//! The diff viewer of a job's worktree:
//! `npx difit . <base> --no-open --port P --host 127.0.0.1 --keep-alive`.
//! difit binds every interface unless told otherwise, so `--host 127.0.0.1`
//! is always passed. The port is picked by binding 127.0.0.1:0 and released
//! just before the launch; a launch that dies at once is retried on a new port.

use super::{log_path, mutate, process, worktree, write_procs, Job, JobError, JobResult};
use crate::store::Target;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ATTEMPTS: usize = 5;

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
}

fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(200),
    )
    .is_ok()
}

pub fn running(target: &Target, job: &Job) -> bool {
    let procs = super::read_procs(target, &job.fingerprint);
    job.difit.pid.is_some()
        && procs
            .difit
            .as_ref()
            .is_some_and(|p| Some(p.pid) == job.difit.pid)
        && super::proc_alive(procs.difit.as_ref())
}

pub fn start(target: &Target, fp: &str, argv: Option<&Vec<String>>) -> JobResult<Job> {
    let job = super::load(target, fp)?.ok_or_else(|| JobError::NotFound(format!("no job {fp}")))?;
    if running(target, &job) {
        return Ok(job);
    }
    let path = worktree::checked_path(target, &job)?;
    if !path.is_dir() {
        return Err(JobError::Conflict("the job has no worktree yet".into()));
    }
    let argv = argv.ok_or_else(|| JobError::Invalid("difit needs npx: install Node.js".into()))?;
    let mut last = String::from("difit did not start");
    for _ in 0..ATTEMPTS {
        let Some(port) = free_port() else {
            continue;
        };
        if listening(port) {
            continue;
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path(target, fp))
            .map_err(anyhow::Error::from)?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .args([
                ".",
                job.worktree.base_branch.as_str(),
                "--no-open",
                "--port",
                &port.to_string(),
                "--host",
                "127.0.0.1",
                "--keep-alive",
            ])
            .current_dir(&path)
            .env(
                "PATH",
                super::settings::child_path(std::path::Path::new(&argv[0]).parent()),
            )
            .env("HOST", "127.0.0.1")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(anyhow::Error::from)?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                last = format!("difit could not start: {error}");
                break;
            }
        };
        let pid = child.id();
        // A difit that exits within a moment lost the port race; try another.
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut exited = false;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                exited = true;
                break;
            }
            if listening(port) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if exited {
            last = "difit exited right after starting".into();
            continue;
        }
        let proc = process::capture(pid);
        let started = proc.started_at.clone();
        let (job, ()) = mutate(target, fp, |job| {
            job.difit = super::Difit {
                port: Some(port),
                pid: Some(pid),
                started_at: Some(started.clone()),
            };
            write_procs(target, fp, |p| p.difit = Some(proc.clone()))?;
            job.event("difit", &format!("diff viewer started on 127.0.0.1:{port}"));
            Ok(())
        })?;
        drop(child);
        return Ok(job);
    }
    Err(JobError::Failed(last))
}

/// Stop the viewer and clear its fields. Stopping a stopped viewer is a no-op.
pub fn stop(target: &Target, fp: &str, grace: Duration) -> JobResult<Job> {
    let procs = super::read_procs(target, fp);
    let refused = match procs.difit.as_ref().map(|p| process::terminate(p, grace)) {
        Some(process::Terminated::Refused(why)) => Some(why),
        _ => None,
    };
    let (job, ()) = mutate(target, fp, |job| {
        let had = job.difit.pid.is_some();
        let port = job.difit.port;
        job.difit = super::Difit::default();
        write_procs(target, fp, |p| p.difit = None)?;
        if let Some(why) = &refused {
            job.event(
                "difit",
                &format!(
                    "the diff viewer was not signalled because {why}; it may still serve 127.0.0.1:{}, stop it yourself",
                    port.unwrap_or_default()
                ),
            );
        } else if had {
            job.event("difit", "diff viewer stopped");
        }
        Ok(())
    })?;
    Ok(job)
}
