//! Durable dashboard projections. The source cursor and every derived report commit together.
//! Source positions, rather than producer timestamps, define catch-up order.
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{BigInt, Text},
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

mod orchestration;
mod session;

type OrchestrationKey = (String, String, String);

fn update_orchestration_fact(
    conn: &mut SqliteConnection,
    row: &Value,
    position: i64,
) -> Result<()> {
    let id = row["id"]
        .as_str()
        .ok_or_else(|| anyhow!("missing orchestration event id"))?;
    diesel::sql_query("DELETE FROM dashboard_orchestration_facts WHERE id=?")
        .bind::<Text, _>(id)
        .execute(conn)?;
    if let Some((category, scope, entity)) = crate::orchestration::entity_key(row) {
        diesel::sql_query("INSERT INTO dashboard_orchestration_facts(id,position,category,scope,entity,recorded_at_us,document) VALUES(?,?,?,?,?,?,?)")
            .bind::<Text,_>(id).bind::<BigInt,_>(position).bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(entity)
            .bind::<BigInt,_>(row["recorded_at_us"].as_i64().unwrap_or(0)).bind::<Text,_>(row.to_string()).execute(conn)?;
    }
    Ok(())
}

fn refresh_orchestration(
    conn: &mut SqliteConnection,
    keys: &BTreeSet<OrchestrationKey>,
) -> Result<()> {
    orchestration::refresh(conn, keys)
}

type SessionEntityKey = (String, String, String);

fn update_session_facts(
    conn: &mut SqliteConnection,
    row: &Value,
    position: i64,
    keys: &mut BTreeSet<SessionEntityKey>,
) -> Result<()> {
    let id = row["id"]
        .as_str()
        .ok_or_else(|| anyhow!("missing workflow event id"))?;
    diesel::sql_query("DELETE FROM dashboard_session_facts WHERE id=?")
        .bind::<Text, _>(id)
        .execute(conn)?;
    for (session, category, entity) in crate::workflow::projection_keys(row) {
        diesel::sql_query("INSERT INTO dashboard_session_facts(id,session,category,entity,position) VALUES(?,?,?,?,?)")
            .bind::<Text,_>(id).bind::<Text,_>(&session).bind::<Text,_>(&category).bind::<Text,_>(&entity).bind::<BigInt,_>(position).execute(conn)?;
        keys.insert((session, category, entity));
    }
    Ok(())
}

fn refresh_session_entities(
    conn: &mut SqliteConnection,
    keys: &BTreeSet<SessionEntityKey>,
) -> Result<()> {
    for (session, category, entity) in keys {
        if category == "context" {
            continue;
        }
        if matches!(category.as_str(), "task" | "todo" | "prompt" | "tool") {
            let head = session::head(conn, session, category, entity)?;
            session::invalidate(conn, session, category, entity, &head)?;
            if head.is_null() {
                diesel::sql_query("DELETE FROM dashboard_session_entities WHERE session=? AND category=? AND entity=?").bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).execute(conn)?;
            } else {
                diesel::sql_query("INSERT INTO dashboard_session_entities(session,category,entity,document) VALUES(?,?,?,?) ON CONFLICT(session,category,entity) DO UPDATE SET document=excluded.document")
                    .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).bind::<Text,_>(head.to_string()).execute(conn)?;
            }
            continue;
        }
        // Metadata can be repeated on every event in a long session. Only its
        // chronological endpoints are needed to derive the current value and
        // timestamps; keep the count in SQLite rather than loading the history.
        let mut count = None;
        let rows = if category == "metadata" {
            let n = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_session_facts WHERE session=? AND category=? AND entity=?")
                .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).get_result::<Position>(conn)?.position;
            count = Some(n);
            let mut endpoints = Vec::new();
            for order in ["ASC", "DESC"] {
                let sql = format!("SELECT e.document FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=? AND f.category=? AND f.entity=? ORDER BY json_extract(e.document, '$.payload.occurred_at') {order}, e.recorded_at_us {order}, f.position {order} LIMIT 1");
                if let Some(document) = diesel::sql_query(sql)
                    .bind::<Text, _>(session)
                    .bind::<Text, _>(category)
                    .bind::<Text, _>(entity)
                    .get_result::<Document>(conn)
                    .optional()?
                {
                    endpoints.push(document);
                }
            }
            documents(endpoints)?
        } else {
            documents(diesel::sql_query("SELECT e.document FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=? AND f.category=? AND f.entity=? ORDER BY f.position")
                .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).load::<Document>(conn)?)?
        };
        if rows.is_empty() {
            diesel::sql_query("DELETE FROM dashboard_session_entities WHERE session=? AND category=? AND entity=?").bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).execute(conn)?;
        } else {
            let mut summary = crate::workflow::projection_entity(category, entity, &rows);
            if let Some(count) = count {
                summary["events"] = json!(count);
            }
            diesel::sql_query("INSERT INTO dashboard_session_entities(session,category,entity,document) VALUES(?,?,?,?) ON CONFLICT(session,category,entity) DO UPDATE SET document=excluded.document")
                .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).bind::<Text,_>(summary.to_string()).execute(conn)?;
        }
    }
    Ok(())
}

// A bounded summary built from indexed facts; no timeline or prompt bodies are loaded.
fn refresh_session_summary(conn: &mut SqliteConnection, id: &str) -> Result<()> {
    let count = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_events WHERE session_id=? AND partition='workflow'")
        .bind::<Text,_>(id).get_result::<Position>(conn)?.position;
    let key = format!("session-summary:{id}");
    if count == 0 {
        diesel::sql_query("DELETE FROM dashboard_reports WHERE key=?")
            .bind::<Text, _>(&key)
            .execute(conn)?;
        return Ok(());
    }
    let mut summary = json!({"session_id":id,"event_count":count,"status":"observed","agent":"Unknown","model":"Unknown","cwd":"Unknown"});
    for field in [
        "session_name",
        "herdr_tab",
        "agent",
        "model",
        "cwd",
        "status",
    ] {
        let entity = diesel::sql_query("SELECT document FROM dashboard_session_entities WHERE session=? AND category='metadata' AND entity=?")
            .bind::<Text,_>(id).bind::<Text,_>(field).get_result::<Document>(conn).optional()?;
        if let Some(entity) = entity {
            summary[field] = serde_json::from_str::<Value>(&entity.document)?["value"].clone();
        }
    }
    for (category, field) in [("task", "task_count"), ("prompt", "prompt_count")] {
        let sql = if category == "prompt" {
            "SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category=? AND json_extract(document,'$.complete')=1"
        } else {
            "SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category=?"
        };
        summary[field] = json!(
            diesel::sql_query(sql)
                .bind::<Text, _>(id)
                .bind::<Text, _>(category)
                .get_result::<Position>(conn)?
                .position
        );
    }
    // The compact directory already contains the canonical verdict. Extract just
    // that JSON value in SQLite, without loading the session timeline.
    if let Some(row) = diesel::sql_query("SELECT json_extract(document, '$.verdict') AS document FROM dashboard_reports WHERE key=? AND json_type(document, '$.verdict')='object'")
        .bind::<Text,_>(format!("directory:{id}")).get_result::<Document>(conn).optional()? {
        summary["verdict"] = serde_json::from_str(&row.document)?;
        summary["verdict_version"] = json!(1);
    }
    save(conn, &key, &summary)
}

const CHECKPOINT_FIELDS: &[&str] = &[
    "family",
    "question_version",
    "policy_version",
    "model_requested",
    "model_returned",
    "interaction_origin",
    "context_version",
];

fn checkpoint_key(row: &Value) -> Option<String> {
    (row["event_type"] == "jev.checkpoint").then(|| {
        let key: Vec<Option<&str>> = CHECKPOINT_FIELDS
            .iter()
            .map(|field| row["payload"][*field].as_str())
            .collect();
        serde_json::to_string(&key).expect("string key is serializable")
    })
}

fn checkpoint_affected(row: &Value, groups: &mut BTreeSet<String>, calls: &mut BTreeSet<String>) {
    if let Some(key) = checkpoint_key(row) {
        groups.insert(key);
    }
    if matches!(
        row["event_type"].as_str(),
        Some("jev.checkpoint" | "jev.checkpoint_outcome")
    ) {
        if let Some(call) = row["payload"]["call_id"].as_str() {
            calls.insert(call.to_string());
        }
    }
}

