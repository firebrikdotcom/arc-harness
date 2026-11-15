use super::commands::TaskAssociationCommand;
use super::events::{TaskListAssociated, TaskListUnassociated, ASSOCIATED, UNASSOCIATED};
use arc_core::aggregate::Aggregate;
use arc_core::event::{Event, NewEvent};
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TaskAssociationError {
    #[error("repository name cannot be empty")]
    EmptyRepo,
    #[error("directory must be an absolute path")]
    RelativeDir,
    #[error("the list has no stored association")]
    NotAssociated,
    #[error("could not serialize event payload: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// One task list's stored association. Absent (`current == None`) means the
/// configured default applies.
#[derive(Default)]
pub struct TaskAssociationAggregate {
    pub version: i64,
    pub current: Option<TaskListAssociated>,
}

impl TaskAssociationAggregate {
    fn event(&self, id: &str, event_type: &str, payload: serde_json::Value) -> Event {
        Event::new(NewEvent {
            aggregate_type: Self::aggregate_type(),
            aggregate_id: id,
            sequence: self.version + 1,
            event_type,
            payload,
        })
    }
}

#[async_trait]
impl Aggregate for TaskAssociationAggregate {
    type Command = TaskAssociationCommand;
    type Event = ();
    type Error = TaskAssociationError;

    fn aggregate_type() -> &'static str {
        "TaskAssociation"
    }

    fn version(&self) -> i64 {
        self.version
    }

    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
        match command {
            TaskAssociationCommand::Associate { id, repo, dir } => {
                if repo.trim().is_empty() {
                    return Err(TaskAssociationError::EmptyRepo);
                }
                if dir.as_deref().is_some_and(|d| !d.starts_with('/')) {
                    return Err(TaskAssociationError::RelativeDir);
                }
                let next = TaskListAssociated { repo, dir };
                if self.current.as_ref() == Some(&next) {
                    return Ok(vec![]);
                }
                Ok(vec![self.event(
                    &id,
                    ASSOCIATED,
                    serde_json::to_value(next)?,
                )])
            }
            TaskAssociationCommand::Unassociate { id } => {
                if self.current.is_none() {
                    return Err(TaskAssociationError::NotAssociated);
                }
                Ok(vec![self.event(
                    &id,
                    UNASSOCIATED,
                    serde_json::to_value(TaskListUnassociated {})?,
                )])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
        match event.event_type.as_str() {
            ASSOCIATED => self.current = serde_json::from_value(event.payload.clone()).ok(),
            UNASSOCIATED => self.current = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(
        state: &mut TaskAssociationAggregate,
        command: TaskAssociationCommand,
    ) -> Result<Vec<Event>, TaskAssociationError> {
        let events = state.handle(command).await?;
        for event in &events {
            state.apply(event);
        }
        Ok(events)
    }

    #[tokio::test]
    async fn associate_then_unassociate() {
        let mut state = TaskAssociationAggregate::default();
        let id = "basecamp:42".to_string();
        let events = run(
            &mut state,
            TaskAssociationCommand::Associate {
                id: id.clone(),
                repo: "app".into(),
                dir: Some("/tmp".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(events[0].event_type, ASSOCIATED);
        assert_eq!(events[0].payload["repo"], "app");
        // Same value again is a no-op.
        let again = run(
            &mut state,
            TaskAssociationCommand::Associate {
                id: id.clone(),
                repo: "app".into(),
                dir: Some("/tmp".into()),
            },
        )
        .await
        .unwrap();
        assert!(again.is_empty());
        run(
            &mut state,
            TaskAssociationCommand::Unassociate { id: id.clone() },
        )
        .await
        .unwrap();
        assert!(state.current.is_none());
        assert!(matches!(
            run(&mut state, TaskAssociationCommand::Unassociate { id }).await,
            Err(TaskAssociationError::NotAssociated)
        ));
    }

    #[tokio::test]
    async fn relative_dir_and_empty_repo_refused() {
        let state = TaskAssociationAggregate::default();
        let refused = |repo: &str, dir: Option<&str>| TaskAssociationCommand::Associate {
            id: "basecamp:1".into(),
            repo: repo.into(),
            dir: dir.map(str::to_string),
        };
        assert!(matches!(
            state.handle(refused("app", Some("rel/dir"))).await,
            Err(TaskAssociationError::RelativeDir)
        ));
        assert!(matches!(
            state.handle(refused(" ", None)).await,
            Err(TaskAssociationError::EmptyRepo)
        ));
    }
}
