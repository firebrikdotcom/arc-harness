//! Durable dashboard projections. The source cursor and every derived report commit together.
//! Source positions, rather than producer timestamps, define catch-up order.
use anyhow::{anyhow, Context, Result};
use diesel::{
    connection::SimpleConnection,
    prelude::*,
    sql_types::{BigInt, Text},
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

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
            CREATE INDEX IF NOT EXISTS dashboard_events_partition_position ON dashboard_events(partition,position);
            CREATE INDEX IF NOT EXISTS dashboard_events_recent ON dashboard_events(recorded_at_us DESC,position DESC);
            CREATE TABLE IF NOT EXISTS dashboard_reports (key TEXT PRIMARY KEY, document TEXT NOT NULL);")?;
        // Change this version when reducer semantics or compact report fields change.
        // Only disposable derived state is rebuilt; events and the legacy view remain intact.
        conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
            if cached(conn, "schema_version")? != Some(json!(4)) {
                conn.batch_execute("DELETE FROM dashboard_events; DELETE FROM dashboard_reports; UPDATE dashboard_checkpoint SET position=0 WHERE singleton=1;")?;
                save(conn,"schema_version",&json!(4))?;
            }
            Ok(())
        })?;
        Ok(store)
    }
    fn connection(&self) -> Result<SqliteConnection> {
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
            return Ok(0);
        }
        conn.immediate_transaction::<_, anyhow::Error, _>(|conn| {
            let cursor = diesel::sql_query("SELECT position FROM dashboard_checkpoint WHERE singleton=1").get_result::<Position>(conn)?.position;
            let source = diesel::sql_query("SELECT id AS position, aggregate_id, aggregate_type, event_type, payload, sequence, timestamp_utc_us AS recorded_at_us FROM events WHERE id > ? ORDER BY id")
                .bind::<BigInt,_>(cursor).load::<Source>(conn)?;
            if source.is_empty() { return Ok(0); }
            let mut sessions = BTreeSet::new();
            let mut partitions = BTreeSet::new();
            let mut raw = cached(conn,"raw")?.unwrap_or_else(|| json!({"events":0,"event_types":{},"outcomes":{},"totals":{}}));
            for event in &source {
                if event.aggregate_type != "AuditEvent" || event.event_type != "AuditEventRecorded" { continue; }
                let envelope: Value = serde_json::from_str(&event.payload).with_context(|| format!("invalid audit source JSON at position {}",event.position))?;
                let kind = envelope["event_type"].as_str().ok_or_else(|| anyhow!("missing audit event type at position {}", event.position))?;
                let payload = envelope.get("payload").filter(|p| p.is_object()).ok_or_else(|| anyhow!("invalid audit payload at position {}", event.position))?;
                let row = json!({"id":event.aggregate_id,"event_type":kind,"payload":payload,"version":event.sequence,"recorded_at_us":event.recorded_at_us});
                let old = diesel::sql_query("SELECT document FROM dashboard_events WHERE id=?").bind::<Text,_>(&event.aggregate_id).get_result::<Document>(conn).optional()?;
                if let Some(old) = old { let old:Value=serde_json::from_str(&old.document)?; adjust_raw(&mut raw,&old,-1)?; affected(&old,&mut sessions,&mut partitions); }
                affected(&row,&mut sessions,&mut partitions);
                adjust_raw(&mut raw,&row,1)?;
                diesel::sql_query("INSERT INTO dashboard_events(id,position,event_type,session_id,partition,recorded_at_us,document) VALUES(?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET position=excluded.position,event_type=excluded.event_type,session_id=excluded.session_id,partition=excluded.partition,recorded_at_us=excluded.recorded_at_us,document=excluded.document")
                    .bind::<Text,_>(&event.aggregate_id).bind::<BigInt,_>(event.position).bind::<Text,_>(kind).bind::<Text,_>(payload["session_id"].as_str().unwrap_or(""))
                    .bind::<Text,_>(partition(kind)).bind::<BigInt,_>(event.recorded_at_us).bind::<Text,_>(row.to_string()).execute(conn)?;
            }
            save(conn,"raw",&raw)?;
            for id in sessions {
                let rows=documents(diesel::sql_query("SELECT document FROM dashboard_events WHERE session_id=? AND partition='workflow' ORDER BY position").bind::<Text,_>(&id).load::<Document>(conn)?)?;
                let key=format!("session:{id}");
                let reduced=crate::workflow::sessions(&rows);
                if let Some(session)=reduced.first() {
                    save(conn,&key,session)?;
                    let mut compact=compact_session(session);
                    for field in ["events","prompts","tasks","task_list","metadata_history"] { compact.as_object_mut().unwrap().remove(field); }
                    save(conn,&format!("directory:{id}"),&compact)?;
                } else {
                    diesel::sql_query("DELETE FROM dashboard_reports WHERE key IN (?,?)").bind::<Text,_>(key).bind::<Text,_>(format!("directory:{id}")).execute(conn)?;
                }
            }
            for part in partitions {
                if part=="workflow" || part=="other" { continue; }
                let rows=partition_rows(conn,part)?;
                match part {
                    "checkpoint" => for fixtures in [false,true] { save(conn,&format!("checkpoint:{fixtures}"),&crate::summary::checkpoint_groups(&rows,fixtures))?; },
                    "route" => for fixtures in [false,true] { save(conn,&format!("route:{fixtures}"),&crate::summary::route_groups(&rows,fixtures))?; },
                    "orchestration" => save(conn,"orchestration",&crate::orchestration::summarize(&rows))?,
                    _=>{}
                }
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
    pub fn sessions(&self) -> Result<Vec<Value>> {
        let mut rows=documents(diesel::sql_query("SELECT document FROM dashboard_reports WHERE key >= 'directory:' AND key < 'directory;' ").load::<Document>(&mut self.connection()?)?)?;
        rows.sort_by(|a, b| b["last_at"].as_str().cmp(&a["last_at"].as_str()));
        Ok(rows)
    }
    /// Chart-only facts keep time-zone bucketing accurate without shipping prompt text.
    pub fn checkpoint_rows(&self) -> Result<Vec<Value>> {
        let mut rows = partition_rows(&mut self.connection()?, "checkpoint")?;
        for row in &mut rows {
            if let Some(p) = row["payload"].as_object_mut() {
                p.retain(|k, _| {
                    [
                        "call_id",
                        "outcome",
                        "model_requested",
                        "model_returned",
                        "recommendation_confidence",
                        "family",
                        "interaction_origin",
                        "context_version",
                    ]
                    .contains(&k.as_str())
                });
            }
        }
        Ok(rows)
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
fn partition_rows(conn: &mut SqliteConnection, part: &str) -> Result<Vec<Value>> {
    documents(
        diesel::sql_query(
            "SELECT document FROM dashboard_events WHERE partition=? ORDER BY position",
        )
        .bind::<Text, _>(part)
        .load::<Document>(conn)?,
    )
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
    let mut compact = session.clone();
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
        let rows = f.store.checkpoint_rows().unwrap();
        assert_eq!(rows[0]["payload"]["recommendation_confidence"], 0);
        assert_eq!(rows[0]["payload"]["model_requested"], "fixture");
        assert!(rows[0]["payload"].get("prompt").is_none());
        assert!(crate::summary::daily_checkpoint_confidence(&rows, false).is_empty());
        assert_eq!(
            crate::summary::daily_checkpoint_confidence(&rows, true)[0].mean,
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
        assert_eq!(reopened.checkpoint_rows().unwrap().len(), 2);
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