fn update_checkpoint_fact(conn: &mut SqliteConnection, row: &Value, position: i64) -> Result<()> {
    let id = row["id"]
        .as_str()
        .ok_or_else(|| anyhow!("missing checkpoint event id"))?;
    diesel::sql_query("DELETE FROM dashboard_checkpoint_facts WHERE id=?")
        .bind::<Text, _>(id)
        .execute(conn)?;
    if matches!(
        row["event_type"].as_str(),
        Some("jev.checkpoint" | "jev.checkpoint_outcome")
    ) {
        use diesel::sql_types::Nullable;
        diesel::sql_query("INSERT INTO dashboard_checkpoint_facts(id,position,kind,call_id,group_key,document) VALUES(?,?,?,?,?,?)")
            .bind::<Text,_>(id).bind::<BigInt,_>(position).bind::<Text,_>(row["event_type"].as_str().unwrap())
            .bind::<Nullable<Text>,_>(row["payload"]["call_id"].as_str()).bind::<Nullable<Text>,_>(checkpoint_key(row))
            .bind::<Text,_>(row.to_string()).execute(conn)?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct CheckpointGroup {
    #[diesel(sql_type = Text)]
    group_key: String,
    #[diesel(sql_type = Text)]
    included: String,
}

#[derive(QueryableByName)]
struct CheckpointRow {
    #[diesel(sql_type = BigInt)]
    position: i64,
    #[diesel(sql_type = Text)]
    document: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    outcome: Option<String>,
}
#[derive(QueryableByName)]
struct Sample {
    #[diesel(sql_type = diesel::sql_types::Double)]
    value: f64,
}
fn checkpoint_median(conn: &mut SqliteConnection, kind: i32) -> Result<Option<f64>> {
    let count =
        diesel::sql_query("SELECT COUNT(*) AS position FROM checkpoint_samples WHERE kind=?")
            .bind::<diesel::sql_types::Integer, _>(kind)
            .get_result::<Position>(conn)?
            .position;
    if count == 0 {
        return Ok(None);
    }
    let values = diesel::sql_query(
        "SELECT value FROM checkpoint_samples WHERE kind=? ORDER BY value LIMIT ? OFFSET ?",
    )
    .bind::<diesel::sql_types::Integer, _>(kind)
    .bind::<BigInt, _>(if count % 2 == 0 { 2 } else { 1 })
    .bind::<BigInt, _>((count - 1) / 2)
    .load::<Sample>(conn)?;
    Ok(Some(if values.len() == 1 {
        values[0].value
    } else {
        (values[0].value + values[1].value) / 2.0
    }))
}

fn refresh_checkpoint_groups(
    conn: &mut SqliteConnection,
    groups: &mut BTreeSet<String>,
    calls: &BTreeSet<String>,
) -> Result<()> {
    for call in calls {
        let keys = diesel::sql_query("SELECT DISTINCT group_key AS document FROM dashboard_checkpoint_facts WHERE call_id=? AND kind='jev.checkpoint'")
            .bind::<Text,_>(call).load::<Document>(conn)?;
        groups.extend(keys.into_iter().map(|row| row.document));
    }
    if groups.is_empty() {
        return Ok(());
    }
    for key in groups.iter() {
        conn.batch_execute("CREATE TEMP TABLE IF NOT EXISTS checkpoint_samples (kind INTEGER NOT NULL, value REAL NOT NULL); DELETE FROM checkpoint_samples;")?;
        let mut included_stream = crate::summary::StreamingCheckpoint::default();
        let mut excluded_stream = crate::summary::StreamingCheckpoint::default();
        let mut cursor = 0;
        loop {
            let rows = diesel::sql_query("SELECT f.position, f.document, (SELECT json_extract(o.document,'$.payload.outcome') FROM dashboard_checkpoint_facts o WHERE o.kind='jev.checkpoint_outcome' AND o.call_id=f.call_id AND json_type(o.document,'$.payload.outcome')='text' ORDER BY COALESCE(json_extract(o.document,'$.recorded_at_us'),0) DESC,o.position DESC LIMIT 1) AS outcome FROM dashboard_checkpoint_facts f WHERE f.kind='jev.checkpoint' AND f.group_key=? AND f.position>? ORDER BY f.position LIMIT 256")
                .bind::<Text,_>(key).bind::<BigInt,_>(cursor).load::<CheckpointRow>(conn)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                cursor = row.position;
                let value: Value = serde_json::from_str(&row.document)?;
                included_stream.record(&value, row.outcome.as_deref(), true);
                excluded_stream.record(&value, row.outcome.as_deref(), false);
                let p = &value["payload"];
                for (kind, sample) in [
                    (
                        0,
                        p["recommendation_confidence"]
                            .as_f64()
                            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                    ),
                    (1, p["jev_latency_ms"].as_f64()),
                ] {
                    if let Some(sample) = sample {
                        diesel::sql_query("INSERT INTO checkpoint_samples(kind,value) VALUES(?,?)")
                            .bind::<diesel::sql_types::Integer, _>(kind)
                            .bind::<diesel::sql_types::Double, _>(sample)
                            .execute(conn)?;
                    }
                }
            }
        }
        let confidence = checkpoint_median(conn, 0)?;
        let latency = checkpoint_median(conn, 1)?;
        let group_key: Vec<Option<String>> = serde_json::from_str(key)?;
        let included = included_stream.finish(group_key.clone(), true, confidence, latency);
        let excluded = excluded_stream.finish(group_key, false, confidence, latency);
        if included["groups"]
            .as_array()
            .is_some_and(|groups| groups.is_empty())
        {
            diesel::sql_query("DELETE FROM dashboard_checkpoint_groups WHERE group_key=?")
                .bind::<Text, _>(key)
                .execute(conn)?;
        } else {
            diesel::sql_query("INSERT INTO dashboard_checkpoint_groups(group_key,included,excluded) VALUES(?,?,?) ON CONFLICT(group_key) DO UPDATE SET included=excluded.included,excluded=excluded.excluded")
                .bind::<Text,_>(key).bind::<Text,_>(included.to_string()).bind::<Text,_>(excluded.to_string()).execute(conn)?;
        }
    }
    Ok(())
}

const ROUTE_FIELDS: &[&str] = &[
    "source",
    "routing_mode",
    "model_requested",
    "model_returned",
    "interaction_origin",
    "context_version",
];

fn route_key(row: &Value) -> Option<String> {
    (row["event_type"] == "jev.route").then(|| {
        let key: Vec<Option<&str>> = ROUTE_FIELDS
            .iter()
            .map(|field| row["payload"][*field].as_str())
            .collect();
        serde_json::to_string(&key).expect("string key is serializable")
    })
}

fn route_affected(row: &Value, groups: &mut BTreeSet<String>, calls: &mut BTreeSet<String>) {
    if let Some(key) = route_key(row) {
        groups.insert(key);
    }
    if matches!(
        row["event_type"].as_str(),
        Some("jev.route" | "jev.outcome")
    ) {
        if let Some(call) = row["payload"]["call_id"].as_str() {
            calls.insert(call.to_string());
        }
    }
}

fn update_route_fact(conn: &mut SqliteConnection, row: &Value, position: i64) -> Result<()> {
    let id = row["id"]
        .as_str()
        .ok_or_else(|| anyhow!("missing route event id"))?;
    diesel::sql_query("DELETE FROM dashboard_route_facts WHERE id=?")
        .bind::<Text, _>(id)
        .execute(conn)?;
    if matches!(
        row["event_type"].as_str(),
        Some("jev.route" | "jev.outcome")
    ) {
        use diesel::sql_types::Nullable;
        diesel::sql_query("INSERT INTO dashboard_route_facts(id,position,kind,call_id,group_key,document) VALUES(?,?,?,?,?,?)")
            .bind::<Text,_>(id).bind::<BigInt,_>(position).bind::<Text,_>(row["event_type"].as_str().unwrap())
            .bind::<Nullable<Text>,_>(row["payload"]["call_id"].as_str()).bind::<Nullable<Text>,_>(route_key(row))
            .bind::<Text,_>(row.to_string()).execute(conn)?;
    }
    Ok(())
}

fn refresh_route_groups(
    conn: &mut SqliteConnection,
    groups: &mut BTreeSet<String>,
    calls: &BTreeSet<String>,
) -> Result<()> {
    for call in calls {
        let keys = diesel::sql_query("SELECT DISTINCT group_key AS document FROM dashboard_route_facts WHERE call_id=? AND kind='jev.route'")
            .bind::<Text,_>(call).load::<Document>(conn)?;
        groups.extend(keys.into_iter().map(|row| row.document));
    }
    if groups.is_empty() {
        return Ok(());
    }
    for key in groups.iter() {
        conn.batch_execute("CREATE TEMP TABLE IF NOT EXISTS checkpoint_samples (kind INTEGER NOT NULL, value REAL NOT NULL); DELETE FROM checkpoint_samples;")?;
        let mut included_stream = crate::summary::StreamingRoute::default();
        let mut excluded_stream = crate::summary::StreamingRoute::default();
        let mut cursor = 0;
        loop {
            let rows = diesel::sql_query("SELECT f.position, f.document, (SELECT json_extract(o.document,'$.payload.outcome') FROM dashboard_route_facts o WHERE o.kind='jev.outcome' AND o.call_id=f.call_id AND json_type(o.document,'$.payload.outcome')='text' ORDER BY COALESCE(json_extract(o.document,'$.recorded_at_us'),0) DESC,o.position DESC LIMIT 1) AS outcome FROM dashboard_route_facts f WHERE f.kind='jev.route' AND f.group_key=? AND f.position>? ORDER BY f.position LIMIT 256")
                .bind::<Text,_>(key).bind::<BigInt,_>(cursor).load::<CheckpointRow>(conn)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                cursor = row.position;
                let value: Value = serde_json::from_str(&row.document)?;
                included_stream.record(&value, row.outcome.as_deref(), true);
                excluded_stream.record(&value, row.outcome.as_deref(), false);
                if let Some(latency) = value["payload"]["jev_latency_ms"].as_f64() {
                    diesel::sql_query("INSERT INTO checkpoint_samples(kind,value) VALUES(1,?)")
                        .bind::<diesel::sql_types::Double, _>(latency)
                        .execute(conn)?;
                }
            }
        }
        let latency = checkpoint_median(conn, 1)?;
        let group_key: Vec<Option<String>> = serde_json::from_str(key)?;
        let included = included_stream.finish(group_key.clone(), true, latency);
        let excluded = excluded_stream.finish(group_key, false, latency);
        if included["groups"]
            .as_array()
            .is_some_and(|groups| groups.is_empty())
        {
            diesel::sql_query("DELETE FROM dashboard_route_groups WHERE group_key=?")
                .bind::<Text, _>(key)
                .execute(conn)?;
        } else {
            diesel::sql_query("INSERT INTO dashboard_route_groups(group_key,included,excluded) VALUES(?,?,?) ON CONFLICT(group_key) DO UPDATE SET included=excluded.included,excluded=excluded.excluded")
                .bind::<Text,_>(key).bind::<Text,_>(included.to_string()).bind::<Text,_>(excluded.to_string()).execute(conn)?;
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct ReportStore {
    database_url: String,
    projection_url: String,
}
#[derive(Default)]
pub struct EventFilter {
    pub event_type: Option<String>,
    pub session_id: Option<String>,
}
#[derive(QueryableByName)]
struct Document {
    #[diesel(sql_type = Text)]
    document: String,
}
#[derive(QueryableByName)]
struct ChartDocument {
    #[diesel(sql_type = BigInt)]
    position: i64,
    #[diesel(sql_type = Text)]
    document: String,
}
#[derive(QueryableByName)]
struct TimelineSource {
    #[diesel(sql_type = BigInt)]
    position: i64,
    #[diesel(sql_type = BigInt)]
    has_time: i64,
    #[diesel(sql_type = Text)]
    occurred_at: String,
    #[diesel(sql_type = BigInt)]
    recorded_at_us: i64,
    #[diesel(sql_type = Text)]
    document: String,
}
#[derive(QueryableByName)]
struct Position {
    #[diesel(sql_type = BigInt)]
    position: i64,
}
#[derive(QueryableByName)]
struct Source {
    #[diesel(sql_type = BigInt)]
    position: i64,
    #[diesel(sql_type = Text)]
    aggregate_id: String,
    #[diesel(sql_type = Text)]
    aggregate_type: String,
    #[diesel(sql_type = Text)]
    event_type: String,
    #[diesel(sql_type = Text)]
    payload: String,
    #[diesel(sql_type = BigInt)]
    sequence: i64,
    #[diesel(sql_type = BigInt)]
    recorded_at_us: i64,
}
pub const CATCH_UP_BATCH: usize = 256;
const TOTALS: &[&str] = &[
    "jev_input_tokens",
    "jev_output_tokens",
    "agent_input_tokens",
    "agent_output_tokens",
    "agent_total_tokens",
    "agent_total_tokens_delta",
    "total_decision_ms",
    "baseline_ms",
    "rework_ms",
];
impl ReportStore {
    pub fn open(database_url: &str) -> Result<Self> {
        // Resolve SQLite's actual file path, including file: URI aliases, so all
        // instances share one sidecar and never attach or write-lock the source.
        let mut source = SqliteConnection::establish(database_url)?;
        let file = diesel::sql_query(
            "SELECT file AS document FROM pragma_database_list WHERE name='main'",
        )
        .get_result::<Document>(&mut source)?
        .document;
        anyhow::ensure!(
            !file.is_empty(),
            "dashboard reports require a file-backed source database"
        );
        let store = Self {
            database_url: database_url.into(),
            projection_url: format!("{file}.dashboard.sqlite"),
        };
        let mut conn = store.connection()?;
        conn.batch_execute("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS dashboard_checkpoint (singleton INTEGER PRIMARY KEY CHECK(singleton=1), position BIGINT NOT NULL);
            INSERT OR IGNORE INTO dashboard_checkpoint VALUES(1,0);
            CREATE TABLE IF NOT EXISTS dashboard_events (id TEXT PRIMARY KEY, position BIGINT NOT NULL UNIQUE, event_type TEXT NOT NULL, session_id TEXT NOT NULL, partition TEXT NOT NULL, recorded_at_us BIGINT NOT NULL, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS dashboard_events_type_position ON dashboard_events(event_type,position);
            CREATE INDEX IF NOT EXISTS dashboard_events_session_position ON dashboard_events(session_id,position);
            CREATE INDEX IF NOT EXISTS dashboard_events_workflow_order ON dashboard_events(session_id,partition,(json_extract(document, '$.payload.occurred_at') IS NOT NULL),COALESCE(json_extract(document, '$.payload.occurred_at'),''),recorded_at_us,position);
            CREATE INDEX IF NOT EXISTS dashboard_events_partition_position ON dashboard_events(partition,position);
            CREATE INDEX IF NOT EXISTS dashboard_events_recent ON dashboard_events(recorded_at_us DESC,position DESC);
            CREATE TABLE IF NOT EXISTS dashboard_reports (key TEXT PRIMARY KEY, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS dashboard_directory_activity ON dashboard_reports(json_extract(document, '$.last_at'), key) WHERE key >= 'directory:' AND key < 'directory;';
            CREATE TABLE IF NOT EXISTS dashboard_checkpoint_facts (id TEXT PRIMARY KEY, position BIGINT NOT NULL, kind TEXT NOT NULL, call_id TEXT, group_key TEXT, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS checkpoint_facts_call ON dashboard_checkpoint_facts(call_id,kind,position);
            CREATE INDEX IF NOT EXISTS checkpoint_facts_group ON dashboard_checkpoint_facts(group_key,kind,position);
            CREATE TABLE IF NOT EXISTS dashboard_checkpoint_groups (group_key TEXT PRIMARY KEY, included TEXT NOT NULL, excluded TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS dashboard_route_facts (id TEXT PRIMARY KEY, position BIGINT NOT NULL, kind TEXT NOT NULL, call_id TEXT, group_key TEXT, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS route_facts_call ON dashboard_route_facts(call_id,kind,position);
            CREATE INDEX IF NOT EXISTS route_facts_group ON dashboard_route_facts(group_key,kind,position);
            CREATE TABLE IF NOT EXISTS dashboard_route_groups (group_key TEXT PRIMARY KEY, included TEXT NOT NULL, excluded TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS dashboard_session_facts (id TEXT NOT NULL, session TEXT NOT NULL, category TEXT NOT NULL, entity TEXT NOT NULL, position BIGINT NOT NULL, PRIMARY KEY(id,category,entity));
            CREATE INDEX IF NOT EXISTS session_facts_entity ON dashboard_session_facts(session,category,entity,position);
            CREATE TABLE IF NOT EXISTS dashboard_workflow_annotations (id TEXT PRIMARY KEY, session TEXT NOT NULL, position BIGINT NOT NULL, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS workflow_annotations_session ON dashboard_workflow_annotations(session,json_extract(document,'$.payload.occurred_at'),json_extract(document,'$.recorded_at_us'),position);
            CREATE TABLE IF NOT EXISTS dashboard_timeline_rows (session TEXT NOT NULL, view TEXT NOT NULL, ordering TEXT NOT NULL, ordinal BIGINT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(session,view,ordering,ordinal));
            CREATE TABLE IF NOT EXISTS dashboard_session_entities (session TEXT NOT NULL, category TEXT NOT NULL, entity TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(session,category,entity));
            CREATE TABLE IF NOT EXISTS dashboard_orchestration_facts (id TEXT PRIMARY KEY, position BIGINT NOT NULL, category TEXT NOT NULL, scope TEXT NOT NULL, entity TEXT NOT NULL, recorded_at_us BIGINT NOT NULL, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS orchestration_facts_entity ON dashboard_orchestration_facts(category,scope,entity,recorded_at_us,position);
            CREATE TABLE IF NOT EXISTS dashboard_orchestration_entities (category TEXT NOT NULL, scope TEXT NOT NULL, entity TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(category,scope,entity));")?;
        // Change this version when reducer semantics or compact report fields change.
        // Only disposable derived state is rebuilt; events and the legacy view remain intact.
        conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
            if cached(conn, "schema_version")? != Some(json!(12)) {
                conn.batch_execute("DELETE FROM dashboard_events; DELETE FROM dashboard_reports; DELETE FROM dashboard_checkpoint_facts; DELETE FROM dashboard_checkpoint_groups; DELETE FROM dashboard_route_facts; DELETE FROM dashboard_route_groups; DELETE FROM dashboard_orchestration_facts; DELETE FROM dashboard_orchestration_entities; DELETE FROM dashboard_session_facts; DELETE FROM dashboard_session_entities; DELETE FROM dashboard_timeline_rows; DELETE FROM dashboard_workflow_annotations; UPDATE dashboard_checkpoint SET position=0 WHERE singleton=1;")?;
                save(conn,"schema_version",&json!(12))?;
            }
            Ok(())
        })?;
        Ok(store)
    }
    pub(crate) fn connection(&self) -> Result<SqliteConnection> {
        let mut conn = SqliteConnection::establish(&self.projection_url)?;
        conn.batch_execute(
            "PRAGMA busy_timeout=10000; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;",
        )?;
        Ok(conn)
    }
    fn source_connection(&self) -> Result<SqliteConnection> {
        let mut conn = SqliteConnection::establish(&self.database_url)?;
        conn.batch_execute("PRAGMA busy_timeout=10000; PRAGMA cache_size=-8192;")?;
        Ok(conn)
    }
    /// Refresh only committed source rows after the durable cursor. An invalid source
    /// row fails the entire batch; it is never silently skipped or acknowledged.
    pub fn refresh(&self) -> Result<usize> {
        let mut conn = self.connection()?;
        let mut source_conn = self.source_connection()?;
        let cursor =
            diesel::sql_query("SELECT position FROM dashboard_checkpoint WHERE singleton=1")
                .get_result::<Position>(&mut conn)?
                .position;
        let pending =
            diesel::sql_query("SELECT EXISTS(SELECT 1 FROM events WHERE id > ?) AS position")
                .bind::<BigInt, _>(cursor)
                .get_result::<Position>(&mut source_conn)?
                .position;
        if pending == 0 {
            // Existing databases gain verdict summaries gradually, without resetting
            // the source checkpoint or rebuilding every session at startup.
            let needs_backfill = diesel::sql_query("SELECT EXISTS(SELECT 1 FROM dashboard_reports d LEFT JOIN dashboard_reports s ON s.key='session-summary:' || substr(d.key,11) WHERE d.key >= 'directory:' AND d.key < 'directory;' AND COALESCE(json_extract(s.document, '$.verdict_version'),0) != 1) AS position")
                .get_result::<Position>(&mut conn)?.position;
            if needs_backfill == 0 {
                return Ok(0);
            }
            return conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
                let rows = diesel::sql_query("SELECT substr(d.key,11) AS document FROM dashboard_reports d LEFT JOIN dashboard_reports s ON s.key='session-summary:' || substr(d.key,11) WHERE d.key >= 'directory:' AND d.key < 'directory;' AND COALESCE(json_extract(s.document, '$.verdict_version'),0) != 1 ORDER BY d.key LIMIT ?")
                    .bind::<BigInt,_>(CATCH_UP_BATCH as i64).load::<Document>(conn)?;
                for row in &rows { refresh_session_summary(conn, &row.document)?; }
                Ok(rows.len())
            });
        }
        conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
            // Serialize refreshers on the sidecar, then re-read its committed
            // cursor. This source SELECT ends before any projection work; its
            // independent connection never participates in the writer transaction.
            let cursor = diesel::sql_query("SELECT position FROM dashboard_checkpoint WHERE singleton=1").get_result::<Position>(conn)?.position;
            let source = diesel::sql_query("SELECT id AS position, aggregate_id, aggregate_type, event_type, payload, sequence, timestamp_utc_us AS recorded_at_us FROM events WHERE id > ? ORDER BY id LIMIT ?")
                .bind::<BigInt,_>(cursor).bind::<BigInt,_>(CATCH_UP_BATCH as i64).load::<Source>(&mut source_conn)?;
            if source.is_empty() { return Ok(0); }
            let mut sessions = BTreeSet::new();
            let mut partitions = BTreeSet::new();
            let mut checkpoint_groups = BTreeSet::new();
            let mut checkpoint_calls = BTreeSet::new();
            let mut route_groups = BTreeSet::new();
            let mut route_calls = BTreeSet::new();
            let mut orchestration_keys = BTreeSet::new();
            let mut session_keys = BTreeSet::new();
            let mut raw = cached(conn,"raw")?.unwrap_or_else(|| json!({"events":0,"event_types":{},"outcomes":{},"totals":{}}));
            for event in &source {
                if event.aggregate_type != "AuditEvent" || event.event_type != "AuditEventRecorded" { continue; }
                let envelope: Value = serde_json::from_str(&event.payload).with_context(|| format!("invalid audit source JSON at position {}",event.position))?;
                let kind = envelope["event_type"].as_str().ok_or_else(|| anyhow!("missing audit event type at position {}", event.position))?;
                let payload = envelope.get("payload").filter(|p| p.is_object()).ok_or_else(|| anyhow!("invalid audit payload at position {}", event.position))?;
                let row = json!({"id":event.aggregate_id,"event_type":kind,"payload":payload,"version":event.sequence,"recorded_at_us":event.recorded_at_us});
                let old = diesel::sql_query("SELECT document FROM dashboard_events WHERE id=?").bind::<Text,_>(&event.aggregate_id).get_result::<Document>(conn).optional()?;
                if let Some(old) = old { let old:Value=serde_json::from_str(&old.document)?; session_keys.extend(crate::workflow::projection_keys(&old)); if let Some(key) = crate::orchestration::entity_key(&old) { orchestration_keys.insert(key); } checkpoint_affected(&old, &mut checkpoint_groups, &mut checkpoint_calls); route_affected(&old, &mut route_groups, &mut route_calls); adjust_raw(&mut raw,&old,-1)?; affected(&old,&mut sessions,&mut partitions); }
                affected(&row,&mut sessions,&mut partitions);
                checkpoint_affected(&row, &mut checkpoint_groups, &mut checkpoint_calls);
                route_affected(&row, &mut route_groups, &mut route_calls);
                if let Some(key) = crate::orchestration::entity_key(&row) { orchestration_keys.insert(key); }
                diesel::sql_query("DELETE FROM dashboard_workflow_annotations WHERE id=?").bind::<Text,_>(&event.aggregate_id).execute(conn)?;
                update_session_facts(conn, &row, event.position, &mut session_keys)?;
                update_orchestration_fact(conn, &row, event.position)?;
                update_route_fact(conn, &row, event.position)?;
                update_checkpoint_fact(conn, &row, event.position)?;
                adjust_raw(&mut raw,&row,1)?;
                diesel::sql_query("INSERT INTO dashboard_events(id,position,event_type,session_id,partition,recorded_at_us,document) VALUES(?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET position=excluded.position,event_type=excluded.event_type,session_id=excluded.session_id,partition=excluded.partition,recorded_at_us=excluded.recorded_at_us,document=excluded.document")
                    .bind::<Text,_>(&event.aggregate_id).bind::<BigInt,_>(event.position).bind::<Text,_>(kind).bind::<Text,_>(payload["session_id"].as_str().unwrap_or(""))
                    .bind::<Text,_>(partition(kind)).bind::<BigInt,_>(event.recorded_at_us).bind::<Text,_>(row.to_string()).execute(conn)?;
            }
            save(conn,"raw",&raw)?;
            refresh_session_entities(conn, &session_keys)?;
            for id in sessions {
                session::refresh(conn, &id)?;
                refresh_session_summary(conn, &id)?;
            }
            refresh_checkpoint_groups(conn, &mut checkpoint_groups, &checkpoint_calls)?;
            refresh_route_groups(conn, &mut route_groups, &route_calls)?;
            if partitions.contains("orchestration") {
                refresh_orchestration(conn, &orchestration_keys)?;
            }
            diesel::sql_query("UPDATE dashboard_checkpoint SET position=? WHERE singleton=1").bind::<BigInt,_>(source.last().unwrap().position).execute(conn)?;
            Ok(source.len())
        })
    }
    /// Authoritative ID lookup, independent of the dashboard catch-up cursor.
    /// The source index starts with (aggregate_type, aggregate_id, sequence).
    pub fn get_source_event(&self, id: &str) -> Result<Option<Value>> {
        let mut conn = self.source_connection()?;
        diesel::sql_query("SELECT payload AS document FROM events WHERE aggregate_type='AuditEvent' AND aggregate_id=? AND sequence=1 AND event_type='AuditEventRecorded'")
            .bind::<Text, _>(id)
            .get_result::<Document>(&mut conn)
            .optional()?
            .map(|d| Ok(serde_json::from_str(&d.document)?))
            .transpose()
    }
    pub fn get_event(&self, id: &str) -> Result<Option<Value>> {
        let mut conn = self.connection()?;
        diesel::sql_query("SELECT document FROM dashboard_events WHERE id=?")
            .bind::<Text, _>(id)
            .get_result::<Document>(&mut conn)
            .optional()?
            .map(|d| Ok(serde_json::from_str(&d.document)?))
            .transpose()
    }
    pub fn events(&self, filter: &EventFilter, limit: usize) -> Result<Vec<Value>> {
        let mut conn = self.connection()?;
        // Optional predicates are assembled from fixed SQL; values always bind.
        let mut sql = "SELECT document FROM dashboard_events WHERE 1=1".to_string();
        if filter.event_type.is_some() {
            sql.push_str(" AND event_type=?");
        }
        if filter.session_id.is_some() {
            sql.push_str(" AND session_id=?");
        }
        sql.push_str(" ORDER BY recorded_at_us DESC,position DESC LIMIT ?");
        let mut query = diesel::sql_query(sql).into_boxed::<diesel::sqlite::Sqlite>();
        if let Some(kind) = &filter.event_type {
            query = query.bind::<Text, _>(kind);
        }
        if let Some(id) = &filter.session_id {
            query = query.bind::<Text, _>(id);
        }
        documents(
            query
                .bind::<BigInt, _>(i64::try_from(limit).unwrap_or(i64::MAX))
                .load::<Document>(&mut conn)?,
        )
    }
    fn report(&self, key: &str, default: Value) -> Result<Value> {
        Ok(cached(&mut self.connection()?, key)?.unwrap_or(default))
    }
    pub fn raw_summary(&self) -> Result<Value> {
        self.report(
            "raw",
            json!({"events":0,"event_types":{},"outcomes":{},"totals":{}}),
        )
    }
    pub fn event_counts(&self) -> Result<BTreeMap<String, usize>> {
        Ok(serde_json::from_value(
            self.raw_summary()?["event_types"].clone(),
        )?)
    }
    pub fn checkpoint_groups(&self, fixtures: bool) -> Result<Value> {
        self.groups_page("checkpoint", fixtures, 0)
    }
    pub fn route_groups(&self, fixtures: bool) -> Result<Value> {
        self.groups_page("route", fixtures, 0)
    }
    pub fn groups_page(&self, kind: &str, fixtures: bool, offset: usize) -> Result<Value> {
        let (table, fields, mut result) = if kind == "checkpoint" {
            (
                "dashboard_checkpoint_groups",
                7,
                crate::summary::checkpoint_groups(&[], fixtures),
            )
        } else {
            (
                "dashboard_route_groups",
                6,
                crate::summary::route_groups(&[], fixtures),
            )
        };
        let column = if fixtures { "included" } else { "excluded" };
        let order = (0..fields)
            .map(|i| format!("json_extract(group_key,'$[{i}]')"))
            .collect::<Vec<_>>()
            .join(",");
        let mut conn = self.connection()?;
        let rows=documents(diesel::sql_query(format!("SELECT {column} AS document FROM {table} WHERE json_array_length({column},'$.groups')>0 ORDER BY {order} LIMIT 200 OFFSET ?")).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(&mut conn)?)?;
        for row in rows {
            result["groups"]
                .as_array_mut()
                .unwrap()
                .extend(row["groups"].as_array().unwrap().iter().cloned());
        }
        result["excluded_fixture_events"]=json!(diesel::sql_query(format!("SELECT COALESCE(SUM(json_extract({column},'$.excluded_fixture_events')),0) AS position FROM {table}")).get_result::<Position>(&mut conn)?.position);
        Ok(result)
    }
    pub fn group_totals(&self, fixtures: bool) -> Result<(Value, Value)> {
        let mut conn = self.connection()?;
        let column = if fixtures { "included" } else { "excluded" };
        let mut aggregate = json!({"count":0,"labeled":0,"correct":0});
        let mut usage_group = json!({"count":0});
        let mut excluded = 0;
        for table in ["dashboard_checkpoint_groups", "dashboard_route_groups"] {
            let mut cursor = String::new();
            loop {
                let rows=diesel::sql_query(format!("SELECT group_key,{column} AS included,{column} AS excluded FROM {table} WHERE group_key>? ORDER BY group_key LIMIT 128")).bind::<Text,_>(&cursor).load::<CheckpointGroup>(&mut conn)?;
                if rows.is_empty() {
                    break;
                }
                for row in rows {
                    cursor = row.group_key;
                    let value: Value = serde_json::from_str(&row.included)?;
                    if table == "dashboard_checkpoint_groups" {
                        excluded += value["excluded_fixture_events"].as_u64().unwrap_or(0);
                        for group in value["groups"].as_array().into_iter().flatten() {
                            for key in ["count", "labeled", "correct"] {
                                aggregate[key] = json!(
                                    aggregate[key].as_u64().unwrap_or(0)
                                        + group[key].as_u64().unwrap_or(0)
                                );
                            }
                        }
                    }
                    let usage =
                        crate::summary::usage_totals(&[&json!({"groups":[usage_group]}), &value]);
                    usage_group = usage;
                    usage_group["count"] = usage_group["calls"].clone();
                }
            }
        }
        let usage = crate::summary::usage_totals(&[&json!({"groups":[usage_group]})]);
        Ok((
            json!({"groups":[aggregate],"excluded_fixture_events":excluded}),
            usage,
        ))
    }
    pub fn orchestration(&self) -> Result<Value> {
        orchestration::report(&mut self.connection()?, 0, 200)
    }
    #[cfg(test)]
    pub fn session(&self, id: &str) -> Result<Option<Value>> {
        // Test-only compatibility reader for equivalence against the original
        // reducer. Production routes never assemble an entire session.
        let mut conn = self.connection()?;
        let Some(mut value) = cached(&mut conn, &format!("session:{id}"))? else {
            return Ok(None);
        };
        for key in [
            "event_count",
            "task_count",
            "completed_tasks",
            "prompt_count",
            "phases",
            "search_text",
            "preview",
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
        value["events"]=json!(documents(diesel::sql_query("SELECT document FROM dashboard_workflow_annotations WHERE session=? ORDER BY json_extract(document,'$.payload.occurred_at'),json_extract(document,'$.recorded_at_us'),position").bind::<Text,_>(id).load::<Document>(&mut conn)?)?);
        value["metadata_history"] = json!(value["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event_type"] == "workflow.session_updated")
            .map(|e| e["payload"].clone())
            .collect::<Vec<_>>());
        value["prompts"] = json!(self.session_prompts(id, 0, i64::MAX as usize)?.unwrap().0);
        value["initial_goal"] = value["prompts"][0]["text"]
            .as_str()
            .map_or(json!(""), |s| json!(s));
        let heads=documents(diesel::sql_query("SELECT document FROM dashboard_session_entities WHERE session=? AND category='task' ORDER BY json_extract(document,'$.first_at'), (SELECT MIN(position) FROM dashboard_session_facts f WHERE f.session=dashboard_session_entities.session AND f.category='task' AND f.entity=dashboard_session_entities.entity), entity").bind::<Text,_>(id).load::<Document>(&mut conn)?)?;
        let mut tasks = serde_json::Map::new();
        for head in heads {
            let key = head["task"]["task_id"].as_str().unwrap();
            let mut task = session::task(&mut conn, id, key, 0, i64::MAX as usize)?.unwrap();
            for todo in task["todos"].as_object_mut().unwrap().values_mut() {
                todo.as_object_mut().unwrap().remove("history_count");
            }
            for key in [
                "todo_list",
                "todos_done",
                "todos_total",
                "todo_count",
                "plan_count",
                "detail_offset",
                "detail_limit",
                "detail_more",
            ] {
                task.as_object_mut().unwrap().remove(key);
            }
            tasks.insert(key.into(), task);
        }
        value["tasks"] = Value::Object(tasks);
        value["task_list"] = json!(crate::workflow::ordered_tasks(&value["tasks"]));
        Ok(Some(value))
    }
    pub fn orchestration_page(&self, offset: usize) -> Result<Value> {
        orchestration::report(&mut self.connection()?, offset, 200)
    }
    pub fn session_without_events(&self, id: &str) -> Result<Option<Value>> {
        let mut conn = self.connection()?;
        cached(&mut conn, &format!("session:{id}"))
    }

    pub fn session_timeline(
        &self,
        id: &str,
        view: &str,
        ordering: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<Value>, usize)>> {
        let limit = limit.min(1000);
        let mut conn = self.connection()?;
        if cached(&mut conn, &format!("directory:{id}"))?.is_none() {
            return Ok(None);
        }
        if view == "all" {
            let condition="session=? AND (json_extract(document,'$.event_type')!='workflow.prompt_recorded' OR COALESCE(json_extract(document,'$.payload.part_index'),0)=0)";
            let total = diesel::sql_query(format!(
                "SELECT COUNT(*) AS position FROM dashboard_workflow_annotations WHERE {condition}"
            ))
            .bind::<Text, _>(id)
            .get_result::<Position>(&mut conn)?
            .position as usize;
            let order = if ordering == "newest" { "DESC" } else { "ASC" };
            let rows=documents(diesel::sql_query(format!("SELECT document FROM dashboard_workflow_annotations WHERE {condition} ORDER BY json_extract(document,'$.payload.occurred_at') {order},json_extract(document,'$.recorded_at_us') {order},position {order} LIMIT ? OFFSET ?"))
                .bind::<Text,_>(id).bind::<BigInt,_>(limit as i64).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(&mut conn)?)?;
            return Ok(Some((rows, total)));
        }
        let total = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_timeline_rows WHERE session=? AND view=? AND ordering='oldest'")
            .bind::<Text,_>(id).bind::<Text,_>(view).get_result::<Position>(&mut conn)?.position as usize;
        let sql = if ordering == "newest" {
            "SELECT document FROM (SELECT *,SUM(CASE WHEN json_extract(document,'$.category')='prompt' THEN 1 ELSE 0 END) OVER (ORDER BY ordinal) AS chapter FROM dashboard_timeline_rows WHERE session=? AND view=? AND ordering='oldest') ORDER BY chapter DESC,CASE WHEN json_extract(document,'$.category')='prompt' THEN 0 ELSE 1 END,ordinal DESC LIMIT ? OFFSET ?"
        } else {
            "SELECT document FROM dashboard_timeline_rows WHERE session=? AND view=? AND ordering='oldest' ORDER BY ordinal LIMIT ? OFFSET ?"
        };
        let rows = diesel::sql_query(sql)
            .bind::<Text, _>(id)
            .bind::<Text, _>(view)
            .bind::<BigInt, _>(limit as i64)
            .bind::<BigInt, _>(offset.min(i64::MAX as usize) as i64)
            .load::<Document>(&mut conn)?;
        Ok(Some((documents(rows)?, total)))
    }
    /// Load only the requested prompt rows, leaving the session's other arrays in SQLite.
    pub fn session_prompts(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<Value>, usize)>> {
        let limit = limit.min(100);
        let mut conn = self.connection()?;
        if cached(&mut conn, &format!("directory:{id}"))?.is_none() {
            return Ok(None);
        }
        let total = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='prompt' AND json_extract(document,'$.complete')=1")
            .bind::<Text,_>(id).get_result::<Position>(&mut conn)?.position as usize;
        // Order by the first event in the same chronological order as the reducer.
        let rows = diesel::sql_query("SELECT json_set(e.document, '$.prompt_id', e.entity) AS document FROM dashboard_session_entities e WHERE e.session=? AND e.category='prompt' AND json_extract(e.document,'$.complete')=1 ORDER BY (SELECT json_extract(d.document,'$.payload.occurred_at') FROM dashboard_session_facts f JOIN dashboard_events d ON d.id=f.id WHERE f.session=e.session AND f.category=e.category AND f.entity=e.entity ORDER BY json_extract(d.document,'$.payload.occurred_at'), d.recorded_at_us, f.position LIMIT 1), (SELECT d.recorded_at_us FROM dashboard_session_facts f JOIN dashboard_events d ON d.id=f.id WHERE f.session=e.session AND f.category=e.category AND f.entity=e.entity ORDER BY json_extract(d.document,'$.payload.occurred_at'), d.recorded_at_us, f.position LIMIT 1), (SELECT f.position FROM dashboard_session_facts f JOIN dashboard_events d ON d.id=f.id WHERE f.session=e.session AND f.category=e.category AND f.entity=e.entity ORDER BY json_extract(d.document,'$.payload.occurred_at'), d.recorded_at_us, f.position LIMIT 1) LIMIT ? OFFSET ?")
            .bind::<Text,_>(id).bind::<BigInt,_>(limit as i64).bind::<BigInt,_>(offset as i64).load::<Document>(&mut conn)?;
        let prompts = documents(rows)?
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                json!({
                    "prompt_id": row["prompt_id"], "number": offset + index + 1,
                    "preview": row["preview"], "text": row["text"], "occurred_at": row["first_at"]
                })
            })
            .collect();
        Ok(Some((prompts, total)))
    }
    /// Assemble the stored turn context for the given prompt IDs, one bounded read each.
    pub fn session_context(
        &self,
        id: &str,
        prompt_ids: &[String],
    ) -> Result<BTreeMap<String, Value>> {
        let mut conn = self.connection()?;
        let mut turns = BTreeMap::new();
        for prompt_id in prompt_ids {
            if let Some(turn) = context_turn(&mut conn, id, prompt_id)? {
                turns.insert(prompt_id.clone(), turn);
            }
        }
        Ok(turns)
    }
    /// The newest turns that have stored context but no complete stored prompt.
    pub fn session_orphan_context(&self, id: &str) -> Result<Vec<Value>> {
        let mut conn = self.connection()?;
        let ids = diesel::sql_query("SELECT f.entity AS document FROM dashboard_session_facts f WHERE f.session=? AND f.category='context' AND NOT EXISTS (SELECT 1 FROM dashboard_session_entities e WHERE e.session=f.session AND e.category='prompt' AND e.entity=f.entity AND json_extract(e.document,'$.complete')=1) GROUP BY f.entity ORDER BY MAX(f.position) DESC LIMIT ?")
            .bind::<Text,_>(id).bind::<BigInt,_>(ORPHAN_TURNS as i64).load::<Document>(&mut conn)?;
        let mut turns = Vec::new();
        for row in ids {
            if let Some(mut turn) = context_turn(&mut conn, id, &row.document)? {
                turn["prompt_id"] = json!(row.document);
                turns.push(turn);
            }
        }
        turns.sort_by(|a, b| a["occurred_at"].as_str().cmp(&b["occurred_at"].as_str()));
        Ok(turns)
    }
    /// Read task projections without loading the session's timeline or prompt bodies.
    pub fn session_tasks(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<Value>, usize)>> {
        let limit = limit.min(100);
        let mut conn = self.connection()?;
        if cached(&mut conn, &format!("directory:{id}"))?.is_none() {
            return Ok(None);
        }
        let total = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='task'")
            .bind::<Text,_>(id).get_result::<Position>(&mut conn)?.position as usize;
        let rows = diesel::sql_query(format!("SELECT h.document FROM dashboard_session_entities h WHERE h.session=? AND h.category='task' ORDER BY {} LIMIT ? OFFSET ?", session::entity_order()))
            .bind::<Text,_>(id).bind::<BigInt,_>(limit as i64).bind::<BigInt,_>(offset as i64).load::<Document>(&mut conn)?;
        let mut tasks = Vec::new();
        for head in documents(rows)? {
            if let Some(task) = session::task(
                &mut conn,
                id,
                head["task"]["task_id"].as_str().unwrap_or(""),
                0,
                0,
            )? {
                tasks.push(task);
            }
        }
        Ok(Some((tasks, total)))
    }
    pub fn session_task(
        &self,
        id: &str,
        task: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<Value>> {
        session::task(
            &mut self.connection()?,
            id,
            task,
            offset,
            limit.clamp(1, 100),
        )
    }
    pub fn sessions(&self) -> Result<Vec<Value>> {
        self.sessions_page(0, 200)
    }
    pub fn sessions_page(&self, offset: usize, limit: usize) -> Result<Vec<Value>> {
        let mut rows=documents(diesel::sql_query("SELECT json_set(d.document, '$.verdict', json(COALESCE(json_extract(s.document, '$.verdict'), json_extract(d.document, '$.verdict')))) AS document FROM dashboard_reports d LEFT JOIN dashboard_reports s ON s.key='session-summary:' || substr(d.key,11) WHERE d.key >= 'directory:' AND d.key < 'directory;' ORDER BY json_extract(d.document,'$.last_at') DESC,d.key LIMIT ? OFFSET ?").bind::<BigInt,_>(limit.min(200) as i64).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(&mut self.connection()?)?)?;
        rows.sort_by(|a, b| b["last_at"].as_str().cmp(&a["last_at"].as_str()));
        Ok(rows)
    }
    pub fn session_count(&self) -> Result<usize> {
        let count = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_reports WHERE key >= 'directory:' AND key < 'directory;'")
            .get_result::<Position>(&mut self.connection()?)?.position;
        Ok(count as usize)
    }
    /// Fold bounded pages into local calendar days under one consistent read snapshot.
    pub fn checkpoint_days(
        &self,
        fixtures: bool,
        timezone: Tz,
    ) -> Result<Vec<crate::summary::DailyConfidence>> {
        let mut conn = self.connection()?;
        conn.transaction::<_, anyhow::Error, _>(|conn| {
            let mut cursor = 0_i64;
            let mut days = BTreeMap::<i64, (usize, f64)>::new();
            loop {
                let rows = diesel::sql_query("SELECT position, document FROM dashboard_events WHERE partition='checkpoint' AND position > ? ORDER BY position LIMIT ?")
                    .bind::<BigInt, _>(cursor).bind::<BigInt, _>(CATCH_UP_BATCH as i64).load::<ChartDocument>(conn)?;
                if rows.is_empty() { break; }
                for item in &rows {
                    let mut row: Value = serde_json::from_str(&item.document)?;
                    if let Some(date) = row["recorded_at_us"].as_i64().and_then(DateTime::<Utc>::from_timestamp_micros) {
                        row["recorded_at_us"] = json!(date.with_timezone(&timezone).naive_local().and_utc().timestamp_micros());
                    }
                    for day in crate::summary::daily_checkpoint_confidence(&[row], fixtures) {
                        let entry = days.entry(day.day).or_default();
                        entry.0 += day.samples;
                        entry.1 += (day.mean - entry.1) * day.samples as f64 / entry.0 as f64;
                    }
                }
                cursor = rows.last().unwrap().position;
                if rows.len() < CATCH_UP_BATCH { break; }
            }
            Ok(days.into_iter().map(|(day, (samples, mean))| crate::summary::DailyConfidence { day, samples, mean }).collect())
        })
    }
}
fn partition(kind: &str) -> &'static str {
    match kind {
        "jev.checkpoint" | "jev.checkpoint_outcome" => "checkpoint",
        "jev.route" | "jev.outcome" => "route",
        // Turn context is read by prompt, never replayed into the session timeline.
        "workflow.context_recorded" => "context",
        s if s.starts_with("workflow.") => "workflow",
        s if s.starts_with("orchestration.") => "orchestration",
        _ => "other",
    }
}
fn affected(row: &Value, sessions: &mut BTreeSet<String>, parts: &mut BTreeSet<&'static str>) {
    let part = partition(row["event_type"].as_str().unwrap_or(""));
    parts.insert(part);
    if part == "workflow" {
        if let Some(id) = row["payload"]["session_id"].as_str() {
            sessions.insert(id.into());
        }
    }
}
/// Most context chunks read for one turn, and most prompt-less turns shown.
const CONTEXT_CHUNKS: usize = 300;
const ORPHAN_TURNS: usize = 10;

fn context_turn(
    conn: &mut SqliteConnection,
    session: &str,
    prompt_id: &str,
) -> Result<Option<Value>> {
    let rows = documents(diesel::sql_query("SELECT e.document FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=? AND f.category='context' AND f.entity=? ORDER BY json_extract(e.document,'$.payload.entry_index'), json_extract(e.document,'$.payload.part_index'), e.recorded_at_us LIMIT ?")
        .bind::<Text,_>(session).bind::<Text,_>(prompt_id).bind::<BigInt,_>(CONTEXT_CHUNKS as i64 + 1).load::<Document>(conn)?)?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(crate::workflow::context_turn(&rows, CONTEXT_CHUNKS)))
}
fn documents(rows: Vec<Document>) -> Result<Vec<Value>> {
    rows.into_iter()
        .map(|r| Ok(serde_json::from_str(&r.document)?))
        .collect()
}

fn cached(conn: &mut SqliteConnection, key: &str) -> Result<Option<Value>> {
    diesel::sql_query("SELECT document FROM dashboard_reports WHERE key=?")
        .bind::<Text, _>(key)
        .get_result::<Document>(conn)
        .optional()?
        .map(|r| Ok(serde_json::from_str(&r.document)?))
        .transpose()
}
fn save(conn: &mut SqliteConnection, key: &str, value: &Value) -> Result<()> {
    diesel::sql_query("INSERT INTO dashboard_reports(key,document) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET document=excluded.document").bind::<Text,_>(key).bind::<Text,_>(value.to_string()).execute(conn)?;
    Ok(())
}
fn increment(value: &mut Value, delta: i64) -> Result<()> {
    *value = json!(value
        .as_i64()
        .unwrap_or(0)
        .checked_add(delta)
        .ok_or_else(|| anyhow!("report counter overflow"))?);
    Ok(())
}
fn adjust_raw(raw: &mut Value, row: &Value, direction: i64) -> Result<()> {
    increment(&mut raw["events"], direction)?;
    let kind = row["event_type"].as_str().unwrap();
    increment(&mut raw["event_types"][kind], direction)?;
    if raw["event_types"][kind] == 0 {
        raw["event_types"].as_object_mut().unwrap().remove(kind);
    }
    let p = &row["payload"];
    if let Some(outcome) = p["outcome"].as_str() {
        increment(&mut raw["outcomes"][outcome], direction)?;
        if raw["outcomes"][outcome] == 0 {
            raw["outcomes"].as_object_mut().unwrap().remove(outcome);
        }
    }
    for key in TOTALS {
        if let Some(value) = p[*key].as_i64() {
            // Telemetry totals must never poison the committed source stream.
            // Once unrepresentable, retain an explicit unknown until a rebuild.
            if raw["overflowed_totals"][*key] == true {
                continue;
            }
            let previous = i128::from(raw["totals"][*key].as_i64().unwrap_or(0));
            let total = previous + i128::from(value) * i128::from(direction);
            if let Ok(total) = i64::try_from(total) {
                raw["totals"][*key] = json!(total);
            } else {
                raw["totals"][*key] = Value::Null;
                raw["overflowed_totals"][*key] = json!(true);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        store: ReportStore,
        path: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("harness-reports-{}.sqlite", uuid::Uuid::new_v4()));
            let store = ReportStore::open(path.to_str().unwrap()).unwrap();
            store.source_connection().unwrap().batch_execute("CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,aggregate_id TEXT NOT NULL,aggregate_type TEXT NOT NULL DEFAULT 'AuditEvent',event_type TEXT NOT NULL,payload TEXT NOT NULL,sequence BIGINT NOT NULL,timestamp_utc_us BIGINT NOT NULL)").unwrap();
            Self { store, path }
        }
        fn append(&self, id: &str, kind: &str, payload: Value, at: i64) {
            self.source(
                id,
                &json!({"event_type":kind,"payload":payload}).to_string(),
                at,
            );
        }
        fn source(&self, id: &str, payload: &str, at: i64) {
            diesel::sql_query("INSERT INTO events(aggregate_id,event_type,payload,sequence,timestamp_utc_us) VALUES(?,'AuditEventRecorded',?,1,?)").bind::<Text,_>(id).bind::<Text,_>(payload).bind::<BigInt,_>(at).execute(&mut self.store.source_connection().unwrap()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.store.projection_url));
            }
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
            let _ = std::fs::remove_file(format!("{}-shm", self.path.display()));
        }
    }
    #[test]
    fn append_only_refresh_reuses_unchanged_annotations() {
        let f = Fixture::new();
        for i in 0..128 {
            f.append(&format!("d{i}"),"workflow.decision",json!({"session_id":"s","task_id":"t","occurred_at":format!("{i:04}"),"description":"decision"}),i);
        }
        f.store.refresh().unwrap();
        let mut conn = f.store.connection().unwrap();
        conn.batch_execute("CREATE TABLE annotation_writes(n INTEGER); CREATE TRIGGER annotation_insert AFTER INSERT ON dashboard_workflow_annotations BEGIN INSERT INTO annotation_writes VALUES(1); END;").unwrap();
        f.append("new","workflow.decision",json!({"session_id":"s","task_id":"t","occurred_at":"0200","description":"new decision"}),200);
        f.store.refresh().unwrap();
        assert_eq!(
            diesel::sql_query("SELECT COUNT(*) AS position FROM annotation_writes")
                .get_result::<Position>(&mut conn)
                .unwrap()
                .position,
            1
        );
        assert_eq!(
            f.store
                .session_timeline("s", "all", "oldest", 125, 10)
                .unwrap()
                .unwrap()
                .0
                .len(),
            4
        );
        let reopened = ReportStore::open(&f.store.database_url).unwrap();
        assert_eq!(reopened.refresh().unwrap(), 0);
        assert_eq!(
            diesel::sql_query("SELECT COUNT(*) AS position FROM annotation_writes")
                .get_result::<Position>(&mut conn)
                .unwrap()
                .position,
            1
        );
    }

    #[test]
    fn group_and_orchestration_pages_keep_exact_global_totals() {
        let f = Fixture::new();
        for i in 0..205 {
            f.append(&format!("c{i}"),"jev.checkpoint",json!({"family":format!("f{i:03}"),"call_id":format!("c{i}"),"recommendation_confidence":0.8,"input_tokens":2,"output_tokens":1,"tokens":3}),i);
            f.append(&format!("w{i}"),"orchestration.worker_completed",json!({"run_id":"r","worker_id":format!("w{i:03}"),"parent_session_id":"parent","total_tokens":3,"duration_ms":i}),i);
        }
        while f.store.refresh().unwrap() > 0 {}
        let first = f.store.groups_page("checkpoint", true, 0).unwrap();
        let last = f.store.groups_page("checkpoint", true, 200).unwrap();
        assert_eq!(first["groups"].as_array().unwrap().len(), 200);
        assert_eq!(last["groups"].as_array().unwrap().len(), 5);
        let (totals, usage) = f.store.group_totals(true).unwrap();
        assert_eq!(totals["groups"][0]["count"], 205);
        let all = json!({"groups":first["groups"].as_array().unwrap().iter().chain(last["groups"].as_array().unwrap()).cloned().collect::<Vec<_>>()});
        assert_eq!(usage, crate::summary::usage_totals(&[&all]));
        let report = f.store.orchestration().unwrap();
        assert_eq!(report["worker_count"], 205);
        assert_eq!(report["workers"].as_array().unwrap().len(), 200);
        assert_eq!(report["worker_groups"][0]["tokens"]["total"], 615.0);
        assert_eq!(
            f.store.orchestration_page(200).unwrap()["workers"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        let diagram = f.store.orchestration_diagram(Some("parent"), 0, 0).unwrap();
        assert_eq!(diagram["child_count"], 205);
        assert_eq!(diagram["children"].as_array().unwrap().len(), 200);
        let next = f
            .store
            .orchestration_diagram(Some("parent"), 200, 0)
            .unwrap();
        assert_eq!(next["session"]["id"], "parent");
        assert_eq!(next["children"].as_array().unwrap().len(), 5);
        assert_eq!(next["hidden_children"], 0);
    }

    #[test]
    fn disk_history_memory_is_bounded_and_restart_keeps_pages() {
        const CHILD: &str = "HARNESS_AUDIT_MEMORY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reports::tests::disk_history_memory_is_bounded_and_restart_keeps_pages",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            eprintln!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let f = Fixture::new();
        let rss = || {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|s| s.starts_with("VmHWM:"))
                        .and_then(|s| s.split_whitespace().nth(1))
                        .and_then(|s| s.parse::<u64>().ok())
                })
        };
        let mut before = None;
        for end in [512, 8192] {
            let start = if end == 512 { 0 } else { 512 };
            let mut conn = f.store.source_connection().unwrap();
            conn.transaction::<_,anyhow::Error,_>(|conn| {
                for n in start..end {
                    let payload=json!({"session_id":"large","task_id":"one","occurred_at":format!("{n:08}"),"description":"x".repeat(1500),"revision":n});
                    diesel::sql_query("INSERT INTO events(aggregate_id,event_type,payload,sequence,timestamp_utc_us) VALUES(?,'AuditEventRecorded',?,1,?)")
                        .bind::<Text,_>(format!("large-{n}"))
                        .bind::<Text,_>(json!({"event_type":"workflow.plan_revised","payload":payload}).to_string()).bind::<BigInt,_>(n as i64).execute(conn)?;
                } Ok(())
            }).unwrap();
            drop(conn);
            while f.store.refresh().unwrap() > 0 {}
            let detail = f
                .store
                .session_task("large", "one", end - 5, 5)
                .unwrap()
                .unwrap();
            assert_eq!(detail["plan_count"], end);
            assert_eq!(detail["plan_revisions"].as_array().unwrap().len(), 5);
            assert_eq!(detail["plan_revisions"][0]["revision"], end - 5);
            let (page, total) = f
                .store
                .session_timeline("large", "all", "oldest", end - 5, 5)
                .unwrap()
                .unwrap();
            assert_eq!(total, end);
            assert_eq!(page.len(), 5);
            let summary = f.store.session_without_events("large").unwrap().unwrap();
            assert!(summary.to_string().len() < 10000);
            if end == 512 {
                before = rss();
            }
        }
        let reopened = ReportStore::open(&f.store.database_url).unwrap();
        assert_eq!(reopened.refresh().unwrap(), 0);
        let (page, total) = reopened
            .filtered_timeline("large", false, false, 8180, 12, |row| {
                row["category"] == "plan"
            })
            .unwrap()
            .unwrap();
        assert_eq!(total, 8192);
        assert_eq!(page.len(), 12);
        if let (Some(before), Some(after)) = (before, rss()) {
            eprintln!(
                "disk history memory: 512 events {before} KiB peak; 8192 events {after} KiB peak"
            );
            assert!(
                after.saturating_sub(before) < 32 * 1024,
                "peak grew by {} KiB",
                after.saturating_sub(before)
            );
            assert!(after < 96 * 1024, "peak exceeded 96 MiB: {after} KiB");
        }
    }

    #[test]
    fn disk_projection_matches_reducer_with_histories_pairs_and_filtered_signals() {
        let f = Fixture::new();
        let inputs = [
            ("session_started", json!({"agent":"Codex"})),
            ("task_started", json!({"name":"Repair"})),
            ("plan_required", json!({"description":"Confirm"})),
            (
                "todo_created",
                json!({"todo_id":"a","description":"First","criterion":"pass","status":"pending"}),
            ),
            (
                "prompt_recorded",
                json!({"prompt_id":"p","part_index":1,"part_count":2,"text":"world"}),
            ),
            (
                "tool_started",
                json!({"tool_call_id":"call","tool_name":"shell","tool_command":"check"}),
            ),
            ("decision", json!({"description":"Keep"})),
            (
                "tool_completed",
                json!({"tool_call_id":"call","tool_name":"shell","outcome":"succeeded","exit_code":0}),
            ),
            (
                "todo_updated",
                json!({"todo_id":"a","description":"First","criterion":"pass","status":"completed","evidence":"done"}),
            ),
            ("plan_confirmed", json!({"description":"Confirmed"})),
            ("verification", json!({"exit_code":0})),
            ("task_completed", json!({"outcome":"Fixed"})),
            (
                "prompt_recorded",
                json!({"prompt_id":"p","part_index":0,"part_count":2,"text":"hello "}),
            ),
        ];
        for (i, (kind, extra)) in inputs.into_iter().enumerate() {
            let mut payload =
                json!({"session_id":"s","task_id":"t","occurred_at":format!("{i:03}")});
            payload
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            f.append(
                &format!("event-{i}"),
                &format!("workflow.{kind}"),
                payload,
                i as i64,
            );
        }
        f.store.refresh().unwrap();
        for round in 0..3 {
            if round == 1 {
                f.append("event-12","workflow.prompt_recorded",json!({"session_id":"s","task_id":"t","occurred_at":"001","prompt_id":"p","part_index":0,"part_count":2,"text":"hello "}),20);
            }
            if round == 2 {
                f.append("event-8","workflow.todo_updated",json!({"session_id":"other","task_id":"u","occurred_at":"030","todo_id":"a","description":"Moved","status":"pending"}),30);
            }
            f.store.refresh().unwrap();
            let rows = f.store.events(&EventFilter::default(), 1000).unwrap();
            let expected = crate::workflow::sessions(&rows)
                .into_iter()
                .find(|s| s["session_id"] == "s")
                .unwrap();
            assert_eq!(f.store.session("s").unwrap().unwrap(), expected);
            for newest in [false, true] {
                let raw = expected["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| {
                        e["event_type"] != "workflow.prompt_recorded"
                            || e["payload"]["part_index"] == 0
                    })
                    .filter(|e| e["category"] != "decision")
                    .cloned()
                    .collect::<Vec<_>>();
                let mut signal = crate::workflow::signal_items(&raw);
                if newest {
                    signal = crate::workflow::newest_first(signal);
                }
                let (actual, total) = f
                    .store
                    .filtered_timeline("s", true, newest, 0, 100, |e| e["category"] != "decision")
                    .unwrap()
                    .unwrap();
                assert_eq!(total, signal.len());
                assert_eq!(actual, signal);
            }
        }
    }

    #[test]
    fn turn_context_is_read_by_prompt_and_stays_out_of_the_timeline() {
        let f = Fixture::new();
        f.append("prompt-1", "workflow.prompt_recorded", json!({"session_id":"s", "prompt_id":"p1", "part_count":1, "part_index":0, "text":"first", "occurred_at":"2026-01-01T00:00:01Z"}), 1);
        let context = |id: &str, prompt: &str, entry: &str, index: u64, text: &str, at: i64| {
            f.append(id, "workflow.context_recorded", json!({"session_id":"s","task_id":"t","prompt_id":prompt,"entry_id":entry,"entry_index":index,"role":"assistant","kind":"text","part_index":0,"part_count":1,"text":text,"entry_bytes":text.len(),"truncated":false,"occurred_at":format!("2026-01-01T00:00:0{at}Z")}), at);
        };
        context("c-b", "p1", "b", 1, "second <b>", 3);
        context("c-a", "p1", "a", 0, "first", 2);
        context("c-orphan", "p9", "z", 0, "stray", 4);
        f.store.refresh().unwrap();
        let turns = f.store.session_context("s", &["p1".to_owned()]).unwrap();
        assert_eq!(turns["p1"]["entries"][0]["text"], "first");
        assert_eq!(turns["p1"]["entries"][1]["text"], "second <b>");
        assert!(!turns.contains_key("p9"));
        let orphans = f.store.session_orphan_context("s").unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0]["prompt_id"], "p9");
        assert_eq!(f.store.session_prompts("s", 0, 10).unwrap().unwrap().1, 1);
        let (rows, _) = f.store.session_timeline("s", "all", "oldest", 0, 50).unwrap().unwrap();
        assert!(rows.iter().all(|row| row["event_type"] != "workflow.context_recorded"));
    }
    #[test]
    fn prompt_pages_keep_order_and_report_total() {
        let f = Fixture::new();
        for i in 1..=3 {
            f.append(&format!("prompt-{i}"), "workflow.prompt_recorded", json!({"session_id":"s", "prompt_id":format!("p{i}"), "part_count":1, "part_index":0, "text":format!("prompt {i}"), "occurred_at":format!("2026-01-01T00:00:0{i}Z")}), i);
        }
        f.store.refresh().unwrap();
        let (rows, total) = f.store.session_prompts("s", 1, 1).unwrap().unwrap();
        assert_eq!(total, 3);
        assert_eq!(rows[0]["number"], 2);
        assert_eq!(rows[0]["text"], "prompt 2");
        assert!(f.store.session_prompts("missing", 0, 1).unwrap().is_none());
    }
    #[test]
    fn catch_up_commits_bounded_batches_and_resumes_from_cursor() {
        let f = Fixture::new();
        for i in 0..(CATCH_UP_BATCH + 3) {
            f.append(&format!("event-{i}"), "other", json!({}), i as i64);
        }
        assert_eq!(f.store.refresh().unwrap(), CATCH_UP_BATCH);
        assert_eq!(f.store.raw_summary().unwrap()["events"], CATCH_UP_BATCH);
        let reopened = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.refresh().unwrap(), 3);
        assert_eq!(
            reopened.raw_summary().unwrap()["events"],
            CATCH_UP_BATCH + 3
        );
        assert_eq!(reopened.refresh().unwrap(), 0);
    }
    #[test]
    fn orchestration_incremental_facts_match_reducer_after_late_events_and_replacements() {
        let f = Fixture::new();
        f.append(
            "a",
            "orchestration.worker_started",
            json!({"run_id":"r", "worker_id":"w", "model":"old"}),
            10,
        );
        f.append(
            "b",
            "orchestration.worker_completed",
            json!({"run_id":"r", "worker_id":"w", "total_tokens":7}),
            20,
        );
        f.append(
            "c",
            "orchestration.decision_evaluated",
            json!({"id":"d", "parent_session_id":"s", "usage":{"total_tokens":3}}),
            12,
        );
        f.append("ignored", "orchestration.unknown", json!({}), 1);
        f.store.refresh().unwrap();
        // A late lifecycle event and a replacement moving the old identity exercise both affected keys.
        f.append(
            "late",
            "orchestration.worker_started",
            json!({"run_id":"r", "worker_id":"w", "model":"late"}),
            5,
        );
        f.append(
            "a",
            "orchestration.worker_failed",
            json!({"run_id":"other", "worker_id":"w"}),
            30,
        );
        f.store.refresh().unwrap();
        let rows = documents(
            diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                .load::<Document>(&mut f.store.connection().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            f.store.orchestration().unwrap(),
            crate::orchestration::summarize(&rows)
        );
        f.append("b", "unrelated", json!({}), 40);
        f.store.refresh().unwrap();
        let rows = documents(
            diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                .load::<Document>(&mut f.store.connection().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            f.store.orchestration().unwrap(),
            crate::orchestration::summarize(&rows)
        );
        let reopened = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.refresh().unwrap(), 0);
        assert_eq!(
            reopened.orchestration().unwrap(),
            crate::orchestration::summarize(&rows)
        );
    }

    #[test]
    fn checkpoint_chart_rows_retain_confidence_and_fixture_identity_without_prompts() {
        let f = Fixture::new();
        f.append(
            "confidence",
            "jev.checkpoint",
            json!({"recommendation_confidence":0,
            "model_requested":"fixture", "prompt":"private prompt"}),
            1,
        );
        f.store.refresh().unwrap();
        assert!(f
            .store
            .checkpoint_days(false, chrono_tz::UTC)
            .unwrap()
            .is_empty());
        assert_eq!(
            f.store.checkpoint_days(true, chrono_tz::UTC).unwrap()[0].mean,
            0.0
        );
    }

    #[test]
    fn reducer_equivalence_resume_and_late_source_time() {
        let f = Fixture::new();
        f.append("c","jev.checkpoint",json!({"call_id":"a","family":"plan","model_requested":"m","baseline_action":"run","recommendation":"run","jev_input_tokens":3}),100);
        f.append(
            "r",
            "jev.route",
            json!({"call_id":"r","model_requested":"m","observed_recommendation":"run"}),
            101,
        );
        f.append(
            "w",
            "orchestration.worker_started",
            json!({"run_id":"r","worker_id":"w","role":"build"}),
            102,
        );
        f.append(
            "s",
            "workflow.session_started",
            json!({"session_id":"s","occurred_at":"2026-10-14T00:00:02Z","session_name":"Initial"}),
            103,
        );
        assert_eq!(f.store.refresh().unwrap(), 4);
        // Reopening must not rewrite any derived row when the cursor is current.
        let reopened = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        reopened.connection().unwrap().batch_execute("CREATE TRIGGER no_report_rewrite BEFORE UPDATE ON dashboard_reports BEGIN SELECT RAISE(ABORT,'unexpected rewrite'); END;").unwrap();
        assert_eq!(reopened.refresh().unwrap(), 0);
        reopened
            .connection()
            .unwrap()
            .batch_execute("DROP TRIGGER no_report_rewrite")
            .unwrap();
        // Producer and recorded time may go backwards; source identity still advances.
        f.append(
            "l",
            "jev.checkpoint_outcome",
            json!({"call_id":"a","outcome":"correct"}),
            1,
        );
        f.append(
            "late",
            "workflow.session_updated",
            json!({"session_id":"s","occurred_at":"2026-10-14T00:00:01Z","session_name":"Older"}),
            0,
        );
        assert_eq!(reopened.refresh().unwrap(), 2);
        let mut rows = reopened
            .events(&EventFilter::default(), usize::MAX)
            .unwrap();
        rows.sort_by_key(|r| r["id"].as_str().unwrap().to_string());
        assert_eq!(
            reopened.checkpoint_groups(false).unwrap(),
            crate::summary::checkpoint_groups(&rows, false)
        );
        assert_eq!(
            reopened.route_groups(false).unwrap(),
            crate::summary::route_groups(&rows, false)
        );
        assert_eq!(
            reopened.orchestration().unwrap(),
            crate::orchestration::summarize(&rows)
        );
        assert_eq!(
            reopened.session("s").unwrap().unwrap(),
            crate::workflow::sessions(&rows)[0]
        );
        assert_eq!(reopened.raw_summary().unwrap()["events"], 6);
        assert_eq!(
            reopened.raw_summary().unwrap()["totals"]["jev_input_tokens"],
            3
        );
        assert!(reopened.sessions().unwrap()[0].get("events").is_none());
        assert_eq!(
            reopened.checkpoint_days(true, chrono_tz::UTC).unwrap(),
            crate::summary::daily_checkpoint_confidence(&rows, true)
        );
    }
    #[test]
    fn checkpoint_large_group_matches_reducer_across_batches() {
        let f = Fixture::new();
        for i in 0..520 {
            f.append(
                &format!("c{i}"),
                "jev.checkpoint",
                json!({
                    "call_id": format!("call{i}"), "family":"same", "model_requested":"real",
                    "recommendation": if i % 2 == 0 {"run"} else {"hold"},
                    "baseline_action":"run", "recommendation_confidence": (i % 101) as f64 / 100.0,
                    "jev_latency_ms": i, "jev_input_tokens": if i == 0 {i64::MAX} else {1},
                    "jev_output_tokens": 1, "jev_cost_usd": 0.001
                }),
                i as i64,
            );
        }
        f.append(
            "fixture",
            "jev.checkpoint",
            json!({"family":"same", "model_requested":"fixture", "recommendation_confidence":0.7}),
            1,
        );
        f.append(
            "old",
            "jev.checkpoint_outcome",
            json!({"call_id":"call0", "outcome":"incorrect"}),
            900,
        );
        f.append(
            "new",
            "jev.checkpoint_outcome",
            json!({"call_id":"call0", "outcome":"correct"}),
            900,
        );
        while f.store.refresh().unwrap() > 0 {}
        let rows = documents(
            diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                .load::<Document>(&mut f.store.connection().unwrap())
                .unwrap(),
        )
        .unwrap();
        for fixtures in [false, true] {
            assert_eq!(
                f.store.checkpoint_groups(fixtures).unwrap(),
                crate::summary::checkpoint_groups(&rows, fixtures)
            );
        }
    }
    #[test]
    fn checkpoint_replacements_and_late_outcomes_match_full_reducer() {
        let f = Fixture::new();
        f.append(
            "a",
            "jev.checkpoint",
            json!({"call_id":"a","family":"one","model_requested":"m","recommendation":"run"}),
            10,
        );
        f.append("b", "jev.checkpoint", json!({"call_id":"b","family":"two","model_requested":"fixture","recommendation_confidence":0.4}), 11);
        f.append(
            "o1",
            "jev.checkpoint_outcome",
            json!({"call_id":"a","outcome":"correct"}),
            20,
        );
        f.store.refresh().unwrap();
        f.append(
            "o2",
            "jev.checkpoint_outcome",
            json!({"call_id":"a","outcome":"incorrect"}),
            20,
        );
        f.append(
            "a",
            "jev.checkpoint",
            json!({"call_id":"b","family":"two","model_requested":"m","recommendation":"hold"}),
            12,
        );
        f.store.refresh().unwrap();
        let rows = documents(
            diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                .load::<Document>(&mut f.store.connection().unwrap())
                .unwrap(),
        )
        .unwrap();
        for fixtures in [false, true] {
            assert_eq!(
                f.store.checkpoint_groups(fixtures).unwrap(),
                crate::summary::checkpoint_groups(&rows, fixtures)
            );
        }
        f.append("o2", "other", json!({}), 30);
        f.store.refresh().unwrap();
        let rows = documents(
            diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                .load::<Document>(&mut f.store.connection().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            f.store.checkpoint_groups(false).unwrap(),
            crate::summary::checkpoint_groups(&rows, false)
        );
    }

    #[test]
    fn route_large_group_matches_reducer_across_batches() {
        let f = Fixture::new();
        for i in 0..520 {
            f.append(
                &format!("r{i}"),
                "jev.route",
                json!({
                    "call_id": format!("call{i}"), "source":"same", "model_requested":"real",
                    "observed_recommendation": if i % 2 == 0 {"run"} else {"hold"},
                    "recommendation_confidence": (i % 101) as f64 / 100.0,
                    "jev_latency_ms": i, "jev_input_tokens": if i == 0 {i64::MAX} else {1},
                    "jev_output_tokens": 1, "jev_cost_usd": 0.001
                }),
                i as i64,
            );
        }
        f.append(
            "fixture",
            "jev.route",
            json!({"source":"same", "model_requested":"fixture", "jev_latency_ms":99}),
            1,
        );
        f.append(
            "old",
            "jev.outcome",
            json!({"call_id":"call0", "outcome":"incorrect"}),
            900,
        );
        f.append(
            "new",
            "jev.outcome",
            json!({"call_id":"call0", "outcome":"correct"}),
            900,
        );
        while f.store.refresh().unwrap() > 0 {}
        let compare = || {
            let rows = documents(
                diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                    .load::<Document>(&mut f.store.connection().unwrap())
                    .unwrap(),
            )
            .unwrap();
            for fixtures in [false, true] {
                assert_eq!(
                    f.store.route_groups(fixtures).unwrap(),
                    crate::summary::route_groups(&rows, fixtures)
                );
            }
        };
        compare();
        f.append("r519", "other", json!({}), 901);
        f.append("new", "other", json!({}), 902);
        f.store.refresh().unwrap();
        compare();
    }

    #[test]
    fn route_replacements_and_late_outcomes_match_full_reducer() {
        let f = Fixture::new();
        f.append(
            "a",
            "jev.route",
            json!({"call_id":"a","source":"one","observed_recommendation":"run"}),
            10,
        );
        f.append(
            "b",
            "jev.route",
            json!({"call_id":"b","source":"two","model_requested":"fixture"}),
            11,
        );
        f.append(
            "o1",
            "jev.outcome",
            json!({"call_id":"a","outcome":"correct"}),
            20,
        );
        f.store.refresh().unwrap();
        f.append(
            "o2",
            "jev.outcome",
            json!({"call_id":"a","outcome":"incorrect"}),
            20,
        );
        f.append(
            "a",
            "jev.route",
            json!({"call_id":"b","source":"two","observed_recommendation":"hold"}),
            12,
        );
        f.store.refresh().unwrap();
        f.append("o2", "other", json!({}), 30);
        f.store.refresh().unwrap();
        let rows = documents(
            diesel::sql_query("SELECT document FROM dashboard_events ORDER BY position")
                .load::<Document>(&mut f.store.connection().unwrap())
                .unwrap(),
        )
        .unwrap();
        for fixtures in [false, true] {
            assert_eq!(
                f.store.route_groups(fixtures).unwrap(),
                crate::summary::route_groups(&rows, fixtures)
            );
        }
    }

    #[test]
    fn duplicate_refresh_and_replacement_do_not_double_count() {
        let f = Fixture::new();
        f.append("a", "other", json!({"outcome":"correct"}), 1);
        assert_eq!(f.store.refresh().unwrap(), 1);
        assert_eq!(f.store.refresh().unwrap(), 0);
        f.append("a", "other", json!({"outcome":"incorrect"}), 2);
        f.store.refresh().unwrap();
        assert_eq!(f.store.raw_summary().unwrap()["events"], 1);
        assert_eq!(
            f.store.raw_summary().unwrap()["outcomes"],
            json!({"incorrect":1})
        );
        assert_eq!(
            f.store.events(&EventFilter::default(), 10).unwrap().len(),
            1
        );
    }
    #[test]
    fn workflow_replacements_move_sessions_and_reassemble_late_prompts() {
        let f = Fixture::new();
        f.append(
            "start",
            "workflow.session_started",
            json!({"session_id":"a","occurred_at":"2026-01-01T00:00:02Z"}),
            2,
        );
        f.append("part", "workflow.prompt_recorded", json!({"session_id":"a","task_id":"t","prompt_id":"p","part_count":2,"part_index":1,"text":"world","occurred_at":"2026-01-01T00:00:03Z"}), 3);
        f.store.refresh().unwrap();
        f.append("first", "workflow.prompt_recorded", json!({"session_id":"a","task_id":"t","prompt_id":"p","part_count":2,"part_index":0,"text":"Hello ","occurred_at":"2026-01-01T00:00:01Z"}), 1);
        f.store.refresh().unwrap();
        assert_eq!(
            f.store.session("a").unwrap().unwrap()["initial_goal"],
            "Hello world"
        );
        // A replacement can change both the producer timestamp and the session.
        f.append(
            "part",
            "workflow.session_started",
            json!({"session_id":"b","occurred_at":"2025-12-31T00:00:00Z"}),
            0,
        );
        f.store.refresh().unwrap();
        let rows = f.store.events(&EventFilter::default(), usize::MAX).unwrap();
        let expected = crate::workflow::sessions(&rows);
        for id in ["a", "b"] {
            assert_eq!(
                f.store.session(id).unwrap().unwrap(),
                *expected.iter().find(|s| s["session_id"] == id).unwrap()
            );
        }
        assert_eq!(f.store.session("a").unwrap().unwrap()["initial_goal"], "");
    }
    #[test]
    fn persisted_session_summary_tracks_late_metadata_and_replacements() {
        let f = Fixture::new();
        f.append(
            "start",
            "workflow.session_started",
            json!({"session_id":"a","agent":"new","occurred_at":"02"}),
            2,
        );
        f.append(
            "task",
            "workflow.task_started",
            json!({"session_id":"a","task_id":"t","occurred_at":"03"}),
            3,
        );
        f.store.refresh().unwrap();
        f.append(
            "old",
            "workflow.session_ended",
            json!({"session_id":"a","agent":"old","occurred_at":"01"}),
            1,
        );
        f.store.refresh().unwrap();
        let mut conn = f.store.connection().unwrap();
        let summary = cached(&mut conn, "session-summary:a").unwrap().unwrap();
        let full = f.store.session("a").unwrap().unwrap();
        assert_eq!(summary["agent"], full["agent"]);
        assert_eq!(summary["status"], full["status"]);
        assert_eq!(summary["task_count"], 1);
        assert_eq!(summary["event_count"], 3);
        f.append(
            "task",
            "workflow.session_started",
            json!({"session_id":"b","occurred_at":"04"}),
            4,
        );
        f.store.refresh().unwrap();
        let summary = cached(&mut conn, "session-summary:a").unwrap().unwrap();
        assert_eq!(summary["task_count"], 0);
        assert_eq!(summary["event_count"], 2);
        assert_eq!(
            cached(&mut conn, "session-summary:b").unwrap().unwrap()["event_count"],
            1
        );
    }
    #[test]
    fn verdict_summary_backfills_and_readers_keep_exact_verdict() {
        let f = Fixture::new();
        f.append(
            "start",
            "workflow.session_started",
            json!({"session_id":"a","occurred_at":"2026-01-01T00:00:00Z"}),
            1,
        );
        f.append(
            "check",
            "workflow.verification",
            json!({"session_id":"a","outcome":"failed","occurred_at":"2026-01-01T00:00:01Z"}),
            2,
        );
        f.append("done", "workflow.task_blocked", json!({"session_id":"a","task_id":"t","outcome":"waiting","occurred_at":"2026-01-01T00:00:02Z"}), 3);
        f.store.refresh().unwrap();
        let expected = f.store.session("a").unwrap().unwrap()["verdict"].clone();
        let mut conn = f.store.connection().unwrap();
        assert_eq!(
            cached(&mut conn, "session-summary:a").unwrap().unwrap()["verdict"],
            expected
        );
        diesel::sql_query("UPDATE dashboard_reports SET document=json_remove(document,'$.verdict','$.verdict_version') WHERE key='session-summary:a'").execute(&mut conn).unwrap();
        assert_eq!(f.store.sessions().unwrap()[0]["verdict"], expected);
        assert_eq!(f.store.refresh().unwrap(), 1);
        assert_eq!(
            cached(&mut conn, "session-summary:a").unwrap().unwrap()["verdict"],
            expected
        );
        assert_eq!(
            f.store.session_without_events("a").unwrap().unwrap()["verdict"],
            expected
        );
        assert_eq!(f.store.refresh().unwrap(), 0);
    }
    #[test]
    fn entity_pages_use_first_chronological_fact_with_recorded_and_position_ties() {
        let f = Fixture::new();
        for (event, task, occurred, recorded) in [
            ("a-new", "a", "02", 1),
            ("b-first", "b", "01", 10),
            ("c-first", "c", "01", 5),
        ] {
            f.append(
                event,
                "workflow.task_started",
                json!({"session_id":"s","task_id":task,"occurred_at":occurred}),
                recorded,
            );
        }
        f.store.refresh().unwrap();
        // A late first fact must replace A's earlier ingestion position as its tie breaker.
        f.append(
            "a-first",
            "workflow.task_started",
            json!({"session_id":"s","task_id":"a","occurred_at":"01"}),
            10,
        );
        for (event, todo, occurred, recorded) in [
            ("todo-a-new", "a", "04", 1),
            ("todo-b-first", "b", "03", 10),
            ("todo-c-first", "c", "03", 5),
            ("todo-a-first", "a", "03", 10),
        ] {
            f.append(
                event,
                "workflow.todo_created",
                json!({"session_id":"s","task_id":"a","todo_id":todo,"occurred_at":occurred}),
                recorded,
            );
        }
        f.store.refresh().unwrap();
        let reopened = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        for (index, expected) in ["c", "b", "a"].iter().enumerate() {
            let (tasks, total) = reopened.session_tasks("s", index, 1).unwrap().unwrap();
            assert_eq!(total, 3);
            assert_eq!(tasks[0]["task_id"], *expected);
            assert_eq!(tasks[0]["sequence"], index + 1);
            let task = reopened.session_task("s", "a", index, 1).unwrap().unwrap();
            assert_eq!(task["sequence"], 3);
            assert_eq!(task["todo_list"][0]["id"], *expected);
            assert_eq!(task["todo_list"][0]["sequence"], index + 1);
        }
    }

    #[test]
    fn task_projection_pages_preserve_order_and_details() {
        let f = Fixture::new();
        f.append("later", "workflow.task_started", json!({"session_id":"s","task_id":"b","name":"Second","occurred_at":"2026-01-01T00:00:02Z"}), 2);
        f.append("earlier", "workflow.task_started", json!({"session_id":"s","task_id":"a","name":"First","occurred_at":"2026-01-01T00:00:01Z"}), 1);
        f.store.refresh().unwrap();
        let (rows, total) = f.store.session_tasks("s", 1, 1).unwrap().unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows[0]["task_id"], "b");
        assert_eq!(rows[0]["sequence"], 2);
        assert!(rows[0]["todo_list"].is_array());
        assert!(f.store.session_tasks("missing", 0, 1).unwrap().is_none());
    }
    #[test]
    fn indexed_timeline_pages_match_full_reducer_and_follow_replacements() {
        let f = Fixture::new();
        for n in 0..45 {
            f.append(&format!("event-{n}"), "workflow.decision", json!({"session_id":"s","occurred_at":format!("2026-01-01T00:00:{n:02}Z"),"description":format!("Decision {n}")}), n);
        }
        f.store.refresh().unwrap();
        let full = f.store.session("s").unwrap().unwrap();
        let raw = full["events"].as_array().unwrap().clone();
        let signal = crate::workflow::signal_items(&raw);
        for (view, order, expected) in [
            ("all", "oldest", raw.clone()),
            ("all", "newest", raw.into_iter().rev().collect()),
            ("signal", "oldest", signal.clone()),
            ("signal", "newest", crate::workflow::newest_first(signal)),
        ] {
            let (first, count) = f
                .store
                .session_timeline("s", view, order, 0, 10)
                .unwrap()
                .unwrap();
            let (rest, count2) = f
                .store
                .session_timeline("s", view, order, 10, 100)
                .unwrap()
                .unwrap();
            assert_eq!(count, expected.len());
            assert_eq!(count, count2);
            assert_eq!(first.into_iter().chain(rest).collect::<Vec<_>>(), expected);
        }
        let compact = f.store.session_without_events("s").unwrap().unwrap();
        assert!(compact.get("events").is_none());
        assert_eq!(compact["event_count"], 45);
        f.append(
            "event-0",
            "workflow.session_started",
            json!({"session_id":"other","occurred_at":"2026-01-01T00:01:00Z"}),
            60,
        );
        f.store.refresh().unwrap();
        assert_eq!(
            f.store
                .session_timeline("s", "all", "oldest", 0, 100)
                .unwrap()
                .unwrap()
                .1,
            44
        );
        assert!(f
            .store
            .session_timeline("missing", "all", "oldest", 0, 1)
            .unwrap()
            .is_none());
    }
    #[test]
    fn timeline_replay_keyset_handles_page_boundaries_and_missing_times() {
        let f = Fixture::new();
        for n in 0..260 {
            let mut payload = json!({"session_id":"s","description":format!("Decision {n}")});
            if n % 3 != 0 {
                payload["occurred_at"] = json!("2026-01-01T00:00:00Z");
            }
            f.append(&format!("event-{n}"), "workflow.decision", payload, 1);
        }
        while f.store.refresh().unwrap() != 0 {}
        let events = f.store.session("s").unwrap().unwrap()["events"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(events.len(), 260);
        assert!(events[..87]
            .iter()
            .all(|e| e["payload"]["occurred_at"].is_null()));
        assert!(events[87..]
            .iter()
            .all(|e| !e["payload"]["occurred_at"].is_null()));
        let (indexed, total) = f
            .store
            .session_timeline("s", "all", "oldest", 0, 300)
            .unwrap()
            .unwrap();
        assert_eq!(total, 260);
        assert_eq!(indexed, events);
    }
    #[test]
    fn session_entities_track_late_chunks_tool_returns_and_replacements() {
        let f = Fixture::new();
        f.append("chunk", "workflow.prompt_recorded", json!({"session_id":"a","task_id":"t","prompt_id":"p","part_count":2,"part_index":1,"text":"world","occurred_at":"03"}), 3);
        f.append(
            "tool",
            "workflow.tool_completed",
            json!({"session_id":"a","task_id":"t","tool_call_id":"c","occurred_at":"04"}),
            4,
        );
        f.store.refresh().unwrap();
        f.append("first", "workflow.prompt_recorded", json!({"session_id":"a","task_id":"t","prompt_id":"p","part_count":2,"part_index":0,"text":"hello","occurred_at":"01"}), 1);
        f.append(
            "start",
            "workflow.tool_started",
            json!({"session_id":"a","task_id":"t","tool_call_id":"c","occurred_at":"02"}),
            2,
        );
        f.store.refresh().unwrap();
        let mut conn = f.store.connection().unwrap();
        let get = |conn: &mut SqliteConnection, category: &str| -> Value {
            let row = diesel::sql_query(
                "SELECT document FROM dashboard_session_entities WHERE session='a' AND category=?",
            )
            .bind::<Text, _>(category)
            .get_result::<Document>(conn)
            .unwrap();
            serde_json::from_str(&row.document).unwrap()
        };
        assert_eq!(get(&mut conn, "prompt")["complete"], true);
        let (prompts, total) = f.store.session_prompts("a", 0, 1).unwrap().unwrap();
        assert_eq!(total, 1);
        assert_eq!(prompts[0]["text"], "helloworld");
        assert_eq!(prompts[0]["number"], 1);
        assert_eq!(get(&mut conn, "tool")["started"], true);
        assert_eq!(get(&mut conn, "tool")["returned"], true);
        f.append(
            "chunk",
            "workflow.session_started",
            json!({"session_id":"b","occurred_at":"05"}),
            5,
        );
        f.store.refresh().unwrap();
        assert_eq!(get(&mut conn, "prompt")["complete"], false);
        assert_eq!(f.store.session_prompts("a", 0, 1).unwrap().unwrap().1, 0);
        assert_eq!(get(&mut conn, "task")["events"], 3);
    }
    #[test]
    fn session_metadata_and_timeline_retract_replaced_and_late_events() {
        let f = Fixture::new();
        f.append(
            "new",
            "workflow.session_updated",
            json!({"session_id":"a","agent":"new","occurred_at":"03"}),
            3,
        );
        f.append(
            "end",
            "workflow.session_ended",
            json!({"session_id":"a","occurred_at":"04"}),
            4,
        );
        f.store.refresh().unwrap();
        f.append(
            "old",
            "workflow.session_started",
            json!({"session_id":"a","agent":"old","occurred_at":"01"}),
            1,
        );
        f.append(
            "start",
            "workflow.tool_started",
            json!({"session_id":"a","tool_call_id":"c","occurred_at":"02"}),
            2,
        );
        f.store.refresh().unwrap();
        let mut conn = f.store.connection().unwrap();
        let get = |conn: &mut SqliteConnection,
                   session: &str,
                   category: &str,
                   entity: &str|
         -> Option<Value> {
            diesel::sql_query("SELECT document FROM dashboard_session_entities WHERE session=? AND category=? AND entity=?")
                .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity)
                .get_result::<Document>(conn).optional().unwrap().map(|r| serde_json::from_str(&r.document).unwrap())
        };
        assert_eq!(
            get(&mut conn, "a", "metadata", "agent").unwrap()["value"],
            "new"
        );
        assert_eq!(
            get(&mut conn, "a", "metadata", "status").unwrap()["value"],
            "ended"
        );
        assert_eq!(
            get(&mut conn, "a", "tool", "c").unwrap()["analysis"]["unobserved"],
            1
        );
        f.append(
            "return",
            "workflow.tool_completed",
            json!({"session_id":"a","tool_call_id":"c","outcome":"succeeded","occurred_at":"05"}),
            5,
        );
        f.append(
            "new",
            "workflow.session_started",
            json!({"session_id":"b","agent":"moved","occurred_at":"06"}),
            6,
        );
        f.store.refresh().unwrap();
        assert_eq!(
            get(&mut conn, "a", "metadata", "agent").unwrap()["value"],
            "old"
        );
        assert!(get(&mut conn, "a", "timeline", "new").is_none());
        assert_eq!(
            get(&mut conn, "b", "timeline", "new").unwrap()["activity"],
            true
        );
        let tool = get(&mut conn, "a", "tool", "c").unwrap();
        assert_eq!(tool["analysis"]["unobserved"], 0);
        assert_eq!(tool["analysis"]["succeeded"], 1);
    }
    #[test]
    fn oversized_metric_totals_remain_unknown_without_poisoning_catch_up() {
        let f = Fixture::new();
        f.append(
            "large",
            "jev.agent",
            json!({"agent_total_tokens":i64::MAX}),
            1,
        );
        f.append(
            "overflow",
            "jev.agent",
            json!({"agent_total_tokens":1,"agent_output_tokens":7}),
            2,
        );
        assert_eq!(f.store.refresh().unwrap(), 2);
        let report = f.store.raw_summary().unwrap();
        assert_eq!(report["events"], 2);
        assert!(report["totals"]["agent_total_tokens"].is_null());
        assert_eq!(report["overflowed_totals"]["agent_total_tokens"], true);
        assert_eq!(report["totals"]["agent_output_tokens"], 7);
        let resumed = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        assert_eq!(resumed.refresh().unwrap(), 0);
        f.append("after", "jev.agent", json!({"agent_total_tokens":3}), 3);
        assert_eq!(resumed.refresh().unwrap(), 1);
        assert!(resumed.raw_summary().unwrap()["totals"]["agent_total_tokens"].is_null());
    }

    #[test]
    fn corrupt_row_rolls_back_cursor_events_and_reports() {
        let f = Fixture::new();
        f.append("a", "other", json!({}), 1);
        f.store.refresh().unwrap();
        let before = f.store.raw_summary().unwrap();
        f.append("b", "other", json!({}), 2);
        f.source("bad", "broken json", 3);
        assert!(f.store.refresh().is_err());
        assert_eq!(f.store.raw_summary().unwrap(), before);
        assert!(f.store.get_event("b").unwrap().is_none());
        let pos = diesel::sql_query("SELECT position FROM dashboard_checkpoint WHERE singleton=1")
            .get_result::<Position>(&mut f.store.connection().unwrap())
            .unwrap();
        assert_eq!(pos.position, 1);
        // Repair is test-only; retry includes the previously rolled-back valid row.
        diesel::sql_query("UPDATE events SET payload=? WHERE aggregate_id='bad'")
            .bind::<Text, _>(json!({"event_type":"other","payload":{}}).to_string())
            .execute(&mut f.store.source_connection().unwrap())
            .unwrap();
        assert_eq!(f.store.refresh().unwrap(), 2);
        assert_eq!(f.store.raw_summary().unwrap()["events"], 3);
    }
    #[test]
    fn sidecar_replays_without_modifying_legacy_source_reports() {
        let f = Fixture::new();
        let mut source = f.store.source_connection().unwrap();
        source.batch_execute("CREATE TABLE dashboard_reports(key TEXT PRIMARY KEY,document TEXT); INSERT INTO dashboard_reports VALUES('legacy','preserve'); CREATE TABLE dashboard_checkpoint(position INTEGER); INSERT INTO dashboard_checkpoint VALUES(999);").unwrap();
        f.append("original", "other", json!({}), 1);
        let reopened = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.refresh().unwrap(), 1);
        assert_eq!(reopened.raw_summary().unwrap()["events"], 1);
        assert_eq!(
            diesel::sql_query("SELECT document FROM dashboard_reports WHERE key='legacy'")
                .get_result::<Document>(&mut source)
                .unwrap()
                .document,
            "preserve"
        );
        assert_eq!(
            diesel::sql_query("SELECT position FROM dashboard_checkpoint")
                .get_result::<Position>(&mut source)
                .unwrap()
                .position,
            999
        );
        // Losing the disposable sidecar replays authoritative events from zero.
        drop(reopened);
        for suffix in ["", "-wal", "-shm"] {
            std::fs::remove_file(format!("{}{suffix}", f.store.projection_url)).ok();
        }
        let rebuilt = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        assert_eq!(rebuilt.refresh().unwrap(), 1);
        assert_eq!(rebuilt.raw_summary().unwrap()["events"], 1);
    }

    #[test]
    fn interrupted_sidecar_transaction_restarts_at_last_committed_cursor() {
        const CHILD: &str = "HARNESS_AUDIT_CRASH_SOURCE";
        if let Ok(path) = std::env::var(CHILD) {
            let store = ReportStore::open(&path).unwrap();
            let mut conn = store.connection().unwrap();
            conn.batch_execute("BEGIN IMMEDIATE; UPDATE dashboard_checkpoint SET position=999; DELETE FROM dashboard_events; DELETE FROM dashboard_reports;").unwrap();
            // Exit without destructors/rollback, as an interrupted service would.
            std::process::exit(17);
        }
        let f = Fixture::new();
        f.append("a", "other", json!({}), 1);
        f.store.refresh().unwrap();
        f.append("b", "other", json!({}), 2);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "reports::tests::interrupted_sidecar_transaction_restarts_at_last_committed_cursor",
            ])
            .env(CHILD, &f.path)
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(17));
        let reopened = ReportStore::open(f.path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.raw_summary().unwrap()["events"], 1);
        assert!(reopened.get_event("a").unwrap().is_some());
        assert_eq!(reopened.refresh().unwrap(), 1);
        assert_eq!(reopened.raw_summary().unwrap()["events"], 2);
        assert_eq!(reopened.refresh().unwrap(), 0);
    }

    #[test]
    fn current_refresh_and_reads_do_not_need_writer_lock() {
        let f = Fixture::new();
        f.append("a", "other", json!({}), 1);
        f.store.refresh().unwrap();
        let mut writer = f.store.connection().unwrap();
        writer.batch_execute("BEGIN IMMEDIATE").unwrap();
        let started = std::time::Instant::now();
        assert_eq!(f.store.refresh().unwrap(), 0);
        assert_eq!(f.store.raw_summary().unwrap()["events"], 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        writer.batch_execute("ROLLBACK").unwrap();
    }
    #[test]
    fn concurrent_readers_and_refreshers_keep_atomic_counts() {
        let f = Fixture::new();
        f.store.refresh().unwrap();
        let mut workers = Vec::new();
        for _ in 0..3 {
            let store = ReportStore::open(f.path.to_str().unwrap()).unwrap();
            workers.push(std::thread::spawn(move || {
                for _ in 0..30 {
                    store.refresh().unwrap();
                    let raw = store.raw_summary().unwrap();
                    let n = raw["events"].as_u64().unwrap();
                    assert_eq!(raw["event_types"]["other"].as_u64().unwrap_or(0), n);
                }
            }));
        }
        for i in 0..30 {
            f.append(&format!("e{i}"), "other", json!({}), i);
        }
        for worker in workers {
            worker.join().unwrap();
        }
        f.store.refresh().unwrap();
        assert_eq!(f.store.raw_summary().unwrap()["events"], 30);
    }
}
