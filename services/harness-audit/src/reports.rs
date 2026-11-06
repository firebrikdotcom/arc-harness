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
    for (category, scope, entity) in keys {
        let rows = documents(diesel::sql_query("SELECT document FROM dashboard_orchestration_facts WHERE category=? AND scope=? AND entity=? ORDER BY recorded_at_us,position")
            .bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(entity).load::<Document>(conn)?)?;
        if let Some(fact) = crate::orchestration::entity(&rows, category) {
            diesel::sql_query("INSERT INTO dashboard_orchestration_entities(category,scope,entity,document) VALUES(?,?,?,?) ON CONFLICT(category,scope,entity) DO UPDATE SET document=excluded.document")
                .bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(entity).bind::<Text,_>(fact.to_string()).execute(conn)?;
        } else {
            diesel::sql_query("DELETE FROM dashboard_orchestration_entities WHERE category=? AND scope=? AND entity=?")
                .bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(entity).execute(conn)?;
        }
    }
    let mut entities = |category: &str| -> Result<Vec<Value>> {
        documents(diesel::sql_query("SELECT document FROM dashboard_orchestration_entities WHERE category=? ORDER BY scope,entity")
            .bind::<Text,_>(category).load::<Document>(conn)?)
    };
    let runs = entities("run")?;
    let workers = entities("worker")?;
    let decisions = entities("decision")?;
    let count = diesel::sql_query(
        "SELECT COUNT(*) AS position FROM dashboard_events WHERE partition='orchestration'",
    )
    .get_result::<Position>(conn)?
    .position;
    save(
        conn,
        "orchestration",
        &crate::orchestration::summarize_entities(count as usize, runs, workers, decisions),
    )
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
    #[diesel(sql_type = Text)]
    excluded: String,
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
    let mut all =
        diesel::sql_query("SELECT group_key,included,excluded FROM dashboard_checkpoint_groups")
            .load::<CheckpointGroup>(conn)?;
    // JSON string order is not Option<String> order (null sorts before a string in the reducer).
    all.sort_by_cached_key(|g| {
        serde_json::from_str::<Vec<Option<String>>>(&g.group_key).expect("stored group key")
    });
    for fixtures in [false, true] {
        let mut result = crate::summary::checkpoint_groups(&[], fixtures);
        for group in &all {
            let part: Value = serde_json::from_str(if fixtures {
                &group.included
            } else {
                &group.excluded
            })?;
            result["excluded_fixture_events"] = json!(
                result["excluded_fixture_events"].as_u64().unwrap()
                    + part["excluded_fixture_events"].as_u64().unwrap()
            );
            result["groups"]
                .as_array_mut()
                .unwrap()
                .extend(part["groups"].as_array().unwrap().iter().cloned());
        }
        save(conn, &format!("checkpoint:{fixtures}"), &result)?;
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

#[derive(QueryableByName)]
struct RouteGroup {
    #[diesel(sql_type = Text)]
    group_key: String,
    #[diesel(sql_type = Text)]
    included: String,
    #[diesel(sql_type = Text)]
    excluded: String,
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
    let mut all =
        diesel::sql_query("SELECT group_key,included,excluded FROM dashboard_route_groups")
            .load::<RouteGroup>(conn)?;
    // JSON string order is not Option<String> order (null sorts before a string in the reducer).
    all.sort_by_cached_key(|g| {
        serde_json::from_str::<Vec<Option<String>>>(&g.group_key).expect("stored group key")
    });
    for fixtures in [false, true] {
        let mut result = crate::summary::route_groups(&[], fixtures);
        for group in &all {
            let part: Value = serde_json::from_str(if fixtures {
                &group.included
            } else {
                &group.excluded
            })?;
            result["excluded_fixture_events"] = json!(
                result["excluded_fixture_events"].as_u64().unwrap()
                    + part["excluded_fixture_events"].as_u64().unwrap()
            );
            result["groups"]
                .as_array_mut()
                .unwrap()
                .extend(part["groups"].as_array().unwrap().iter().cloned());
        }
        save(conn, &format!("route:{fixtures}"), &result)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct ReportStore {
    database_url: String,
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
        let store = Self {
            database_url: database_url.into(),
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
            CREATE TABLE IF NOT EXISTS dashboard_timeline_rows (session TEXT NOT NULL, view TEXT NOT NULL, ordering TEXT NOT NULL, ordinal BIGINT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(session,view,ordering,ordinal));
            CREATE TABLE IF NOT EXISTS dashboard_session_entities (session TEXT NOT NULL, category TEXT NOT NULL, entity TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(session,category,entity));
            CREATE TABLE IF NOT EXISTS dashboard_orchestration_facts (id TEXT PRIMARY KEY, position BIGINT NOT NULL, category TEXT NOT NULL, scope TEXT NOT NULL, entity TEXT NOT NULL, recorded_at_us BIGINT NOT NULL, document TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS orchestration_facts_entity ON dashboard_orchestration_facts(category,scope,entity,recorded_at_us,position);
            CREATE TABLE IF NOT EXISTS dashboard_orchestration_entities (category TEXT NOT NULL, scope TEXT NOT NULL, entity TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY(category,scope,entity));")?;
        // Change this version when reducer semantics or compact report fields change.
        // Only disposable derived state is rebuilt; events and the legacy view remain intact.
        conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
            if cached(conn, "schema_version")? != Some(json!(11)) {
                conn.batch_execute("DELETE FROM dashboard_events; DELETE FROM dashboard_reports; DELETE FROM dashboard_checkpoint_facts; DELETE FROM dashboard_checkpoint_groups; DELETE FROM dashboard_route_facts; DELETE FROM dashboard_route_groups; DELETE FROM dashboard_orchestration_facts; DELETE FROM dashboard_orchestration_entities; DELETE FROM dashboard_session_facts; DELETE FROM dashboard_session_entities; DELETE FROM dashboard_timeline_rows; UPDATE dashboard_checkpoint SET position=0 WHERE singleton=1;")?;
                save(conn,"schema_version",&json!(11))?;
            }
            Ok(())
        })?;
        Ok(store)
    }
    pub(crate) fn connection(&self) -> Result<SqliteConnection> {
        let mut conn = SqliteConnection::establish(&self.database_url)?;
        conn.batch_execute("PRAGMA busy_timeout=10000;")?;
        Ok(conn)
    }
    /// Refresh only committed source rows after the durable cursor. An invalid source
    /// row fails the entire batch; it is never silently skipped or acknowledged.
    pub fn refresh(&self) -> Result<usize> {
        let mut conn = self.connection()?;
        let pending = diesel::sql_query("SELECT EXISTS(SELECT 1 FROM events WHERE id > (SELECT position FROM dashboard_checkpoint WHERE singleton=1)) AS position").get_result::<Position>(&mut conn)?.position;
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
            let cursor = diesel::sql_query("SELECT position FROM dashboard_checkpoint WHERE singleton=1").get_result::<Position>(conn)?.position;
            let source = diesel::sql_query("SELECT id AS position, aggregate_id, aggregate_type, event_type, payload, sequence, timestamp_utc_us AS recorded_at_us FROM events WHERE id > ? ORDER BY id LIMIT ?")
                .bind::<BigInt,_>(cursor).bind::<BigInt,_>(CATCH_UP_BATCH as i64).load::<Source>(conn)?;
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
                // Replay one ordered page at a time, including source position as the
                // stable tiebreaker used by the slice-based reducer.
                let mut reducer = crate::workflow::SessionReducer::default();
                // Keyset pagination keeps each replay page bounded even for long sessions.
                // The presence flag preserves SQLite's NULL-before-text ordering.
                let mut cursor: Option<(i64, String, i64, i64)> = None;
                loop {
                    let (has_time, occurred_at, recorded_at_us, position) = cursor
                        .as_ref().map(|(h, t, r, p)| (*h, t.as_str(), *r, *p))
                        .unwrap_or((-1, "", i64::MIN, i64::MIN));
                    let page = diesel::sql_query("SELECT position, (json_extract(document, '$.payload.occurred_at') IS NOT NULL) AS has_time, COALESCE(json_extract(document, '$.payload.occurred_at'),'') AS occurred_at, recorded_at_us, document FROM dashboard_events WHERE session_id=? AND partition='workflow' AND ((json_extract(document, '$.payload.occurred_at') IS NOT NULL), COALESCE(json_extract(document, '$.payload.occurred_at'),''), recorded_at_us, position) > (?,?,?,?) ORDER BY (json_extract(document, '$.payload.occurred_at') IS NOT NULL), COALESCE(json_extract(document, '$.payload.occurred_at'),''), recorded_at_us, position LIMIT ?")
                        .bind::<Text,_>(&id).bind::<BigInt,_>(has_time).bind::<Text,_>(occurred_at).bind::<BigInt,_>(recorded_at_us).bind::<BigInt,_>(position).bind::<BigInt,_>(CATCH_UP_BATCH as i64).load::<TimelineSource>(conn)?;
                    if page.is_empty() { break; }
                    for row in page {
                        reducer.push(&serde_json::from_str(&row.document)?);
                        cursor = Some((row.has_time, row.occurred_at, row.recorded_at_us, row.position));
                    }
                }
                let key=format!("session:{id}");
                let reduced=reducer.finish();
                diesel::sql_query("DELETE FROM dashboard_timeline_rows WHERE session=?").bind::<Text,_>(&id).execute(conn)?;
                if let Some(session)=reduced.first() {
                    save(conn,&key,session)?;
                    let raw: Vec<Value> = session["events"].as_array().into_iter().flatten().filter(|e| e["event_type"] != "workflow.prompt_recorded" || e["payload"]["part_index"].as_u64().unwrap_or(0) == 0).cloned().collect();
                    let signal = crate::workflow::signal_items(&raw);
                    // Persist each ordering before constructing the next one. Keeping
                    // four owned timelines here multiplies peak memory for long sessions.
                    let mut persist = |view: &str, ordering: &str, items: &[Value], reverse: bool| -> Result<()> {
                        for ordinal in 0..items.len() {
                            let index = if reverse { items.len() - ordinal - 1 } else { ordinal };
                            diesel::sql_query("INSERT INTO dashboard_timeline_rows(session,view,ordering,ordinal,document) VALUES(?,?,?,?,?)").bind::<Text,_>(&id).bind::<Text,_>(view).bind::<Text,_>(ordering).bind::<BigInt,_>(ordinal as i64).bind::<Text,_>(items[index].to_string()).execute(conn)?;
                        }
                        Ok(())
                    };
                    persist("all", "oldest", &raw, false)?;
                    persist("all", "newest", &raw, true)?;
                    drop(raw);
                    persist("signal", "oldest", &signal, false)?;
                    let signal = crate::workflow::newest_first(signal);
                    persist("signal", "newest", &signal, false)?;
                    let mut compact=compact_session(session);
                    for field in ["events","prompts","tasks","task_list","metadata_history"] { compact.as_object_mut().unwrap().remove(field); }
                    save(conn,&format!("directory:{id}"),&compact)?;
                } else {
                    diesel::sql_query("DELETE FROM dashboard_reports WHERE key IN (?,?)").bind::<Text,_>(key).bind::<Text,_>(format!("directory:{id}")).execute(conn)?;
                }
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
        self.report(
            &format!("checkpoint:{fixtures}"),
            crate::summary::checkpoint_groups(&[], fixtures),
        )
    }
    pub fn route_groups(&self, fixtures: bool) -> Result<Value> {
        self.report(
            &format!("route:{fixtures}"),
            crate::summary::route_groups(&[], fixtures),
        )
    }
    pub fn orchestration(&self) -> Result<Value> {
        self.report("orchestration", crate::orchestration::summarize(&[]))
    }
    pub fn session(&self, id: &str) -> Result<Option<Value>> {
        cached(&mut self.connection()?, &format!("session:{id}"))
    }
    pub fn session_without_events(&self, id: &str) -> Result<Option<Value>> {
        let mut conn = self.connection()?;
        let row = diesel::sql_query("SELECT json_set(json_remove(d.document, '$.events'), '$.event_count', json_array_length(d.document, '$.events'), '$.verdict', json(COALESCE(json_extract(s.document, '$.verdict'), json_extract(d.document, '$.verdict')))) AS document FROM dashboard_reports d LEFT JOIN dashboard_reports s ON s.key='session-summary:' || substr(d.key,9) WHERE d.key=?")
            .bind::<Text,_>(format!("session:{id}")).get_result::<Document>(&mut conn).optional()?;
        row.map(|row| Ok(serde_json::from_str(&row.document)?))
            .transpose()
    }
    pub fn session_timeline(
        &self,
        id: &str,
        view: &str,
        ordering: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<Value>, usize)>> {
        let mut conn = self.connection()?;
        if cached(&mut conn, &format!("directory:{id}"))?.is_none() {
            return Ok(None);
        }
        let total = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_timeline_rows WHERE session=? AND view=? AND ordering=?")
            .bind::<Text,_>(id).bind::<Text,_>(view).bind::<Text,_>(ordering).get_result::<Position>(&mut conn)?.position as usize;
        let rows = diesel::sql_query("SELECT document FROM dashboard_timeline_rows WHERE session=? AND view=? AND ordering=? ORDER BY ordinal LIMIT ? OFFSET ?")
            .bind::<Text,_>(id).bind::<Text,_>(view).bind::<Text,_>(ordering).bind::<BigInt,_>(limit as i64).bind::<BigInt,_>(offset as i64).load::<Document>(&mut conn)?;
        Ok(Some((documents(rows)?, total)))
    }
    /// Load only the requested prompt rows, leaving the session's other arrays in SQLite.
    pub fn session_prompts(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<Value>, usize)>> {
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
    /// Read task projections without loading the session's timeline or prompt bodies.
    pub fn session_tasks(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<Value>, usize)>> {
        let mut conn = self.connection()?;
        if cached(&mut conn, &format!("directory:{id}"))?.is_none() {
            return Ok(None);
        }
        let total = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='task'")
            .bind::<Text,_>(id).get_result::<Position>(&mut conn)?.position as usize;
        let rows = diesel::sql_query("SELECT document FROM dashboard_session_entities WHERE session=? AND category='task' ORDER BY json_extract(document, '$.first_at'), (SELECT MIN(position) FROM dashboard_session_facts f WHERE f.session=dashboard_session_entities.session AND f.category='task' AND f.entity=dashboard_session_entities.entity), entity LIMIT ? OFFSET ?")
            .bind::<Text,_>(id).bind::<BigInt,_>(limit as i64).bind::<BigInt,_>(offset as i64).load::<Document>(&mut conn)?;
        let mut tasks = Vec::new();
        for (index, entity) in documents(rows)?.into_iter().enumerate() {
            let mut task = entity["task"].clone();
            task["sequence"] = json!(offset + index + 1);
            let mut map = serde_json::Map::new();
            map.insert("task".into(), task);
            tasks.extend(crate::workflow::ordered_tasks(&Value::Object(map)));
        }
        Ok(Some((tasks, total)))
    }
    pub fn sessions(&self) -> Result<Vec<Value>> {
        let mut rows=documents(diesel::sql_query("SELECT json_set(d.document, '$.verdict', json(COALESCE(json_extract(s.document, '$.verdict'), json_extract(d.document, '$.verdict')))) AS document FROM dashboard_reports d LEFT JOIN dashboard_reports s ON s.key='session-summary:' || substr(d.key,11) WHERE d.key >= 'directory:' AND d.key < 'directory;' ").load::<Document>(&mut self.connection()?)?)?;
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

fn compact_session(session: &Value) -> Value {
    let mut compact = Value::Object(session.as_object().unwrap().iter()
        .filter(|(key, _)| !matches!(key.as_str(), "events" | "prompts" | "tasks" | "task_list" | "metadata_history"))
        .map(|(key, value)| (key.clone(), value.clone())).collect());
    let tasks = session["tasks"].as_object();
    compact["task_count"] = json!(tasks.map_or(0, |t| t.len()));
    compact["completed_tasks"] = json!(tasks.map_or(0, |t| t
        .values()
        .filter(|t| t["status"] == "completed")
        .count()));
    compact["prompt_count"] = json!(session["prompts"].as_array().map_or(0, Vec::len));
    let phases: BTreeSet<&str> = tasks
        .into_iter()
        .flat_map(|t| t.values())
        .filter_map(|t| t["phase"].as_str().filter(|s| !s.is_empty()))
        .collect();
    compact["phases"] = json!(phases);
    let mut words = Vec::new();
    for key in [
        "label",
        "session_name",
        "herdr_tab",
        "agent",
        "model",
        "cwd",
        "status",
        "session_id",
        "initial_goal",
    ] {
        if let Some(text) = session[key].as_str() {
            words.push(text);
        }
    }
    for task in tasks.into_iter().flat_map(|t| t.values()) {
        for key in [
            "name",
            "description",
            "outcome",
            "status",
            "phase",
            "task_id",
        ] {
            if let Some(text) = task[key].as_str() {
                words.push(text);
            }
        }
    }
    for prompt in session["prompts"].as_array().into_iter().flatten() {
        if let Some(text) = prompt["text"].as_str() {
            words.push(text);
        }
    }
    for event in session["events"].as_array().into_iter().flatten() {
        for key in ["name", "description", "outcome", "selected", "tool_name"] {
            if let Some(text) = event["payload"][key].as_str() {
                words.push(text);
            }
        }
        for option in event["payload"]["options"].as_array().into_iter().flatten() {
            if let Some(text) = option.as_str() {
                words.push(text);
            }
        }
    }
    compact["search_text"] = json!(words.join(" ").to_lowercase());
    let description = session["events"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|e| {
            if e["payload"]["name"]
                .as_str()
                .is_some_and(|n| n != "Session work")
            {
                e["payload"]["description"].as_str()
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| session["initial_goal"].as_str().unwrap_or(""));
    let preview: String = description.chars().take(180).collect();
    compact["preview"] = json!(if description.chars().count() > 180 {
        format!("{preview}…")
    } else {
        preview
    });
    compact
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
            store.connection().unwrap().batch_execute("CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,aggregate_id TEXT NOT NULL,aggregate_type TEXT NOT NULL DEFAULT 'AuditEvent',event_type TEXT NOT NULL,payload TEXT NOT NULL,sequence BIGINT NOT NULL,timestamp_utc_us BIGINT NOT NULL)").unwrap();
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
            diesel::sql_query("INSERT INTO events(aggregate_id,event_type,payload,sequence,timestamp_utc_us) VALUES(?,'AuditEventRecorded',?,1,?)").bind::<Text,_>(id).bind::<Text,_>(payload).bind::<BigInt,_>(at).execute(&mut self.store.connection().unwrap()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
            let _ = std::fs::remove_file(format!("{}-shm", self.path.display()));
        }
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
            .execute(&mut f.store.connection().unwrap())
            .unwrap();
        assert_eq!(f.store.refresh().unwrap(), 2);
        assert_eq!(f.store.raw_summary().unwrap()["events"], 3);
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
            let store = f.store.clone();
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
