use super::events::AgentKind;
use arc_core::aggregate::Command;

/// What a new agent generation records at start; the aggregate assigns the generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAgent {
    pub code: String,
    pub agent_kind: AgentKind,
    pub repo: String,
    pub worktree: String,
    pub branch: Option<String>,
    pub created_worktree: bool,
    pub session_id: String,
    pub pid: u32,
    pub started_at: i64,
}

/// A lifecycle change on the live generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentChange {
    EndTurn,
    Resume {
        pid: u32,
    },
    RequestTakeover,
    /// `force` attaches over a running service turn.
    Attach {
        force: bool,
    },
    HandBack,
    Stop,
    Done {
        pr_url: String,
    },
    Fail {
        reason: String,
    },
}

/// Commands on one item's agent; `id` is `<provider>:<item id>`.
pub enum TaskAgentCommand {
    Start {
        id: String,
        start: StartAgent,
    },
    /// `generation` must name the live generation, so a stale caller cannot
    /// change a later agent.
    Change {
        id: String,
        generation: u32,
        at: i64,
        change: AgentChange,
    },
}

impl Command for TaskAgentCommand {
    fn aggregate_id(&self) -> &str {
        match self {
            Self::Start { id, .. } | Self::Change { id, .. } => id,
        }
    }
}
