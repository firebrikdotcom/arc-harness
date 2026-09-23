use arc_core::aggregate::Command;
use serde_json::Value;

pub enum AuditEventCommand {
    Record {
        id: String,
        event_type: String,
        payload: Value,
    },
}

impl Command for AuditEventCommand {
    fn aggregate_id(&self) -> &str {
        match self {
            Self::Record { id, .. } => id,
        }
    }
}
