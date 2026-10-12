use super::events::RECORDED;
use arc_core::event::Event;
use arc_core::projection::{ProjectionError, ProjectionResult, Projector};
use arc_core::read_model_store::{ReadModelStore, Upsert};
use async_trait::async_trait;
use serde_json::json;

pub const AUDIT_EVENTS_VIEW: &str = "audit_events_view";

pub struct AuditEventProjector;

#[async_trait]
impl Projector for AuditEventProjector {
    fn name(&self) -> &str {
        "AuditEventProjector"
    }

    fn handles(&self) -> Vec<String> {
        vec![RECORDED.to_string()]
    }

    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        if event.event_type != RECORDED {
            return Ok(());
        }
        let payload = event
            .payload
            .get("payload")
            .cloned()
            .ok_or_else(|| ProjectionError::other("audit event payload missing payload"))?;
        let row = json!({
            "id": event.aggregate_id,
            "event_type": event.payload.get("event_type"),
            "payload": payload,
            "version": event.sequence,
            "recorded_at_us": event.audit.timestamp_utc_us,
        });
        store
            .upsert(Upsert::new(AUDIT_EVENTS_VIEW, &event.aggregate_id, row))
            .await
            .map_err(|error| {
                ProjectionError::handle_failed(
                    self.name(),
                    &event.event_type,
                    event.event_id.to_string(),
                    error.to_string(),
                )
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_only_recorded_events() {
        assert_eq!(AuditEventProjector.handles(), vec![RECORDED.to_string()]);
    }
}
