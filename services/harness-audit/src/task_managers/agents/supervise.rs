//! The supervisor: notices when a service-owned headless turn has ended and
//! records it. It never signals or resumes anything, and it never touches an
//! agent you hold (attached) or one waiting for your takeover.

use super::{agent_dir, process, Runtime};
use crate::domain::task_agent::commands::AgentChange;
use crate::domain::task_agent::events::{AgentOwner, AgentState};
use crate::task_managers::{AgentRecord, TaskError, TaskManagers};

/// A fixed failure reason when the turn's own output reports an error; the
/// output itself (which may quote item text) is never recorded.
pub(crate) fn turn_failure(log: &str) -> Option<&'static str> {
    for line in log.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if event["is_error"] == true
            || event["subtype"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("error"))
        {
            return Some("the headless turn ended with an error reported by claude");
        }
        if matches!(event["type"].as_str(), Some("turn.failed" | "error")) {
            return Some("the headless turn ended with an error reported by codex");
        }
    }
    None
}

fn turn_change(rt: &Runtime, agent: &AgentRecord) -> AgentChange {
    let log = std::fs::read_to_string(agent_dir(rt, &agent.item_id, &agent.code).join("turn.log"))
        .unwrap_or_default();
    match turn_failure(&log) {
        Some(reason) => AgentChange::Fail {
            reason: reason.into(),
        },
        None => AgentChange::EndTurn,
    }
}

/// One pass over live agents; returns how many changed. Only `Running`
/// agents the service owns are looked at: when their process group is gone
/// (exited, or the pid no longer leads its own group), the turn is recorded
/// as ended, or failed when its output says so.
pub async fn supervise_once(tm: &TaskManagers, rt: &Runtime) -> Result<usize, TaskError> {
    tm.refresh()?;
    let agents = tm
        .store()
        .agents()
        .map_err(|e| TaskError::Internal(e.to_string()))?;
    let mut changed = 0;
    for agent in agents {
        if agent.owner != Some(AgentOwner::Service) || agent.state != AgentState::Running {
            continue;
        }
        if process::alive(agent.pid) {
            continue;
        }
        // A conflict means someone else moved it first (attach, stop); skip.
        match tm.change_agent(&agent.code, turn_change(rt, &agent)).await {
            Ok(_) => changed += 1,
            Err(TaskError::Conflict(_) | TaskError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(changed)
}
