//! Process identity for the runner. A recorded process is signalled only when
//! its pid, process group, OS start time and exact executable path all still
//! match what the OS reported right after it started, and it leads its own
//! process group. Anything else fails closed: no signal is sent.

use super::Proc;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return !zombie(pid);
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// An exited child nobody has reaped still answers signal 0.
fn zombie(pid: libc::pid_t) -> bool {
    Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim_start()
                .starts_with('Z')
        })
}

/// What the OS says about a live process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsIdentity {
    pub start: String,
    pub pgid: u32,
    pub exe: String,
}

#[cfg(target_os = "macos")]
pub fn os_identity(pid: u32) -> Option<OsIdentity> {
    let pid = libc::c_int::try_from(pid).ok().filter(|p| *p > 0)?;
    // SAFETY: proc_bsdinfo is plain data; zeroed is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: the buffer is a proc_bsdinfo of exactly `size` bytes.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    let mut path = vec![0u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
    // SAFETY: the buffer holds PROC_PIDPATHINFO_MAXSIZE bytes.
    let len = unsafe {
        libc::proc_pidpath(
            pid,
            path.as_mut_ptr().cast(),
            u32::try_from(path.len()).ok()?,
        )
    };
    path.truncate(usize::try_from(len).ok().filter(|l| *l > 0)?);
    Some(OsIdentity {
        start: format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        pgid: info.pbi_pgid,
        exe: String::from_utf8(path).ok()?,
    })
}

#[cfg(target_os = "linux")]
pub fn os_identity(pid: u32) -> Option<OsIdentity> {
    if pid == 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesised command name: state ppid pgrp ... starttime (field 22).
    let fields: Vec<&str> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(OsIdentity {
        start: (*fields.get(19)?).to_owned(),
        pgid: fields.get(2)?.parse().ok()?,
        exe: exe.to_str()?.to_owned(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn os_identity(_pid: u32) -> Option<OsIdentity> {
    None
}

/// The record for a process just started. An interpreter or `env` shim may
/// exec into its real program right after the start, so the executable is
/// read until it holds still (at most half a second).
pub fn capture(pid: u32) -> Proc {
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut seen = os_identity(pid);
    loop {
        std::thread::sleep(Duration::from_millis(60));
        let again = os_identity(pid);
        if again == seen || Instant::now() >= deadline {
            seen = again;
            break;
        }
        seen = again;
    }
    let identity = seen.unwrap_or(OsIdentity {
        start: String::new(),
        pgid: 0,
        exe: String::new(),
    });
    Proc {
        pid,
        pgid: identity.pgid,
        os_start: identity.start,
        exe: identity.exe,
        started_at: super::now(),
    }
}

/// Whether a recorded process is still the one that was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Same,
    /// It ended: the pid is free, or a different process (another start time) holds it.
    Gone,
    /// The pid is alive but cannot be confirmed as ours; it must not be signalled.
    Unverified(String),
}

pub fn check(proc: &Proc) -> Check {
    if !alive(proc.pid) {
        return Check::Gone;
    }
    if proc.os_start.is_empty() || proc.exe.is_empty() || proc.pgid == 0 {
        return Check::Unverified(format!("process {} has no recorded OS identity", proc.pid));
    }
    let Some(now) = os_identity(proc.pid) else {
        if !alive(proc.pid) {
            return Check::Gone;
        }
        return Check::Unverified(format!(
            "the OS identity of process {} cannot be read",
            proc.pid
        ));
    };
    if now.start != proc.os_start {
        return Check::Gone;
    }
    if now.pgid != proc.pgid {
        return Check::Unverified(format!(
            "process {} moved from process group {} to {}",
            proc.pid, proc.pgid, now.pgid
        ));
    }
    if now.exe != proc.exe {
        return Check::Unverified(format!(
            "process {} now runs {} instead of {}",
            proc.pid, now.exe, proc.exe
        ));
    }
    Check::Same
}

fn signal_group(pgid: u32, signal: libc::c_int) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        if pgid > 1 {
            // SAFETY: a negative pid addresses the process group the job started.
            unsafe {
                libc::kill(-pgid, signal);
            }
        }
    }
}

/// What `terminate` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminated {
    Stopped,
    /// Nothing to do: the process had already ended.
    Gone,
    /// Not signalled, because its identity could not be confirmed.
    Refused(String),
}

/// TERM the recorded process group, then KILL it if it is still there after
/// `grace`. The identity is checked before each signal.
pub fn terminate(proc: &Proc, grace: Duration) -> Terminated {
    match check(proc) {
        Check::Gone => return Terminated::Gone,
        Check::Unverified(why) => return Terminated::Refused(why),
        Check::Same => {}
    }
    if proc.pgid != proc.pid {
        return Terminated::Refused(format!(
            "process {} does not lead its own process group",
            proc.pid
        ));
    }
    signal_group(proc.pgid, libc::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        // A process that is exiting can briefly report no identity; keep waiting.
        if check(proc) == Check::Gone {
            return Terminated::Stopped;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    match check(proc) {
        Check::Same => {
            signal_group(proc.pgid, libc::SIGKILL);
            // KILL is asynchronous: wait for the process to actually exit.
            let kill_deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < kill_deadline {
                if check(proc) == Check::Gone {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Terminated::Stopped
        }
        Check::Gone => Terminated::Stopped,
        Check::Unverified(why) => Terminated::Refused(format!("after TERM, {why}")),
    }
}

/// TERM, then KILL, the group of a child this process started and has not
/// reaped. The unreaped child keeps its pid and group id reserved, so the
/// handle itself is the identity.
pub fn terminate_child(child: &mut Child, grace: Duration) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let pid = child.id();
    let leads_group = os_identity(pid).is_some_and(|i| i.pgid == pid);
    if leads_group {
        signal_group(pid, libc::SIGTERM);
    } else {
        let _ = child.kill();
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if leads_group {
        signal_group(pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
