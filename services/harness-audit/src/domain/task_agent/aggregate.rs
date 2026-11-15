use super::commands::{AgentChange, StartAgent, TaskAgentCommand};
use super::events::{
    AgentState, TaskAgentAttached, TaskAgentDone, TaskAgentFailed, TaskAgentResumed,
    TaskAgentStarted, TaskAgentStep, ATTACHED, DONE, FAILED, HANDED_BACK, RESUMED, STARTED,
    STOPPED, TAKEOVER_REQUESTED, TURN_ENDED,
};
use crate::task_managers::valid_code;
use arc_core::aggregate::Aggregate;
use arc_core::event::{Event, NewEvent};
use async_trait::async_trait;
use thiserror::Error;

/// Longest failure reason accepted.
pub const MAX_REASON: usize = 500;

#[derive(Debug, Error)]
pub enum TaskAgentError {
    #[error("the item already has a live agent (generation {0})")]
    AlreadyLive(u32),
    #[error("the item has no live agent")]
    NotLive,
    #[error("generation {given} is not the live generation {live}")]
    StaleGeneration { given: u32, live: u32 },
    #[error("cannot {action} an agent that is {state}")]
    InvalidTransition {
        action: &'static str,
        state: &'static str,
    },
    #[error("invalid start: {0}")]
    InvalidStart(&'static str),
    #[error("invalid change: {0}")]
    InvalidChange(&'static str),
    #[error("could not serialize event payload: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// One item's agents: successive generations, at most one live.
#[derive(Default)]
pub struct TaskAgentAggregate {
    pub version: i64,
    /// The latest generation started (0 before the first).
    pub generation: u32,
    /// State of the latest generation.
    pub state: Option<AgentState>,
    pub code: Option<String>,
}

impl TaskAgentAggregate {
    pub fn live_state(&self) -> Option<AgentState> {
        self.state.filter(|state| state.is_live())
    }

    fn event(&self, id: &str, event_type: &str, payload: serde_json::Value) -> Event {
        Event::new(NewEvent {
            aggregate_type: Self::aggregate_type(),
            aggregate_id: id,
            sequence: self.version + 1,
            event_type,
            payload,
        })
    }

    fn start(&self, id: &str, start: StartAgent) -> Result<Vec<Event>, TaskAgentError> {
        if self.live_state().is_some() {
            return Err(TaskAgentError::AlreadyLive(self.generation));
        }
        if !valid_code(&start.code) {
            return Err(TaskAgentError::InvalidStart(
                "code must be tm- and 6 hex digits",
            ));
        }
        if start.repo.trim().is_empty() {
            return Err(TaskAgentError::InvalidStart("repo cannot be empty"));
        }
        if !start.worktree.starts_with('/') {
            return Err(TaskAgentError::InvalidStart("worktree must be absolute"));
        }
        if start.session_id.trim().is_empty() {
            return Err(TaskAgentError::InvalidStart("session id cannot be empty"));
        }
        if start.branch.as_deref().is_some_and(|b| b.trim().is_empty()) {
            return Err(TaskAgentError::InvalidStart("branch cannot be blank"));
        }
        let payload = TaskAgentStarted {
            generation: self.generation + 1,
            code: start.code,
            agent_kind: start.agent_kind,
            repo: start.repo,
            worktree: start.worktree,
            branch: start.branch,
            created_worktree: start.created_worktree,
            session_id: start.session_id,
            pid: start.pid,
            started_at: start.started_at,
        };
        Ok(vec![self.event(
            id,
            STARTED,
            serde_json::to_value(payload)?,
        )])
    }

    fn change(
        &self,
        id: &str,
        generation: u32,
        at: i64,
        change: AgentChange,
    ) -> Result<Vec<Event>, TaskAgentError> {
        use AgentState::*;
        let state = self.live_state().ok_or(TaskAgentError::NotLive)?;
        if generation != self.generation {
            return Err(TaskAgentError::StaleGeneration {
                given: generation,
                live: self.generation,
            });
        }
        let invalid = |action| TaskAgentError::InvalidTransition {
            action,
            state: state.as_str(),
        };
        let step = || serde_json::to_value(TaskAgentStep { generation, at });
        let (event_type, payload) = match change {
            AgentChange::EndTurn => match state {
                Running | TakeoverRequested => (TURN_ENDED, step()?),
                _ => return Err(invalid("end the turn of")),
            },
            AgentChange::Resume { pid } => match state {
                Waiting => (
                    RESUMED,
                    serde_json::to_value(TaskAgentResumed {
                        generation,
                        pid,
                        at,
                    })?,
                ),
                _ => return Err(invalid("resume")),
            },
            AgentChange::RequestTakeover => match state {
                Running | Waiting => (TAKEOVER_REQUESTED, step()?),
                TakeoverRequested => return Ok(vec![]),
                _ => return Err(invalid("request takeover of")),
            },
            AgentChange::Attach { force } => match state {
                Waiting | TakeoverRequested => (
                    ATTACHED,
                    serde_json::to_value(TaskAgentAttached {
                        generation,
                        forced: false,
                        at,
                    })?,
                ),
                Running if force => (
                    ATTACHED,
                    serde_json::to_value(TaskAgentAttached {
                        generation,
                        forced: true,
                        at,
                    })?,
                ),
                _ => return Err(invalid("attach")),
            },
            AgentChange::HandBack => match state {
                Attached => (HANDED_BACK, step()?),
                _ => return Err(invalid("hand back")),
            },
            AgentChange::Stop => (STOPPED, step()?),
            AgentChange::Done { pr_url } => {
                if !pr_url.starts_with("https://") || pr_url.len() > 500 {
                    return Err(TaskAgentError::InvalidChange("pr_url must be an https URL"));
                }
                (
                    DONE,
                    serde_json::to_value(TaskAgentDone {
                        generation,
                        pr_url,
                        at,
                    })?,
                )
            }
            AgentChange::Fail { reason } => {
                if reason.trim().is_empty() || reason.len() > MAX_REASON {
                    return Err(TaskAgentError::InvalidChange("reason must be 1-500 bytes"));
                }
                (
                    FAILED,
                    serde_json::to_value(TaskAgentFailed {
                        generation,
                        reason,
                        at,
                    })?,
                )
            }
        };
        Ok(vec![self.event(id, event_type, payload)])
    }
}

#[async_trait]
impl Aggregate for TaskAgentAggregate {
    type Command = TaskAgentCommand;
    type Event = ();
    type Error = TaskAgentError;

    fn aggregate_type() -> &'static str {
        "TaskAgent"
    }

    fn version(&self) -> i64 {
        self.version
    }

    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
        match command {
            TaskAgentCommand::Start { id, start } => self.start(&id, start),
            TaskAgentCommand::Change {
                id,
                generation,
                at,
                change,
            } => self.change(&id, generation, at, change),
        }
    }

    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
        if event.event_type == STARTED {
            self.generation = event.payload["generation"]
                .as_u64()
                .and_then(|g| u32::try_from(g).ok())
                .unwrap_or(self.generation + 1);
            self.code = event.payload["code"].as_str().map(str::to_string);
            self.state = Some(AgentState::Running);
        } else if let Some(state) = self.state {
            self.state = Some(state.after(&event.event_type));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::task_agent::events::{AgentKind, AgentOwner};

    const ID: &str = "basecamp:7";

    async fn run(
        state: &mut TaskAgentAggregate,
        command: TaskAgentCommand,
    ) -> Result<Vec<Event>, TaskAgentError> {
        let events = state.handle(command).await?;
        for event in &events {
            state.apply(event);
        }
        Ok(events)
    }

    fn start(code: &str) -> TaskAgentCommand {
        TaskAgentCommand::Start {
            id: ID.into(),
            start: StartAgent {
                code: code.into(),
                agent_kind: AgentKind::Claude,
                repo: "app".into(),
                worktree: "/tmp/wt".into(),
                branch: Some("tm/7".into()),
                created_worktree: true,
                session_id: "sess-1".into(),
                pid: 100,
                started_at: 1,
            },
        }
    }

    fn change(generation: u32, change: AgentChange) -> TaskAgentCommand {
        TaskAgentCommand::Change {
            id: ID.into(),
            generation,
            at: 2,
            change,
        }
    }

    #[tokio::test]
    async fn second_live_agent_refused_until_first_ends() {
        let mut agent = TaskAgentAggregate::default();
        let events = run(&mut agent, start("tm-00aa11")).await.unwrap();
        assert_eq!(events[0].payload["generation"], 1);
        assert_eq!(events[0].payload["session_id"], "sess-1");
        assert!(matches!(
            run(&mut agent, start("tm-00aa12")).await,
            Err(TaskAgentError::AlreadyLive(1))
        ));
        run(&mut agent, change(1, AgentChange::Stop)).await.unwrap();
        let events = run(&mut agent, start("tm-00aa12")).await.unwrap();
        assert_eq!(events[0].payload["generation"], 2);
        assert!(matches!(
            run(&mut agent, change(1, AgentChange::Stop)).await,
            Err(TaskAgentError::StaleGeneration { given: 1, live: 2 })
        ));
    }

    #[tokio::test]
    async fn ownership_follows_takeover_attach_and_handback() {
        let mut agent = TaskAgentAggregate::default();
        run(&mut agent, start("tm-abcdef")).await.unwrap();
        assert!(matches!(
            run(&mut agent, change(1, AgentChange::Attach { force: false })).await,
            Err(TaskAgentError::InvalidTransition { .. })
        ));
        run(&mut agent, change(1, AgentChange::RequestTakeover))
            .await
            .unwrap();
        let state = agent.state.unwrap();
        assert_eq!(state.owner(), Some(AgentOwner::Service));
        assert!(!state.supervisor_may_resume());
        run(&mut agent, change(1, AgentChange::EndTurn))
            .await
            .unwrap();
        assert_eq!(agent.state, Some(AgentState::TakeoverRequested));
        run(&mut agent, change(1, AgentChange::Attach { force: false }))
            .await
            .unwrap();
        let state = agent.state.unwrap();
        assert_eq!(state.owner(), Some(AgentOwner::You));
        assert!(!state.supervisor_may_signal());
        assert!(matches!(
            run(&mut agent, change(1, AgentChange::Resume { pid: 5 })).await,
            Err(TaskAgentError::InvalidTransition { .. })
        ));
        run(&mut agent, change(1, AgentChange::HandBack))
            .await
            .unwrap();
        assert_eq!(agent.state, Some(AgentState::Waiting));
        assert!(agent.state.unwrap().supervisor_may_resume());
        run(
            &mut agent,
            change(
                1,
                AgentChange::Done {
                    pr_url: "https://github.com/o/r/pull/1".into(),
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(agent.state.unwrap().owner(), None);
    }

    #[tokio::test]
    async fn forced_attach_over_running_turn() {
        let mut agent = TaskAgentAggregate::default();
        run(&mut agent, start("tm-abcdef")).await.unwrap();
        let events = run(&mut agent, change(1, AgentChange::Attach { force: true }))
            .await
            .unwrap();
        assert_eq!(events[0].payload["forced"], true);
        assert_eq!(agent.state, Some(AgentState::Attached));
    }

    #[tokio::test]
    async fn bad_code_refused() {
        let agent = TaskAgentAggregate::default();
        assert!(matches!(
            agent.handle(start("TM-ABCDEF")).await,
            Err(TaskAgentError::InvalidStart(_))
        ));
    }
}
