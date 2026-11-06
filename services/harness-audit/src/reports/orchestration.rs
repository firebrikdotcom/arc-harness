//! Orchestration heads and aggregates are reduced in SQLite, independently of
//! the number of lifecycle observations or historical workers.
use super::*;

pub(super) fn refresh(
    conn: &mut SqliteConnection,
    keys: &BTreeSet<OrchestrationKey>,
) -> Result<()> {
    for (category, scope, id) in keys {
        let mut cursor = (i64::MIN, i64::MIN);
        let mut merged = json!({});
        let mut found = false;
        loop {
            let rows=diesel::sql_query("SELECT position,recorded_at_us,0 AS has_time,'' AS occurred_at,document FROM dashboard_orchestration_facts WHERE category=? AND scope=? AND entity=? AND (recorded_at_us,position)>(?,?) ORDER BY recorded_at_us,position LIMIT 128")
                .bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(id).bind::<BigInt,_>(cursor.0).bind::<BigInt,_>(cursor.1).load::<TimelineSource>(conn)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                cursor = (row.recorded_at_us, row.position);
                found = true;
                let row: Value = serde_json::from_str(&row.document)?;
                let p = &row["payload"];
                for (key, value) in p.as_object().into_iter().flatten() {
                    if !value.is_null() {
                        merged[key] = value.clone();
                    }
                }
                let kind = row["event_type"]
                    .as_str()
                    .unwrap_or("")
                    .rsplit('_')
                    .next()
                    .unwrap_or("");
                let status = match kind {
                    "planned" => Some("planned"),
                    "created" => Some("created"),
                    "started" => Some("running"),
                    "completed" => Some(
                        p["status"]
                            .as_str()
                            .filter(|s| {
                                matches!(
                                    *s,
                                    "completed"
                                        | "failed"
                                        | "cancelled"
                                        | "disabled"
                                        | "reserved"
                                        | "blocked"
                                )
                            })
                            .unwrap_or("completed"),
                    ),
                    "failed" => Some("failed"),
                    "cancelled" => Some("cancelled"),
                    _ => None,
                };
                if let Some(status) = status {
                    merged["status"] = json!(status);
                }
            }
        }
        if found {
            let row =
                json!({"event_type":format!("orchestration.{category}_observed"),"payload":merged});
            if let Some(head) = crate::orchestration::entity(&[row], category) {
                diesel::sql_query("INSERT INTO dashboard_orchestration_entities(category,scope,entity,document) VALUES(?,?,?,?) ON CONFLICT(category,scope,entity) DO UPDATE SET document=excluded.document")
                .bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(id).bind::<Text,_>(head.to_string()).execute(conn)?;
            }
        } else {
            diesel::sql_query("DELETE FROM dashboard_orchestration_entities WHERE category=? AND scope=? AND entity=?").bind::<Text,_>(category).bind::<Text,_>(scope).bind::<Text,_>(id).execute(conn)?;
        }
    }
    // Report data is now read in bounded pages. Do not serialize all entities
    // back into a monolithic cached report after an incremental head update.
    diesel::sql_query("DELETE FROM dashboard_reports WHERE key='orchestration'").execute(conn)?;
    Ok(())
}
fn count(conn: &mut SqliteConnection, condition: &str) -> Result<i64> {
    Ok(diesel::sql_query(format!(
        "SELECT COUNT(*) AS position FROM dashboard_orchestration_entities WHERE {condition}"
    ))
    .get_result::<Position>(conn)?
    .position)
}
fn metric(conn: &mut SqliteConnection, condition: &str, key: &str) -> Result<Value> {
    let population = count(conn, condition)?;
    let expression = format!("json_extract(document,'$.{key}')");
    let valid = format!(
        "({condition}) AND json_type(document,'$.{key}') IN ('integer','real') AND {expression}>=0"
    );
    let n = count(conn, &valid)?;
    #[derive(QueryableByName)]
    struct Number {
        #[diesel(sql_type=diesel::sql_types::Nullable<diesel::sql_types::Double>)]
        value: Option<f64>,
    }
    let total=diesel::sql_query(format!("SELECT SUM(CAST({expression} AS REAL)) AS value FROM dashboard_orchestration_entities WHERE {valid}")).get_result::<Number>(conn)?.value;
    let median = if n == 0 {
        None
    } else {
        diesel::sql_query(format!("SELECT AVG(value) AS value FROM (SELECT CAST({expression} AS REAL) AS value FROM dashboard_orchestration_entities WHERE {valid} ORDER BY value LIMIT {} OFFSET {})",if n%2==0{2}else{1},(n-1)/2)).get_result::<Number>(conn)?.value
    };
    Ok(json!({"total":total,"samples":n,"population":population,"median":median}))
}
fn statuses(conn: &mut SqliteConnection, condition: &str) -> Result<Value> {
    #[derive(QueryableByName)]
    struct Status {
        #[diesel(sql_type=Text)]
        document: String,
        #[diesel(sql_type=BigInt)]
        position: i64,
    }
    let rows=diesel::sql_query(format!("SELECT COALESCE(json_extract(document,'$.status'),'unknown') AS document,COUNT(*) AS position FROM dashboard_orchestration_entities WHERE {condition} GROUP BY 1")).load::<Status>(conn)?;
    Ok(Value::Object(
        rows.into_iter()
            .map(|r| (r.document, json!(r.position)))
            .collect(),
    ))
}
fn quote(value: &Value) -> String {
    match value.as_str() {
        Some(s) => format!("'{}'", s.replace('\'', "''")),
        None => "NULL".into(),
    }
}
pub(super) fn report(conn: &mut SqliteConnection, offset: usize, limit: usize) -> Result<Value> {
    let mut report = crate::orchestration::summarize(&[]);
    for (category, key) in [
        ("run", "runs"),
        ("worker", "workers"),
        ("decision", "decisions"),
    ] {
        report[key]=json!(documents(diesel::sql_query("SELECT document FROM dashboard_orchestration_entities WHERE category=? ORDER BY scope,entity LIMIT ? OFFSET ?").bind::<Text,_>(category).bind::<BigInt,_>(limit.min(200) as i64).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(conn)?)?);
    }
    report["events"] = json!(
        diesel::sql_query(
            "SELECT COUNT(*) AS position FROM dashboard_events WHERE partition='orchestration'"
        )
        .get_result::<Position>(conn)?
        .position
    );
    report["worker_count"] = json!(count(conn, "category='worker'")?);
    report["decision_count"] = json!(count(conn, "category='decision'")?);
    report["statuses"] = statuses(conn, "category='worker'")?;
    for (field, key) in [
        ("decision_tokens", "total_tokens"),
        ("decision_cost_usd", "cost_usd"),
        ("decision_latency_ms", "latency_ms"),
        ("decision_duration_ms", "duration_ms"),
    ] {
        report[field] = metric(conn, "category='decision'", key)?;
    }
    report["cache_hits"] = json!(count(
        conn,
        "category='decision' AND json_extract(document,'$.cache_hit')=1"
    )?);
    report["cache_samples"] = json!(count(
        conn,
        "category='decision' AND json_type(document,'$.cache_hit') IN ('true','false')"
    )?);
    for (category, field, dimensions) in [
        (
            "worker",
            "worker_groups",
            vec!["role", "provider", "model", "effort"],
        ),
        (
            "run",
            "task_measurements",
            vec!["comparison_key", "strategy"],
        ),
    ] {
        let expressions = dimensions
            .iter()
            .map(|k| format!("json_extract(document,'$.{k}')"))
            .collect::<Vec<_>>();
        let groups=documents(diesel::sql_query(format!("SELECT json_array({}) AS document FROM dashboard_orchestration_entities WHERE category=? GROUP BY {} ORDER BY {} LIMIT ? OFFSET ?",expressions.join(","),expressions.join(","),expressions.join(","))).bind::<Text,_>(category).bind::<BigInt,_>(limit.min(200) as i64).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(conn)?)?;
        let mut output = Vec::new();
        for group in groups {
            let mut result = json!({});
            let mut condition = format!("category='{category}'");
            for (i, key) in dimensions.iter().enumerate() {
                result[*key] = group[i].clone();
                condition.push_str(&format!(
                    " AND json_extract(document,'$.{key}') IS {}",
                    quote(&group[i])
                ));
            }
            result["count"] = json!(count(conn, &condition)?);
            if category == "worker" {
                result["statuses"] = statuses(conn, &condition)?;
                for (field, key) in [
                    ("duration_ms", "duration_ms"),
                    ("queue_ms", "queue_ms"),
                    ("context_bytes", "context_bytes"),
                    ("tokens", "total_tokens"),
                    ("cached_input_tokens", "cached_input_tokens"),
                    ("cost_usd", "cost_usd"),
                ] {
                    result[field] = metric(conn, &condition, key)?;
                }
            } else {
                for key in ["elapsed_ms", "total_tokens", "cost_usd", "rework_count"] {
                    result[key] = metric(conn, &condition, key)?;
                }
                result["measured"]=json!(count(conn,&format!("{condition} AND json_type(document,'$.measured_at') IN ('integer','real') AND json_extract(document,'$.measured_at')>=0"))?);
                result["quality_samples"]=json!(count(conn,&format!("{condition} AND json_type(document,'$.quality_passed') IN ('true','false')"))?);
                result["quality_passed"] = json!(count(
                    conn,
                    &format!("{condition} AND json_extract(document,'$.quality_passed')=1")
                )?);
            }
            output.push(result);
        }
        report[field] = json!(output);
    }
    conn.register_sql_function::<Text,diesel::sql_types::Nullable<BigInt>,String,Option<i64>,_>("orchestration_instant",true,|s|serde_json::from_str::<Value>(&s).ok().and_then(|v|crate::orchestration::instant(&v)))?;
    let intervals="SELECT orchestration_instant(COALESCE(json_quote(json_extract(document,'$.started_at')),'null')) AS start,orchestration_instant(COALESCE(json_quote(json_extract(document,'$.finished_at')),'null')) AS finish FROM dashboard_orchestration_entities WHERE category='worker'";
    let samples = diesel::sql_query(format!(
        "SELECT COUNT(*) AS position FROM ({intervals}) WHERE finish>start"
    ))
    .get_result::<Position>(conn)?
    .position;
    report["concurrency_samples"] = json!(samples);
    report["peak_concurrency"] = if samples == 0 {
        Value::Null
    } else {
        json!(diesel::sql_query(format!("WITH intervals AS ({intervals}), edges AS (SELECT start AS at,1 AS delta FROM intervals WHERE finish>start UNION ALL SELECT finish AS at,-1 AS delta FROM intervals WHERE finish>start) SELECT MAX(active) AS position FROM (SELECT SUM(delta) OVER (ORDER BY at,delta ROWS UNBOUNDED PRECEDING) AS active FROM edges)")).get_result::<Position>(conn)?.position)
    };
    Ok(report)
}

