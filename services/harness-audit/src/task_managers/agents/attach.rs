//! Take an agent's session over: request takeover (the supervisor then
//! leaves it alone), wait for the headless turn to end, record the attach,
//! and give the command that resumes the session interactively.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{process, resume_commands, ResumeCommands, Runtime};
use crate::domain::task_agent::commands::AgentChange;
use crate::domain::task_agent::events::{AgentKind, AgentState};
use crate::task_managers::{AgentRecord, TaskError, TaskManagers};

#[derive(Debug, Clone, Serialize)]
pub struct AttachPlan {
    pub record: AgentRecord,
    /// What `attach --print` shows.
    pub commands: ResumeCommands,
    /// What `attach` execs: program, arguments and working directory.
    #[serde(skip)]
    pub program: OsString,
    #[serde(skip)]
    pub args: Vec<String>,
    #[serde(skip)]
    pub cwd: PathBuf,
}

/// Attach to the live agent holding `code`. Waits up to `rt.takeover_wait`
/// for a running headless turn; then, with `force`, terminates its process
/// group, otherwise refuses (`Conflict`) and leaves the takeover requested.
/// Attaching an agent you already hold just returns the commands again.
pub async fn attach(
    tm: &TaskManagers,
    rt: &Runtime,
    code: &str,
    force: bool,
) -> Result<AttachPlan, TaskError> {
    tm.refresh()?;
    let mut record = tm.live_by_code(code)?;
    if matches!(record.state, AgentState::Running | AgentState::Waiting) {
        record = tm.change_agent(code, AgentChange::RequestTakeover).await?;
    }
    if record.state == AgentState::TakeoverRequested {
        let deadline = Instant::now() + rt.takeover_wait;
        while process::alive(record.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if process::alive(record.pid) {
            if !force {
                return Err(TaskError::Conflict(format!(
                    "the headless turn is still running after {}s; retry, or pass --force to stop it",
                    rt.takeover_wait.as_secs()
                )));
            }
            process::terminate_group(record.pid, Duration::from_secs(5));
        }
        record = tm
            .change_agent(code, AgentChange::Attach { force: false })
            .await?;
    }
    let config_ssh = rt.ssh.as_deref();
    let (program, args) = match record.agent_kind {
        AgentKind::Claude => (
            rt.claude.clone(),
            vec!["--resume".to_string(), record.session_id.clone()],
        ),
        AgentKind::Codex => (
            rt.codex.clone(),
            vec![
                "resume".to_string(),
                record.session_id.clone(),
                "--cd".to_string(),
                record.worktree.clone(),
            ],
        ),
    };
    Ok(AttachPlan {
        commands: resume_commands(&record, config_ssh),
        cwd: PathBuf::from(&record.worktree),
        program,
        args,
        record,
    })
}

/// Replace this process with the interactive session (`attach` without `--print`).
pub fn exec(plan: &AttachPlan) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(&plan.program);
    command.args(&plan.args).current_dir(&plan.cwd);
    super::scrub(&mut command);
    command.exec()
}
