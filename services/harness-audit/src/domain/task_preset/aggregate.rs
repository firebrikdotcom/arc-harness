use std::collections::BTreeMap;

use super::commands::TaskPresetCommand;
use super::events::{
    TaskFilters, TaskPresetDefaultCleared, TaskPresetDefaultSet, TaskPresetDeleted,
    TaskPresetSaved, DEFAULT_CLEARED, DEFAULT_SET, PRESET_DELETED, PRESET_SAVED,
};
use crate::task_managers::valid_preset_name;
use arc_core::aggregate::Aggregate;
use arc_core::event::{Event, NewEvent};
use async_trait::async_trait;
use thiserror::Error;

/// Most presets one provider may hold.
pub const MAX_PRESETS: usize = 50;

#[derive(Debug, Error)]
pub enum TaskPresetError {
    #[error("preset names are 1-32 of [a-z0-9_-]")]
    InvalidName,
    #[error("invalid filters: {0}")]
    InvalidFilters(String),
    #[error("a preset needs at least one filter")]
    EmptyFilters,
    #[error("no preset named {0}")]
    UnknownPreset(String),
    #[error("at most {MAX_PRESETS} presets")]
    TooMany,
    #[error("could not serialize event payload: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Default)]
pub struct TaskPresetBookAggregate {
    pub version: i64,
    pub presets: BTreeMap<String, TaskFilters>,
    pub default: Option<String>,
}

impl TaskPresetBookAggregate {
    fn event(&self, id: &str, event_type: &str, payload: serde_json::Value) -> Event {
        Event::new(NewEvent {
            aggregate_type: Self::aggregate_type(),
            aggregate_id: id,
            sequence: self.version + 1,
            event_type,
            payload,
        })
    }

    fn known(&self, name: &str) -> Result<(), TaskPresetError> {
        if !valid_preset_name(name) {
            return Err(TaskPresetError::InvalidName);
        }
        if !self.presets.contains_key(name) {
            return Err(TaskPresetError::UnknownPreset(name.to_string()));
        }
        Ok(())
    }
}

#[async_trait]
impl Aggregate for TaskPresetBookAggregate {
    type Command = TaskPresetCommand;
    type Event = ();
    type Error = TaskPresetError;

    fn aggregate_type() -> &'static str {
        "TaskPresetBook"
    }

    fn version(&self) -> i64 {
        self.version
    }

    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
        match command {
            TaskPresetCommand::SavePreset { id, name, filters } => {
                if !valid_preset_name(&name) {
                    return Err(TaskPresetError::InvalidName);
                }
                filters
                    .validate()
                    .map_err(TaskPresetError::InvalidFilters)?;
                if filters.is_empty() {
                    return Err(TaskPresetError::EmptyFilters);
                }
                if self.presets.get(&name) == Some(&filters) {
                    return Ok(vec![]);
                }
                if !self.presets.contains_key(&name) && self.presets.len() >= MAX_PRESETS {
                    return Err(TaskPresetError::TooMany);
                }
                let payload = serde_json::to_value(TaskPresetSaved { name, filters })?;
                Ok(vec![self.event(&id, PRESET_SAVED, payload)])
            }
            TaskPresetCommand::DeletePreset { id, name } => {
                self.known(&name)?;
                let payload = serde_json::to_value(TaskPresetDeleted { name })?;
                Ok(vec![self.event(&id, PRESET_DELETED, payload)])
            }
            TaskPresetCommand::SetDefault { id, name } => {
                self.known(&name)?;
                if self.default.as_deref() == Some(name.as_str()) {
                    return Ok(vec![]);
                }
                let payload = serde_json::to_value(TaskPresetDefaultSet { name })?;
                Ok(vec![self.event(&id, DEFAULT_SET, payload)])
            }
            TaskPresetCommand::ClearDefault { id } => {
                if self.default.is_none() {
                    return Ok(vec![]);
                }
                let payload = serde_json::to_value(TaskPresetDefaultCleared {})?;
                Ok(vec![self.event(&id, DEFAULT_CLEARED, payload)])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
        let name = || {
            event.payload["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        match event.event_type.as_str() {
            PRESET_SAVED => {
                if let Ok(saved) = serde_json::from_value::<TaskPresetSaved>(event.payload.clone())
                {
                    self.presets.insert(saved.name, saved.filters);
                }
            }
            PRESET_DELETED => {
                let name = name();
                self.presets.remove(&name);
                if self.default.as_deref() == Some(name.as_str()) {
                    self.default = None;
                }
            }
            DEFAULT_SET => self.default = Some(name()),
            DEFAULT_CLEARED => self.default = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(
        state: &mut TaskPresetBookAggregate,
        command: TaskPresetCommand,
    ) -> Result<Vec<Event>, TaskPresetError> {
        let events = state.handle(command).await?;
        for event in &events {
            state.apply(event);
        }
        Ok(events)
    }

    fn save(name: &str, filters: TaskFilters) -> TaskPresetCommand {
        TaskPresetCommand::SavePreset {
            id: "basecamp".into(),
            name: name.into(),
            filters,
        }
    }

    #[tokio::test]
    async fn save_default_delete_clears_default() {
        let mut book = TaskPresetBookAggregate::default();
        let mine = TaskFilters {
            assignee: Some("me".into()),
            ..TaskFilters::default()
        };
        run(&mut book, save("mine", mine.clone())).await.unwrap();
        assert_eq!(book.presets["mine"], mine);
        run(
            &mut book,
            TaskPresetCommand::SetDefault {
                id: "basecamp".into(),
                name: "mine".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(book.default.as_deref(), Some("mine"));
        run(
            &mut book,
            TaskPresetCommand::DeletePreset {
                id: "basecamp".into(),
                name: "mine".into(),
            },
        )
        .await
        .unwrap();
        assert!(book.default.is_none());
        assert!(book.presets.is_empty());
    }

    #[tokio::test]
    async fn bad_names_filters_and_unknown_refused() {
        let mut book = TaskPresetBookAggregate::default();
        let some = TaskFilters {
            overdue: true,
            ..TaskFilters::default()
        };
        assert!(matches!(
            run(&mut book, save("Bad Name", some.clone())).await,
            Err(TaskPresetError::InvalidName)
        ));
        assert!(matches!(
            run(&mut book, save("empty", TaskFilters::default())).await,
            Err(TaskPresetError::EmptyFilters)
        ));
        let bad = TaskFilters {
            list: Some("12x".into()),
            ..TaskFilters::default()
        };
        assert!(matches!(
            run(&mut book, save("bad", bad)).await,
            Err(TaskPresetError::InvalidFilters(_))
        ));
        assert!(matches!(
            run(
                &mut book,
                TaskPresetCommand::SetDefault {
                    id: "basecamp".into(),
                    name: "nope".into()
                }
            )
            .await,
            Err(TaskPresetError::UnknownPreset(_))
        ));
    }
}
