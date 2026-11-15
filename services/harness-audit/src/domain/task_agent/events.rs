use serde::{Deserialize, Serialize};

pub const STARTED: &str = "TaskAgentStarted";
pub const TURN_ENDED: &str = "TaskAgentTurnEnded";
pub const RESUMED: &str = "TaskAgentResumed";
pub const TAKEOVER_REQUESTED: &str = "TaskAgentTakeoverRequested";
pub const ATTACHED: &str = "TaskAgentAttached";
pub const HANDED_BACK: &str = "TaskAgentHandedBack";
pub const STOPPED: &str = "TaskAgentStopped";
pub const DONE: &str = "TaskAgentDone";
pub const FAILED: &str = "TaskAgentFailed";

/// Every event type the TaskAgent aggregate emits.
pub const ALL: &[&str] = &[
    STARTED,
    TURN_ENDED,
    RESUMED,
    TAKEOVER_REQUESTED,
    ATTACHED,
    HANDED_BACK,
    STOPPED,
    DONE,
    FAILED,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// Who may drive the agent's session. The service supervisor may signal or
/// resume only an agent it owns; it never touches one owned by `You`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOwner {
    Service,
    You,
}

/// Lifecycle state of one agent generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// A service-driven turn is in progress.
    Running,
    /// Between turns; the service owns the idle session.
    Waiting,
    /// You asked to attach; the supervisor must release the session and not resume it.
    TakeoverRequested,
    /// You hold the session (`claude --resume` / `codex resume`).
    Attached,
    Stopped,
    Done,
    Failed,
}

impl AgentState {
    pub fn is_live(self) -> bool {
        !matches!(self, Self::Stopped | Self::Done | Self::Failed)
    }

    /// `None` once the generation has ended.
    pub fn owner(self) -> Option<AgentOwner> {
        match self {
            Self::Attached => Some(AgentOwner::You),
            Self::Running | Self::Waiting | Self::TakeoverRequested => Some(AgentOwner::Service),
            Self::Stopped | Self::Done | Self::Failed => None,
        }
    }

    /// The supervisor may start another turn only from `Waiting`.
    pub fn supervisor_may_resume(self) -> bool {
        self == Self::Waiting
    }

    /// The supervisor may signal its own child process only while it owns the session.
    pub fn supervisor_may_signal(self) -> bool {
        self.owner() == Some(AgentOwner::Service)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::TakeoverRequested => "takeover_requested",
            Self::Attached => "attached",
            Self::Stopped => "stopped",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Running,
            Self::Waiting,
            Self::TakeoverRequested,
            Self::Attached,
            Self::Stopped,
            Self::Done,
            Self::Failed,
        ]
        .into_iter()
        .find(|state| state.as_str() == value)
    }

    /// The state an event of this type leaves the generation in, given the current one.
    pub fn after(self, event_type: &str) -> Self {
        match event_type {
            TURN_ENDED if self == Self::Running => Self::Waiting,
            RESUMED => Self::Running,
            TAKEOVER_REQUESTED => Self::TakeoverRequested,
            ATTACHED => Self::Attached,
            HANDED_BACK => Self::Waiting,
            STOPPED => Self::Stopped,
            DONE => Self::Done,
            FAILED => Self::Failed,
            _ => self,
        }
    }
}

/// Payload of `TaskAgentStarted`. Ids, names, paths and codes only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAgentStarted {
    pub generation: u32,
    pub code: String,
    pub agent_kind: AgentKind,
    pub repo: String,
    pub worktree: String,
    pub branch: Option<String>,
    pub created_worktree: bool,
    pub session_id: String,
    pub pid: u32,
    /// Unix seconds.
    pub started_at: i64,
}

/// Payload of `TurnEnded`, `TakeoverRequested`, `HandedBack` and `Stopped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAgentStep {
    pub generation: u32,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAgentResumed {
    pub generation: u32,
    pub pid: u32,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAgentAttached {
    pub generation: u32,
    /// True when you attached over a running service turn.
    pub forced: bool,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAgentDone {
    pub generation: u32,
    pub pr_url: String,
    pub at: i64,
}

/// `reason` is service-written (an error class or short message), never item text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAgentFailed {
    pub generation: u32,
    pub reason: String,
    pub at: i64,
}
