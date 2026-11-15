use arc_core::aggregate::Command;

/// Commands on one task list's association; `id` is `<provider>:<list id>`.
pub enum TaskAssociationCommand {
    /// Associate the list with a configured repository and, optionally, an
    /// absolute working directory. Re-associating replaces the previous value.
    Associate {
        id: String,
        repo: String,
        dir: Option<String>,
    },
    /// Remove the stored association so the configured default applies again.
    Unassociate { id: String },
}

impl Command for TaskAssociationCommand {
    fn aggregate_id(&self) -> &str {
        match self {
            Self::Associate { id, .. } | Self::Unassociate { id } => id,
        }
    }
}
