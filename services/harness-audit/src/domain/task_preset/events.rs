use crate::task_managers::{valid_date, valid_id, valid_label, valid_repo};
use serde::{Deserialize, Serialize};

pub const PRESET_SAVED: &str = "TaskPresetSaved";
pub const PRESET_DELETED: &str = "TaskPresetDeleted";
pub const DEFAULT_SET: &str = "TaskPresetDefaultSet";
pub const DEFAULT_CLEARED: &str = "TaskPresetDefaultCleared";

/// `--agent none|active` on the open-items view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentFilter {
    #[serde(rename = "none")]
    NoAgent,
    #[serde(rename = "active")]
    Active,
}

/// Open-item filters; every field set must match (AND). Carries ids, a
/// repository name, an assignee name or `me`, and dates, never item text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    /// An assignee name (case-insensitive) or `me`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// `YYYY-MM-DD`; items due strictly before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_before: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overdue: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentFilter>,
}

impl TaskFilters {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Check every field's shape; the message names the bad field.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [("project", &self.project), ("list", &self.list)] {
            if value.as_deref().is_some_and(|v| !valid_id(v)) {
                return Err(format!("{field} must be 1-20 digits"));
            }
        }
        if self.assignee.as_deref().is_some_and(|v| !valid_label(v)) {
            return Err("assignee must be 1-100 printable characters".into());
        }
        if self.due_before.as_deref().is_some_and(|v| !valid_date(v)) {
            return Err("due_before must be a YYYY-MM-DD date".into());
        }
        if self.repo.as_deref().is_some_and(|v| !valid_repo(v)) {
            return Err("repo must be 1-100 of [A-Za-z0-9._-]".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPresetSaved {
    pub name: String,
    pub filters: TaskFilters,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPresetDeleted {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPresetDefaultSet {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskPresetDefaultCleared {}
