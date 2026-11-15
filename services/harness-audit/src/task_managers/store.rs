//! Index tables for Task Managers, kept in a sidecar SQLite file beside the
//! event database (`<db>.tasks.sqlite`), created on open and caught up from
//! the three task aggregates' events. They are disposable: deleting the file
//! rebuilds them from the events table.

use anyhow::{anyhow, Context, Result};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Nullable, Text};
use serde::Serialize;
use serde_json::Value;

use super::PROVIDER;
use crate::domain::task_agent::events::{self as agent_events, AgentKind, AgentOwner, AgentState};
use crate::domain::task_association::events::{ASSOCIATED, UNASSOCIATED};
use crate::domain::task_preset::events::{
    TaskFilters, DEFAULT_CLEARED, DEFAULT_SET, PRESET_DELETED, PRESET_SAVED,
};

/// Bump when the index layout or reducer changes; the index is rebuilt from events.
const SCHEMA_VERSION: i64 = 1;
const BATCH: i64 = 500;
const AGGREGATE_TYPES: &str = "('TaskAssociation','TaskPresetBook','TaskAgent')";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationSource {
    /// Recorded with `associate`.
    Stored,
    /// From `mappings` in task-managers.json, used when nothing is stored.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Association {
    pub list_id: String,
    pub repo: String,
    pub dir: Option<String>,
    pub source: AssociationSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preset {
    pub name: String,
    pub filters: TaskFilters,
    pub is_default: bool,
}

/// One agent generation on an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRecord {
    pub item_id: String,
    pub generation: u32,
    pub code: String,
    pub agent_kind: AgentKind,
    pub repo: String,
    pub worktree: String,
    pub branch: Option<String>,
    pub created_worktree: bool,
    pub session_id: String,
    pub pid: u32,
    pub started_at: i64,
    pub state: AgentState,
    /// `service` or `you`; `None` once ended. The supervisor never signals
    /// or resumes an agent owned by `you`.
    pub owner: Option<AgentOwner>,
    pub pr_url: Option<String>,
    pub reason: Option<String>,
    /// Unix seconds of the last lifecycle event.
    pub updated_at: i64,
}

#[derive(Clone)]
pub struct TaskStore {
    database_url: String,
    index_url: String,
}

#[derive(QueryableByName)]
struct Position {
    #[diesel(sql_type = BigInt)]
    position: i64,
}

#[derive(QueryableByName)]
struct FileName {
    #[diesel(sql_type = Text)]
    file: String,
}

#[derive(QueryableByName)]
struct SourceEvent {
    #[diesel(sql_type = BigInt)]
    id: i64,
    #[diesel(sql_type = Text)]
    aggregate_type: String,
    #[diesel(sql_type = Text)]
    aggregate_id: String,
    #[diesel(sql_type = Text)]
    event_type: String,
    #[diesel(sql_type = Text)]
    payload: String,
}

#[derive(QueryableByName)]
struct AssociationRow {
    #[diesel(sql_type = Text)]
    list_id: String,
    #[diesel(sql_type = Text)]
    repo: String,
    #[diesel(sql_type = Nullable<Text>)]
    dir: Option<String>,
}

#[derive(QueryableByName)]
struct PresetRow {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text)]
    filters: String,
    #[diesel(sql_type = Integer)]
    is_default: i32,
}

#[derive(QueryableByName)]
struct AgentRow {
    #[diesel(sql_type = Text)]
    item_id: String,
    #[diesel(sql_type = BigInt)]
    generation: i64,
    #[diesel(sql_type = Text)]
    code: String,
    #[diesel(sql_type = Text)]
    agent_kind: String,
    #[diesel(sql_type = Text)]
    repo: String,
    #[diesel(sql_type = Text)]
    worktree: String,
    #[diesel(sql_type = Nullable<Text>)]
    branch: Option<String>,
    #[diesel(sql_type = Integer)]
    created_worktree: i32,
    #[diesel(sql_type = Text)]
    session_id: String,
    #[diesel(sql_type = BigInt)]
    pid: i64,
    #[diesel(sql_type = BigInt)]
    started_at: i64,
    #[diesel(sql_type = Text)]
    state: String,
    #[diesel(sql_type = Nullable<Text>)]
    pr_url: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    reason: Option<String>,
    #[diesel(sql_type = BigInt)]
    updated_at: i64,
}

