use serde::{Deserialize, Serialize};

pub const ASSOCIATED: &str = "TaskListAssociated";
pub const UNASSOCIATED: &str = "TaskListUnassociated";

/// Payload of `TaskListAssociated`: a repository name and an optional absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListAssociated {
    pub repo: String,
    pub dir: Option<String>,
}

/// Payload of `TaskListUnassociated` (an empty object).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskListUnassociated {}