impl ReportStore {
    pub fn orchestration_diagram(
        &self,
        selected: Option<&str>,
        offset: usize,
        parents_offset: usize,
    ) -> Result<Value> {
        let mut conn = self.connection()?;
        let mut parents=diesel::sql_query("SELECT COALESCE(json_extract(document,'$.parent_session_id'),'') AS document FROM dashboard_orchestration_entities GROUP BY 1 ORDER BY (SELECT json_extract(d.document,'$.last_at') FROM dashboard_reports d WHERE d.key='directory:' || COALESCE(json_extract(dashboard_orchestration_entities.document,'$.parent_session_id'),'')) DESC,1 LIMIT 200 OFFSET ?")
            .bind::<BigInt,_>(parents_offset.min(i64::MAX as usize) as i64).load::<Document>(&mut conn)?;
        if let Some(id) = selected.filter(|id| !parents.iter().any(|p| p.document == *id)) {
            if diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_orchestration_entities WHERE COALESCE(json_extract(document,'$.parent_session_id'),'')=?").bind::<Text,_>(id).get_result::<Position>(&mut conn)?.position>0 {parents.push(Document{document:id.to_owned()});}
        }
        let mut sessions = Vec::new();
        for parent in parents {
            let id = &parent.document;
            let metadata = cached(&mut conn, &format!("directory:{id}"))?;
            let meta = metadata.as_ref().unwrap_or(&Value::Null);
            let text = |key: &str| meta[key].as_str().filter(|s| !s.trim().is_empty());
            let mut session = json!({"id":id,"name":text("session_name").or(text("herdr_tab")).or(text("label")).unwrap_or(if id.is_empty(){"Unlinked observations"}else{"Unnamed session"}),"workflow_available":metadata.is_some(),"model":text("model").unwrap_or("Unknown model"),"agent":text("agent").unwrap_or("Unknown agent"),"status":text("status").unwrap_or("unknown"),"last_at":text("last_at").unwrap_or("")});
            for (category, field) in [
                ("run", "run_count"),
                ("worker", "worker_count"),
                ("decision", "decision_count"),
            ] {
                session[field]=json!(diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_orchestration_entities WHERE category=? AND COALESCE(json_extract(document,'$.parent_session_id'),'')=?").bind::<Text,_>(category).bind::<Text,_>(id).get_result::<Position>(&mut conn)?.position);
            }
            sessions.push(session);
        }
        let selected = selected
            .or_else(|| sessions.first().and_then(|s| s["id"].as_str()))
            .unwrap_or("");
        let session = sessions
            .iter()
            .find(|s| s["id"] == selected)
            .cloned()
            .unwrap_or(Value::Null);
        let children=documents(diesel::sql_query("SELECT document FROM dashboard_orchestration_entities WHERE category='worker' AND COALESCE(json_extract(document,'$.parent_session_id'),'')=? ORDER BY scope,entity LIMIT 200 OFFSET ?").bind::<Text,_>(selected).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(&mut conn)?)?;
        let count=diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_orchestration_entities WHERE category='worker' AND COALESCE(json_extract(document,'$.parent_session_id'),'')=?").bind::<Text,_>(selected).get_result::<Position>(&mut conn)?.position as usize;
        Ok(
            json!({"sessions":sessions,"session":session,"children":children,"child_count":count,"hidden_children":count.saturating_sub(offset.saturating_add(children.len()))}),
        )
    }
}