impl TryFrom<AgentRow> for AgentRecord {
    type Error = anyhow::Error;

    fn try_from(row: AgentRow) -> Result<Self> {
        let state = AgentState::parse(&row.state)
            .ok_or_else(|| anyhow!("unknown agent state {}", row.state))?;
        Ok(Self {
            item_id: row.item_id,
            generation: u32::try_from(row.generation)?,
            code: row.code,
            agent_kind: AgentKind::parse(&row.agent_kind)
                .ok_or_else(|| anyhow!("unknown agent kind {}", row.agent_kind))?,
            repo: row.repo,
            worktree: row.worktree,
            branch: row.branch,
            created_worktree: row.created_worktree != 0,
            session_id: row.session_id,
            pid: u32::try_from(row.pid)?,
            started_at: row.started_at,
            state,
            owner: state.owner(),
            pr_url: row.pr_url,
            reason: row.reason,
            updated_at: row.updated_at,
        })
    }
}

const AGENT_COLUMNS: &str = "item_id, generation, code, agent_kind, repo, worktree, branch, created_worktree, session_id, pid, started_at, state, pr_url, reason, updated_at";

const SCHEMA: &str = "PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS tm_checkpoint (singleton INTEGER PRIMARY KEY CHECK(singleton=1), position BIGINT NOT NULL, schema_version BIGINT NOT NULL);
    CREATE TABLE IF NOT EXISTS tm_associations (provider TEXT NOT NULL, list_id TEXT NOT NULL, repo TEXT NOT NULL, dir TEXT, PRIMARY KEY(provider, list_id));
    CREATE TABLE IF NOT EXISTS tm_presets (provider TEXT NOT NULL, name TEXT NOT NULL, filters TEXT NOT NULL, is_default INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(provider, name));
    CREATE TABLE IF NOT EXISTS tm_agents (provider TEXT NOT NULL, item_id TEXT NOT NULL, generation BIGINT NOT NULL, code TEXT NOT NULL, agent_kind TEXT NOT NULL, repo TEXT NOT NULL, worktree TEXT NOT NULL, branch TEXT, created_worktree INTEGER NOT NULL, session_id TEXT NOT NULL, pid BIGINT NOT NULL, started_at BIGINT NOT NULL, state TEXT NOT NULL, live INTEGER NOT NULL, pr_url TEXT, reason TEXT, updated_at BIGINT NOT NULL, PRIMARY KEY(provider, item_id, generation));
    CREATE UNIQUE INDEX IF NOT EXISTS tm_agents_live_code ON tm_agents(code) WHERE live=1;
    CREATE UNIQUE INDEX IF NOT EXISTS tm_agents_live_item ON tm_agents(provider, item_id) WHERE live=1;
    CREATE INDEX IF NOT EXISTS tm_agents_code ON tm_agents(code, generation);";

