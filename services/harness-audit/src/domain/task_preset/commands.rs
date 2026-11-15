use super::events::TaskFilters;
use arc_core::aggregate::Command;

/// Commands on one provider's preset book; `id` is the provider (`basecamp`).
pub enum TaskPresetCommand {
    /// Create or replace a named preset.
    SavePreset {
        id: String,
        name: String,
        filters: TaskFilters,
    },
    /// Delete a preset; deleting the default also clears the default.
    DeletePreset {
        id: String,
        name: String,
    },
    SetDefault {
        id: String,
        name: String,
    },
    ClearDefault {
        id: String,
    },
}

impl Command for TaskPresetCommand {
    fn aggregate_id(&self) -> &str {
        match self {
            Self::SavePreset { id, .. }
            | Self::DeletePreset { id, .. }
            | Self::SetDefault { id, .. }
            | Self::ClearDefault { id } => id,
        }
    }
}
