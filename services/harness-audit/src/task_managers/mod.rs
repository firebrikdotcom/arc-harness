//! Task Managers state: associations, open-item presets and attachable agents.
//!
//! Writes go through the three aggregates' command buses; [`TaskStore`] keeps
//! small index tables that are caught up from the event log after every
//! dispatch and at open. Item text never enters this module's events.

pub mod agents;
pub mod cli;
pub mod open;
pub mod store;

use std::collections::HashSet;
use std::path::Path;

use arc_core::aggregate::Aggregate;
use arc_core::command_bus::{CommandBus, CommandBusError, CommandContext};
use arc_core::event_store::EventStoreError;
use rand::Rng;
use serde_json::Value;
use thiserror::Error;

use crate::domain::task_agent::aggregate::{TaskAgentAggregate, MAX_REASON};
use crate::domain::task_agent::commands::{AgentChange, StartAgent, TaskAgentCommand};
use crate::domain::task_agent::events::AgentKind;
use crate::domain::task_association::aggregate::TaskAssociationAggregate;
use crate::domain::task_association::commands::TaskAssociationCommand;
use crate::domain::task_preset::aggregate::TaskPresetBookAggregate;
use crate::domain::task_preset::commands::TaskPresetCommand;
use crate::domain::task_preset::events::TaskFilters;

pub use open::{OpenContext, OpenPage};
pub use store::{AgentRecord, Association, TaskStore};

/// The only provider so far; aggregate ids are `basecamp:<id>`.
pub const PROVIDER: &str = "basecamp";