impl TaskStore {
    /// Create the index beside a file-backed event database and catch it up.
    pub fn open(database_url: &str) -> Result<Self> {
        let mut source = SqliteConnection::establish(database_url)?;
        let file = diesel::sql_query("SELECT file FROM pragma_database_list WHERE name='main'")
            .get_result::<FileName>(&mut source)?
            .file;
        anyhow::ensure!(
            !file.is_empty(),
            "task manager state requires a file-backed source database"
        );
        let store = Self {
            database_url: database_url.into(),
            index_url: format!("{file}.tasks.sqlite"),
        };
        let mut conn = store.connection()?;
        conn.batch_execute(SCHEMA)?;
        conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
            let version = diesel::sql_query(
                "SELECT schema_version AS position FROM tm_checkpoint WHERE singleton=1",
            )
            .get_result::<Position>(conn)
            .optional()?
            .map(|row| row.position);
            if version != Some(SCHEMA_VERSION) {
                conn.batch_execute(
                    "DELETE FROM tm_associations; DELETE FROM tm_presets; DELETE FROM tm_agents; DELETE FROM tm_checkpoint;",
                )?;
                diesel::sql_query("INSERT INTO tm_checkpoint VALUES(1, 0, ?)")
                    .bind::<BigInt, _>(SCHEMA_VERSION)
                    .execute(conn)?;
            }
            Ok(())
        })?;
        store.refresh()?;
        Ok(store)
    }

    fn connection(&self) -> Result<SqliteConnection> {
        let mut conn = SqliteConnection::establish(&self.index_url)?;
        conn.batch_execute("PRAGMA busy_timeout=10000;")?;
        Ok(conn)
    }

    /// Apply task events committed after the checkpoint; returns how many.
    /// Reads only the three task aggregate types through the aggregate index,
    /// so a large audit log is never scanned.
    pub fn refresh(&self) -> Result<usize> {
        let mut conn = self.connection()?;
        let mut source = SqliteConnection::establish(&self.database_url)?;
        source.batch_execute("PRAGMA busy_timeout=10000;")?;
        let mut total = 0;
        loop {
            let applied = conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
                let cursor = diesel::sql_query(
                    "SELECT position FROM tm_checkpoint WHERE singleton=1",
                )
                .get_result::<Position>(conn)?
                .position;
                // `+id` keeps SQLite on idx_events_aggregate instead of a rowid scan.
                let events = diesel::sql_query(format!(
                    "SELECT id, aggregate_type, aggregate_id, event_type, payload FROM events WHERE aggregate_type IN {AGGREGATE_TYPES} AND +id > ? ORDER BY id LIMIT ?"
                ))
                .bind::<BigInt, _>(cursor)
                .bind::<BigInt, _>(BATCH)
                .load::<SourceEvent>(&mut source)?;
                for event in &events {
                    apply(conn, event).with_context(|| {
                        format!("task event {} ({}) could not be indexed", event.id, event.event_type)
                    })?;
                }
                if let Some(last) = events.last() {
                    diesel::sql_query("UPDATE tm_checkpoint SET position=? WHERE singleton=1")
                        .bind::<BigInt, _>(last.id)
                        .execute(conn)?;
                }
                Ok(events.len())
            })?;
            total += applied;
            if (applied as i64) < BATCH {
                return Ok(total);
            }
        }
    }

    /// The stored association of a list, if any.
    pub fn association(&self, list_id: &str) -> Result<Option<Association>> {
        let mut conn = self.connection()?;
        Ok(diesel::sql_query(
            "SELECT list_id, repo, dir FROM tm_associations WHERE provider=? AND list_id=?",
        )
        .bind::<Text, _>(PROVIDER)
        .bind::<Text, _>(list_id)
        .get_result::<AssociationRow>(&mut conn)
        .optional()?
        .map(|row| Association {
            list_id: row.list_id,
            repo: row.repo,
            dir: row.dir,
            source: AssociationSource::Stored,
        }))
    }

    /// The stored association, else the configured default repo (no dir).
    pub fn resolve_association(
        &self,
        list_id: &str,
        default_repo: Option<&str>,
    ) -> Result<Option<Association>> {
        Ok(self.association(list_id)?.or_else(|| {
            default_repo.map(|repo| Association {
                list_id: list_id.into(),
                repo: repo.into(),
                dir: None,
                source: AssociationSource::Default,
            })
        }))
    }

    /// Every stored association, by list id.
    pub fn associations(&self) -> Result<Vec<Association>> {
        let mut conn = self.connection()?;
        Ok(diesel::sql_query(
            "SELECT list_id, repo, dir FROM tm_associations WHERE provider=? ORDER BY list_id",
        )
        .bind::<Text, _>(PROVIDER)
        .load::<AssociationRow>(&mut conn)?
        .into_iter()
        .map(|row| Association {
            list_id: row.list_id,
            repo: row.repo,
            dir: row.dir,
            source: AssociationSource::Stored,
        })
        .collect())
    }

    /// Presets by name.
    pub fn presets(&self) -> Result<Vec<Preset>> {
        let mut conn = self.connection()?;
        diesel::sql_query(
            "SELECT name, filters, is_default FROM tm_presets WHERE provider=? ORDER BY name",
        )
        .bind::<Text, _>(PROVIDER)
        .load::<PresetRow>(&mut conn)?
        .into_iter()
        .map(|row| {
            Ok(Preset {
                filters: serde_json::from_str(&row.filters)?,
                name: row.name,
                is_default: row.is_default != 0,
            })
        })
        .collect()
    }

    pub fn default_preset(&self) -> Result<Option<Preset>> {
        Ok(self.presets()?.into_iter().find(|preset| preset.is_default))
    }

    fn agents_where(&self, clause: &str, binds: &[&str]) -> Result<Vec<AgentRecord>> {
        let mut conn = self.connection()?;
        let sql = format!("SELECT {AGENT_COLUMNS} FROM tm_agents WHERE provider=? {clause}");
        let rows = match binds {
            [] => diesel::sql_query(sql)
                .bind::<Text, _>(PROVIDER)
                .load::<AgentRow>(&mut conn)?,
            [a] => diesel::sql_query(sql)
                .bind::<Text, _>(PROVIDER)
                .bind::<Text, _>(*a)
                .load::<AgentRow>(&mut conn)?,
            [a, b] => diesel::sql_query(sql)
                .bind::<Text, _>(PROVIDER)
                .bind::<Text, _>(*a)
                .bind::<Text, _>(*b)
                .load::<AgentRow>(&mut conn)?,
            _ => return Err(anyhow!("too many binds")),
        };
        rows.into_iter().map(AgentRecord::try_from).collect()
    }

    /// The live agent holding `code`, else the most recent ended one.
    pub fn agent_by_code(&self, code: &str) -> Result<Option<AgentRecord>> {
        Ok(self
            .agents_where(
                "AND code=? ORDER BY live DESC, updated_at DESC, generation DESC LIMIT 1",
                &[code],
            )?
            .pop())
    }

    pub fn live_agent_for_item(&self, item_id: &str) -> Result<Option<AgentRecord>> {
        Ok(self
            .agents_where("AND item_id=? AND live=1 LIMIT 1", &[item_id])?
            .pop())
    }

    /// One generation of an item's agent.
    pub fn agent(&self, item_id: &str, generation: u32) -> Result<Option<AgentRecord>> {
        let generation = generation.to_string();
        Ok(self
            .agents_where(
                "AND item_id=? AND generation=CAST(? AS INTEGER) LIMIT 1",
                &[item_id, &generation],
            )?
            .pop())
    }

    /// Every agent generation: live ones first, then newest first.
    pub fn agents(&self) -> Result<Vec<AgentRecord>> {
        self.agents_where(
            "ORDER BY live DESC, updated_at DESC, item_id, generation DESC",
            &[],
        )
    }
}

