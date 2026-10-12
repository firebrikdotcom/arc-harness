use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RECORDED: &str = "AuditEventRecorded";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEventRecorded {
    pub id: String,
    pub event_type: String,
    pub payload: Value,
}
