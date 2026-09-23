use super::commands::AuditEventCommand;
use super::events::{AuditEventRecorded, RECORDED};
use arc_core::aggregate::Aggregate;
use arc_core::event::{Event, NewEvent};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuditEventError {
    #[error("audit event already exists")]
    AlreadyExists,
    #[error("audit event id cannot be empty")]
    EmptyId,
    #[error("audit event type cannot be empty")]
    EmptyType,
    #[error("audit event payload must be an object")]
    InvalidPayload,
    #[error("could not serialize event payload: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Default, Serialize, Deserialize)]
pub struct AuditEventAggregate {
    pub version: i64,
    pub exists: bool,
}

#[async_trait]
impl Aggregate for AuditEventAggregate {
    type Command = AuditEventCommand;
    type Event = ();
    type Error = AuditEventError;

    fn aggregate_type() -> &'static str {
        "AuditEvent"
    }

    fn version(&self) -> i64 {
        self.version
    }

    async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
        match command {
            AuditEventCommand::Record {
                id,
                event_type,
                payload,
            } => {
                if self.exists {
                    return Err(AuditEventError::AlreadyExists);
                }
                if id.trim().is_empty() {
                    return Err(AuditEventError::EmptyId);
                }
                if event_type.trim().is_empty() {
                    return Err(AuditEventError::EmptyType);
                }
                if !payload.is_object() {
                    return Err(AuditEventError::InvalidPayload);
                }
                Ok(vec![Event::new(NewEvent {
                    aggregate_type: Self::aggregate_type(),
                    aggregate_id: &id,
                    sequence: self.version + 1,
                    event_type: RECORDED,
                    payload: serde_json::to_value(AuditEventRecorded {
                        id: id.clone(),
                        event_type,
                        payload,
                    })?,
                })])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        self.version = event.sequence;
        if event.event_type == RECORDED {
            self.exists = true;
        }
    }

    fn to_snapshot(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }

    fn from_snapshot(state: serde_json::Value) -> Option<Self> {
        serde_json::from_value(state).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn record_emits_an_event_with_measurements() {
        let events = AuditEventAggregate::default()
            .handle(AuditEventCommand::Record {
                id: "call-1".to_string(),
                event_type: "jev.route".to_string(),
                payload: json!({"input_tokens": 12}),
            })
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, RECORDED);
        assert_eq!(events[0].payload["payload"]["input_tokens"], 12);
    }
}