fn text<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload[field]
        .as_str()
        .ok_or_else(|| anyhow!("missing {field}"))
}

fn int(payload: &Value, field: &str) -> Result<i64> {
    payload[field]
        .as_i64()
        .ok_or_else(|| anyhow!("missing {field}"))
}

fn apply(conn: &mut SqliteConnection, event: &SourceEvent) -> Result<()> {
    let payload: Value = serde_json::from_str(&event.payload)?;
    // `provider:id` for associations and agents; the preset book's id is the provider.
    let (provider, key) = event
        .aggregate_id
        .split_once(':')
        .unwrap_or((event.aggregate_id.as_str(), ""));
    match (event.aggregate_type.as_str(), event.event_type.as_str()) {
        ("TaskAssociation", ASSOCIATED) => {
            diesel::sql_query("INSERT INTO tm_associations(provider, list_id, repo, dir) VALUES(?,?,?,?) ON CONFLICT(provider, list_id) DO UPDATE SET repo=excluded.repo, dir=excluded.dir")
                .bind::<Text, _>(provider)
                .bind::<Text, _>(key)
                .bind::<Text, _>(text(&payload, "repo")?)
                .bind::<Nullable<Text>, _>(payload["dir"].as_str())
                .execute(conn)?;
        }
        ("TaskAssociation", UNASSOCIATED) => {
            diesel::sql_query("DELETE FROM tm_associations WHERE provider=? AND list_id=?")
                .bind::<Text, _>(provider)
                .bind::<Text, _>(key)
                .execute(conn)?;
        }
        ("TaskPresetBook", PRESET_SAVED) => {
            diesel::sql_query("INSERT INTO tm_presets(provider, name, filters) VALUES(?,?,?) ON CONFLICT(provider, name) DO UPDATE SET filters=excluded.filters")
                .bind::<Text, _>(provider)
                .bind::<Text, _>(text(&payload, "name")?)
                .bind::<Text, _>(payload["filters"].to_string())
                .execute(conn)?;
        }
        ("TaskPresetBook", PRESET_DELETED) => {
            diesel::sql_query("DELETE FROM tm_presets WHERE provider=? AND name=?")
                .bind::<Text, _>(provider)
                .bind::<Text, _>(text(&payload, "name")?)
                .execute(conn)?;
        }
        ("TaskPresetBook", DEFAULT_SET) => {
            diesel::sql_query("UPDATE tm_presets SET is_default=(name=?) WHERE provider=?")
                .bind::<Text, _>(text(&payload, "name")?)
                .bind::<Text, _>(provider)
                .execute(conn)?;
        }
        ("TaskPresetBook", DEFAULT_CLEARED) => {
            diesel::sql_query("UPDATE tm_presets SET is_default=0 WHERE provider=?")
                .bind::<Text, _>(provider)
                .execute(conn)?;
        }
        ("TaskAgent", agent_events::STARTED) => {
            let started: agent_events::TaskAgentStarted = serde_json::from_value(payload)?;
            diesel::sql_query("INSERT INTO tm_agents(provider, item_id, generation, code, agent_kind, repo, worktree, branch, created_worktree, session_id, pid, started_at, state, live, updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,1,?)")
                .bind::<Text, _>(provider)
                .bind::<Text, _>(key)
                .bind::<BigInt, _>(i64::from(started.generation))
                .bind::<Text, _>(&started.code)
                .bind::<Text, _>(started.agent_kind.as_str())
                .bind::<Text, _>(&started.repo)
                .bind::<Text, _>(&started.worktree)
                .bind::<Nullable<Text>, _>(started.branch.as_deref())
                .bind::<Integer, _>(i32::from(started.created_worktree))
                .bind::<Text, _>(&started.session_id)
                .bind::<BigInt, _>(i64::from(started.pid))
                .bind::<BigInt, _>(started.started_at)
                .bind::<Text, _>(AgentState::Running.as_str())
                .bind::<BigInt, _>(started.started_at)
                .execute(conn)?;
        }
        ("TaskAgent", kind) if agent_events::ALL.contains(&kind) => {
            let generation = int(&payload, "generation")?;
            let current = diesel::sql_query(
                "SELECT state AS file FROM tm_agents WHERE provider=? AND item_id=? AND generation=?",
            )
            .bind::<Text, _>(provider)
            .bind::<Text, _>(key)
            .bind::<BigInt, _>(generation)
            .get_result::<FileName>(conn)?
            .file;
            let state = AgentState::parse(&current)
                .ok_or_else(|| anyhow!("unknown agent state {current}"))?
                .after(kind);
            diesel::sql_query("UPDATE tm_agents SET state=?, live=?, pid=COALESCE(?, pid), pr_url=COALESCE(?, pr_url), reason=COALESCE(?, reason), updated_at=? WHERE provider=? AND item_id=? AND generation=?")
                .bind::<Text, _>(state.as_str())
                .bind::<Integer, _>(i32::from(state.is_live()))
                .bind::<Nullable<BigInt>, _>(payload["pid"].as_i64())
                .bind::<Nullable<Text>, _>(payload["pr_url"].as_str())
                .bind::<Nullable<Text>, _>(payload["reason"].as_str())
                .bind::<BigInt, _>(int(&payload, "at")?)
                .bind::<Text, _>(provider)
                .bind::<Text, _>(key)
                .bind::<BigInt, _>(generation)
                .execute(conn)?;
        }
        // Unknown task event types from a newer build are skipped, not fatal.
        _ => {}
    }
    Ok(())
}