/// Provider ids: 1-20 ASCII digits.
pub fn valid_id(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

/// Preset names: `[a-z0-9_-]{1,32}`.
pub fn valid_preset_name(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Attach codes: `tm-` and 6 lowercase hex digits.
pub fn valid_code(value: &str) -> bool {
    value.len() == 9
        && value.starts_with("tm-")
        && value[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `YYYY-MM-DD` calendar dates.
pub fn valid_date(value: &str) -> bool {
    value.len() == 10 && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// Short printable labels such as an assignee name: 1-100 characters, no controls.
pub fn valid_label(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 100 && !value.chars().any(char::is_control)
}

/// Repository names as `task-managers.json` lists them: `[A-Za-z0-9._-]{1,100}`, not `.`/`..`.
pub fn valid_repo(value: &str) -> bool {
    (1..=100).contains(&value.len())
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Errors callers map to responses: `Usage` is a bad request (CLI exit 2),
/// `NotFound` an unknown list/preset/code, `Conflict` a state refusal (for
/// example a second live agent), `Internal` storage trouble.
#[derive(Debug, Error)]
pub enum TaskError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

impl TaskError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Internal(_) => "internal",
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::Internal(error.to_string())
    }
}

impl From<CommandBusError> for TaskError {
    fn from(error: CommandBusError) -> Self {
        match error {
            // Shapes are validated before dispatch, so a refusal here is about state.
            CommandBusError::HandleFailed { message, .. } => Self::Conflict(message),
            CommandBusError::AppendFailed {
                source: EventStoreError::ConcurrencyConflict { .. },
                ..
            } => Self::Conflict("the record changed concurrently; retry".into()),
            other => Self::internal(other),
        }
    }
}

/// What a caller supplies to record a newly launched agent.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub agent_kind: AgentKind,
    pub repo: String,
    /// Absolute, existing directory the agent works in.
    pub worktree: String,
    pub branch: Option<String>,
    pub created_worktree: bool,
    pub session_id: String,
    pub pid: u32,
    /// A code reserved with [`TaskManagers::new_code`] before launch (so the
    /// agent's files can be named by it); `None` draws a fresh one.
    pub code: Option<String>,
}

/// The service facade: validate, dispatch, then catch the index up.
pub struct TaskManagers {
    associations: CommandBus<TaskAssociationAggregate>,
    presets: CommandBus<TaskPresetBookAggregate>,
    agents: CommandBus<TaskAgentAggregate>,
    store: TaskStore,
}

async fn bus<A: Aggregate>(database_url: &str) -> Result<CommandBus<A>, TaskError> {
    arc_web::helpers::es_stack::build::<A>(database_url, vec![], None)
        .await
        .map(|stack| stack.command_bus)
        .map_err(|error| TaskError::Internal(error.to_string()))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn agent_id(item_id: &str) -> String {
    format!("{PROVIDER}:{item_id}")
}

impl TaskManagers {
    /// Open the buses and the index (created and caught up on open).
    pub async fn open(database_url: &str) -> Result<Self, TaskError> {
        Ok(Self {
            associations: bus(database_url).await?,
            presets: bus(database_url).await?,
            agents: bus(database_url).await?,
            store: TaskStore::open(database_url).map_err(TaskError::internal)?,
        })
    }

    pub fn store(&self) -> &TaskStore {
        &self.store
    }

    /// Catch the index up with events other processes committed.
    pub fn refresh(&self) -> Result<(), TaskError> {
        self.store
            .refresh()
            .map(|_| ())
            .map_err(TaskError::internal)
    }

    fn require_id(kind: &str, value: &str) -> Result<(), TaskError> {
        if valid_id(value) {
            Ok(())
        } else {
            Err(TaskError::usage(format!("{kind} id must be 1-20 digits")))
        }
    }

    fn require_repo(repo: &str, allowed_repos: &[String]) -> Result<(), TaskError> {
        if !valid_repo(repo) || !allowed_repos.iter().any(|allowed| allowed == repo) {
            return Err(TaskError::usage(format!(
                "repository {repo:?} is not listed in repos of task-managers.json"
            )));
        }
        Ok(())
    }

    fn require_dir(dir: &Path, what: &str) -> Result<String, TaskError> {
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(TaskError::usage(format!(
                "{what} must be an absolute path to an existing directory"
            )));
        }
        dir.to_str()
            .map(str::to_string)
            .ok_or_else(|| TaskError::usage(format!("{what} must be valid UTF-8")))
    }

    /// Store `list_id -> repo (+ dir)`. `repo` must be in `allowed_repos`;
    /// `dir`, when given, must be an absolute existing directory.
    pub async fn associate(
        &self,
        list_id: &str,
        repo: &str,
        dir: Option<&Path>,
        allowed_repos: &[String],
    ) -> Result<Association, TaskError> {
        Self::require_id("list", list_id)?;
        Self::require_repo(repo, allowed_repos)?;
        let dir = dir.map(|d| Self::require_dir(d, "dir")).transpose()?;
        self.associations
            .dispatch(
                TaskAssociationCommand::Associate {
                    id: agent_id(list_id),
                    repo: repo.to_string(),
                    dir,
                },
                CommandContext::system(),
            )
            .await?;
        self.refresh()?;
        self.store
            .association(list_id)
            .map_err(TaskError::internal)?
            .ok_or_else(|| TaskError::Internal("association missing after dispatch".into()))
    }

    /// Remove the stored association so the configured default applies again.
    /// Removing a list with nothing stored is a no-op.
    pub async fn unassociate(&self, list_id: &str) -> Result<(), TaskError> {
        Self::require_id("list", list_id)?;
        self.refresh()?;
        if self
            .store
            .association(list_id)
            .map_err(TaskError::internal)?
            .is_none()
        {
            return Ok(());
        }
        self.associations
            .dispatch(
                TaskAssociationCommand::Unassociate {
                    id: agent_id(list_id),
                },
                CommandContext::system(),
            )
            .await?;
        self.refresh()
    }

    fn require_preset_name(name: &str) -> Result<(), TaskError> {
        if valid_preset_name(name) {
            Ok(())
        } else {
            Err(TaskError::usage("preset names are 1-32 of [a-z0-9_-]"))
        }
    }

    fn require_known_preset(&self, name: &str) -> Result<(), TaskError> {
        Self::require_preset_name(name)?;
        self.refresh()?;
        if self
            .store
            .presets()
            .map_err(TaskError::internal)?
            .iter()
            .any(|preset| preset.name == name)
        {
            Ok(())
        } else {
            Err(TaskError::NotFound(format!("no preset named {name}")))
        }
    }

    pub async fn save_preset(&self, name: &str, filters: TaskFilters) -> Result<(), TaskError> {
        Self::require_preset_name(name)?;
        filters.validate().map_err(TaskError::Usage)?;
        if filters.is_empty() {
            return Err(TaskError::usage("a preset needs at least one filter"));
        }
        self.presets
            .dispatch(
                TaskPresetCommand::SavePreset {
                    id: PROVIDER.into(),
                    name: name.into(),
                    filters,
                },
                CommandContext::system(),
            )
            .await?;
        self.refresh()
    }

    pub async fn delete_preset(&self, name: &str) -> Result<(), TaskError> {
        self.require_known_preset(name)?;
        self.presets
            .dispatch(
                TaskPresetCommand::DeletePreset {
                    id: PROVIDER.into(),
                    name: name.into(),
                },
                CommandContext::system(),
            )
            .await?;
        self.refresh()
    }

    /// `Some(name)` makes that preset the default; `None` clears the default.
    pub async fn set_default_preset(&self, name: Option<&str>) -> Result<(), TaskError> {
        let command = match name {
            Some(name) => {
                self.require_known_preset(name)?;
                TaskPresetCommand::SetDefault {
                    id: PROVIDER.into(),
                    name: name.into(),
                }
            }
            None => TaskPresetCommand::ClearDefault {
                id: PROVIDER.into(),
            },
        };
        self.presets
            .dispatch(command, CommandContext::system())
            .await?;
        self.refresh()
    }

    /// A fresh `tm-xxxxxx` code not held by any live agent.
    pub fn new_code(&self) -> Result<String, TaskError> {
        self.refresh()?;
        let mut rng = rand::thread_rng();
        for _ in 0..32 {
            let code = format!("tm-{:06x}", rng.gen::<u32>() & 0x00ff_ffff);
            if self
                .store
                .agent_by_code(&code)
                .map_err(TaskError::internal)?
                .is_none_or(|agent| !agent.state.is_live())
            {
                return Ok(code);
            }
        }
        Err(TaskError::Internal(
            "could not find a free attach code".into(),
        ))
    }

    /// Record a newly launched agent on `item_id` and give it a fresh code.
    /// Refused (`Conflict`) while the item has a live agent.
    pub async fn start_agent(
        &self,
        item_id: &str,
        agent: NewAgent,
        allowed_repos: &[String],
    ) -> Result<AgentRecord, TaskError> {
        Self::require_id("item", item_id)?;
        Self::require_repo(&agent.repo, allowed_repos)?;
        let worktree = Self::require_dir(Path::new(&agent.worktree), "worktree")?;
        if agent.session_id.is_empty()
            || agent.session_id.len() > 200
            || agent.session_id.chars().any(char::is_control)
        {
            return Err(TaskError::usage(
                "session id must be 1-200 printable characters",
            ));
        }
        if let Some(branch) = &agent.branch {
            if branch.is_empty()
                || branch.len() > 200
                || branch.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(TaskError::usage(
                    "branch must be 1-200 characters, no spaces",
                ));
            }
        }
        if let Some(live) = self
            .store
            .live_agent_for_item(item_id)
            .map_err(TaskError::internal)?
        {
            return Err(TaskError::Conflict(format!(
                "item {item_id} already has a live agent ({})",
                live.code
            )));
        }
        let code = match agent.code {
            Some(code) => {
                if !valid_code(&code) {
                    return Err(TaskError::usage("codes are tm- and 6 lowercase hex digits"));
                }
                if self.live_by_code(&code).is_ok() {
                    return Err(TaskError::Conflict(format!(
                        "code {code} is held by a live agent"
                    )));
                }
                code
            }
            None => self.new_code()?,
        };
        self.agents
            .dispatch(
                TaskAgentCommand::Start {
                    id: agent_id(item_id),
                    start: StartAgent {
                        code: code.clone(),
                        agent_kind: agent.agent_kind,
                        repo: agent.repo,
                        worktree,
                        branch: agent.branch,
                        created_worktree: agent.created_worktree,
                        session_id: agent.session_id,
                        pid: agent.pid,
                        started_at: now(),
                    },
                },
                CommandContext::system(),
            )
            .await?;
        self.refresh()?;
        self.live_by_code(&code)
    }

    pub fn live_by_code(&self, code: &str) -> Result<AgentRecord, TaskError> {
        if !valid_code(code) {
            return Err(TaskError::usage("codes are tm- and 6 lowercase hex digits"));
        }
        self.store
            .agent_by_code(code)
            .map_err(TaskError::internal)?
            .filter(|agent| agent.state.is_live())
            .ok_or_else(|| TaskError::NotFound(format!("no live agent with code {code}")))
    }

    /// Apply a lifecycle change to the live agent holding `code`; returns its new record.
    pub async fn change_agent(
        &self,
        code: &str,
        change: AgentChange,
    ) -> Result<AgentRecord, TaskError> {
        if let AgentChange::Fail { reason } = &change {
            if reason.trim().is_empty() || reason.len() > MAX_REASON {
                return Err(TaskError::usage("reason must be 1-500 bytes"));
            }
        }
        if let AgentChange::Done { pr_url } = &change {
            if !pr_url.starts_with("https://") || pr_url.len() > 500 {
                return Err(TaskError::usage("pr_url must be an https URL"));
            }
        }
        self.refresh()?;
        let agent = self.live_by_code(code)?;
        self.agents
            .dispatch(
                TaskAgentCommand::Change {
                    id: agent_id(&agent.item_id),
                    generation: agent.generation,
                    at: now(),
                    change,
                },
                CommandContext::system(),
            )
            .await?;
        self.refresh()?;
        self.store
            .agent(&agent.item_id, agent.generation)
            .map_err(TaskError::internal)?
            .ok_or_else(|| TaskError::Internal("agent missing after dispatch".into()))
    }

    /// Filter an `open-walk` result. `given` empty means the default preset
    /// applies. `default_repo` maps a list id to its configured default repo
    /// (`mappings` in task-managers.json); stored associations win.
    pub fn open_items(
        &self,
        walk: &Value,
        given: &TaskFilters,
        me: Option<&str>,
        today: &str,
        default_repo: &dyn Fn(&str) -> Option<String>,
        limit: usize,
    ) -> Result<OpenPage, TaskError> {
        self.refresh()?;
        let default = self.store.default_preset().map_err(TaskError::internal)?;
        let active: HashSet<String> = self
            .store
            .agents()
            .map_err(TaskError::internal)?
            .into_iter()
            .filter(|agent| agent.state.is_live())
            .map(|agent| agent.item_id)
            .collect();
        let repo_for_list = |list: &str| {
            self.store
                .resolve_association(list, default_repo(list).as_deref())
                .ok()
                .flatten()
                .map(|association| association.repo)
        };
        let context = OpenContext {
            me,
            today,
            repo_for_list: &repo_for_list,
            active_items: &active,
        };
        open::open_items(walk, given, default.as_ref(), &context, limit)
    }
}

#[cfg(test)]
mod tests;
