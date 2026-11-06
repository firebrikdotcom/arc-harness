//! Disk-backed workflow state. Histories remain individual source facts; heads never
//! contain growing event, revision or todo arrays.
use super::*;

const ORDER: &str =
    "json_extract(e.document,'$.payload.occurred_at'), e.recorded_at_us, f.position";
// Every entity is ordered by its first chronological fact, including both
// recorded-time and source-position ties; ingestion order alone is insufficient.
pub(super) fn entity_order() -> String {
    ["json_extract(e.document,'$.payload.occurred_at')", "e.recorded_at_us", "f.position"]
        .iter()
        .map(|column| format!("(SELECT {column} FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=h.session AND f.category=h.category AND f.entity=h.entity ORDER BY {ORDER} LIMIT 1)"))
        .collect::<Vec<_>>()
        .join(",")
}

fn facts(
    conn: &mut SqliteConnection,
    session: &str,
    category: &str,
    entity: &str,
    suffix: &str,
) -> Result<Vec<Value>> {
    documents(diesel::sql_query(format!("SELECT e.document FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=? AND f.category=? AND f.entity=? {suffix}"))
        .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).load::<Document>(conn)?)
}
fn latest(
    conn: &mut SqliteConnection,
    session: &str,
    category: &str,
    entity: &str,
    condition: &str,
) -> Result<Option<Value>> {
    Ok(facts(conn,session,category,entity,&format!("AND ({condition}) ORDER BY json_extract(e.document,'$.payload.occurred_at') DESC,e.recorded_at_us DESC,f.position DESC LIMIT 1"))?.pop())
}
pub(super) fn head(
    conn: &mut SqliteConnection,
    session: &str,
    category: &str,
    entity: &str,
) -> Result<Value> {
    let first = facts(
        conn,
        session,
        category,
        entity,
        &format!("ORDER BY {ORDER} LIMIT 1"),
    )?
    .pop();
    let Some(first) = first else {
        return Ok(Value::Null);
    };
    let last = latest(conn, session, category, entity, "1")?.unwrap();
    let count = diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_session_facts WHERE session=? AND category=? AND entity=?")
        .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(entity).get_result::<Position>(conn)?.position;
    let mut result = json!({"events":count,"first_at":first["payload"]["occurred_at"],"last_at":last["payload"]["occurred_at"]});
    if category == "prompt" {
        // Only the last version of each chunk participates, even if the producer
        // has re-emitted a chunk thousands of times under different event IDs.
        let rows = documents(diesel::sql_query("SELECT document FROM (SELECT e.document, ROW_NUMBER() OVER (PARTITION BY json_extract(e.document,'$.payload.part_index') ORDER BY json_extract(e.document,'$.payload.occurred_at') DESC,e.recorded_at_us DESC,f.position DESC) AS rank FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=? AND f.category='prompt' AND f.entity=? AND e.event_type='workflow.prompt_recorded') WHERE rank=1 ORDER BY json_extract(document,'$.payload.part_index')")
            .bind::<Text,_>(session).bind::<Text,_>(entity).load::<Document>(conn)?)?;
        let mut projection = crate::workflow::projection_entity("prompt", entity, &rows);
        // The first chronological chunk defines the expected count, not the
        // earliest surviving version of any particular chunk.
        let expected = first["payload"]["part_count"].as_u64().unwrap_or(0);
        let complete = expected > 0
            && rows.len() as u64 == expected
            && rows
                .iter()
                .enumerate()
                .all(|(i, r)| r["payload"]["part_index"].as_u64() == Some(i as u64));
        projection["complete"] = json!(complete);
        projection["part_count"] = json!(expected);
        if complete {
            let body = rows
                .iter()
                .filter_map(|r| r["payload"]["text"].as_str())
                .collect::<String>();
            projection["preview"] = json!(crate::workflow::preview(&body));
            projection["text"] = json!(body);
        } else {
            projection.as_object_mut().unwrap().remove("text");
            projection.as_object_mut().unwrap().remove("preview");
        }
        for key in ["events", "first_at", "last_at"] {
            projection[key] = result[key].clone();
        }
        return Ok(projection);
    }
    if category == "task" || category == "todo" {
        let task_id = first["payload"]["task_id"].as_str().unwrap_or("");
        let mut reducer = crate::workflow::SessionReducer::default();
        reducer.push(&first);
        let mut task = reducer.sessions.remove(session).unwrap()["tasks"][task_id].take();
        let mut value = if category == "task" {
            task.clone()
        } else {
            task["todos"][first["payload"]["todo_id"].as_str().unwrap_or("")].take()
        };
        let fields: &[&str] = if category == "task" {
            &["name", "description", "status", "outcome", "parent_task_id"]
        } else {
            &[
                "description",
                "criterion",
                "status",
                "evidence",
                "todo_required",
                "revision",
            ]
        };
        for field in fields {
            let kind = if category == "task" {
                "e.event_type LIKE 'workflow.task_%'"
            } else {
                "e.event_type LIKE 'workflow.todo_%'"
            };
            if let Some(row)=latest(conn,session,category,entity,&format!("{kind} AND json_type(e.document,'$.payload.{field}') IS NOT NULL AND json_type(e.document,'$.payload.{field}') != 'null'"))? {value[*field]=row["payload"][*field].clone();}
        }
        value["last_at"] = last["payload"]["occurred_at"].clone();
        if category == "task" {
            value["todos"] = json!({});
            value["plan_revisions"] = json!([]);
            value["phases"] = json!({});
            if let Some(plan) = latest(
                conn,
                session,
                category,
                entity,
                "e.event_type LIKE 'workflow.plan_%'",
            )? {
                value["plan_status"] = json!(match plan["event_type"].as_str().unwrap_or("") {
                    "workflow.plan_required" => "required",
                    "workflow.plan_exempted" => "question",
                    _ => "confirmed",
                });
                result["last_plan_id"] = plan["id"].clone();
            }
            if let Some(phase) = latest(
                conn,
                session,
                category,
                entity,
                "json_type(e.document,'$.payload.phase')='text'",
            )? {
                value["phase"] = phase["payload"]["phase"].clone();
            }
            result["task"] = value;
        } else {
            value["history"] = json!([]);
            result["todo"] = value;
            result["task_id"] = json!(task_id);
        }
        return Ok(result);
    }
    // Tool counters are additive, and pairing depends only on the existence of
    // a return. SQL scans facts without retaining call history in the process.
    if category == "tool" {
        let started = latest(
            conn,
            session,
            category,
            entity,
            "e.event_type='workflow.tool_started'",
        )?
        .is_some();
        let returned = latest(
            conn,
            session,
            category,
            entity,
            "e.event_type IN ('workflow.tool_completed','workflow.tool_failed')",
        )?;
        result["started"] = json!(started);
        result["returned"] = json!(returned.is_some());
        let mut counts =
            json!({"succeeded":0,"failed":0,"returned":0,"process_running":0,"unobserved":0});
        let mut offset = 0;
        loop {
            let rows = facts(
                conn,
                session,
                category,
                entity,
                &format!("ORDER BY f.position LIMIT 128 OFFSET {offset}"),
            )?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                let one =
                    crate::workflow::projection_entity("tool", entity, std::slice::from_ref(row));
                for key in [
                    "succeeded",
                    "failed",
                    "returned",
                    "process_running",
                    "unobserved",
                ] {
                    if key != "unobserved" || returned.is_none() {
                        counts[key] = json!(
                            counts[key].as_u64().unwrap()
                                + one["analysis"][key].as_u64().unwrap_or(0)
                        );
                    }
                }
            }
            offset += rows.len();
        }
        result["analysis"] = counts;
        if let Some(row) = returned {
            result["return"] = row;
        }
        return Ok(result);
    }
    unreachable!()
}

fn entity(conn: &mut SqliteConnection, session: &str, category: &str, id: &str) -> Result<Value> {
    Ok(diesel::sql_query("SELECT document FROM dashboard_session_entities WHERE session=? AND category=? AND entity=?")
        .bind::<Text,_>(session).bind::<Text,_>(category).bind::<Text,_>(id).get_result::<Document>(conn).optional()?.map(|r|serde_json::from_str(&r.document)).transpose()?.unwrap_or(Value::Null))
}
fn scalar(conn: &mut SqliteConnection, session: &str, sql: &str) -> Result<i64> {
    Ok(diesel::sql_query(sql)
        .bind::<Text, _>(session)
        .get_result::<Position>(conn)?
        .position)
}
fn save_row(
    conn: &mut SqliteConnection,
    id: &str,
    view: &str,
    ordinal: usize,
    row: &Value,
) -> Result<()> {
    diesel::sql_query("INSERT INTO dashboard_timeline_rows(session,view,ordering,ordinal,document) VALUES(?,?,'oldest',?,?) ON CONFLICT(session,view,ordering,ordinal) DO UPDATE SET document=excluded.document WHERE document!=excluded.document")
        .bind::<Text,_>(id).bind::<Text,_>(view).bind::<BigInt,_>(ordinal as i64).bind::<Text,_>(row.to_string()).execute(conn)?;
    Ok(())
}
/// Rebuild ordering-dependent rows with bounded working state. Only affected
/// sessions are touched; independent entity heads above update by identity.
pub(super) fn refresh(conn: &mut SqliteConnection, id: &str) -> Result<()> {
    let count=scalar(conn,id,"SELECT COUNT(*) AS position FROM dashboard_events WHERE session_id=? AND partition='workflow'")?;
    if count == 0 {
        diesel::sql_query("DELETE FROM dashboard_reports WHERE key IN (?,?,?)")
            .bind::<Text, _>(format!("directory:{id}"))
            .bind::<Text, _>(format!("session:{id}"))
            .bind::<Text, _>(format!("session-summary:{id}"))
            .execute(conn)?;
        return Ok(());
    }
    let mut summary = json!({"session_id":id,"label":"Untitled session","agent":"Unknown","model":"Unknown","cwd":"Unknown","initial_goal":"","status":"observed","last_at":"","first_at":"","event_count":count,"empty":true});
    for field in [
        "session_name",
        "herdr_tab",
        "agent",
        "model",
        "cwd",
        "status",
    ] {
        let head = entity(conn, id, "metadata", field)?;
        if !head.is_null() {
            summary[field] = head["value"].clone();
        }
    }
    // Prompt numbers follow first source occurrence; store the ordinal alongside
    // the head so each timeline row needs only a point lookup.
    conn.batch_execute("CREATE TEMP TABLE IF NOT EXISTS workflow_prompt_order(entity TEXT PRIMARY KEY, number BIGINT); DELETE FROM workflow_prompt_order;")?;
    diesel::sql_query("INSERT INTO workflow_prompt_order SELECT entity, ROW_NUMBER() OVER (ORDER BY json_extract(document,'$.first_at'), (SELECT d.recorded_at_us FROM dashboard_session_facts f JOIN dashboard_events d ON d.id=f.id WHERE f.session=e.session AND f.category='prompt' AND f.entity=e.entity ORDER BY json_extract(d.document,'$.payload.occurred_at'),d.recorded_at_us,f.position LIMIT 1), (SELECT f.position FROM dashboard_session_facts f JOIN dashboard_events d ON d.id=f.id WHERE f.session=e.session AND f.category='prompt' AND f.entity=e.entity ORDER BY json_extract(d.document,'$.payload.occurred_at'),d.recorded_at_us,f.position LIMIT 1)) FROM dashboard_session_entities e WHERE session=? AND category='prompt' AND json_extract(document,'$.complete')=1")
        .bind::<Text,_>(id).execute(conn)?;
    if let Some(row)=diesel::sql_query("SELECT e.document FROM dashboard_session_entities e JOIN workflow_prompt_order p ON p.entity=e.entity WHERE e.session=? AND e.category='prompt' AND p.number=1").bind::<Text,_>(id).get_result::<Document>(conn).optional()? {summary["initial_goal"]=serde_json::from_str::<Value>(&row.document)?["text"].clone();}
    let mut counts = json!({"succeeded":0,"failed":0,"returned":0,"process_running":0,"unobserved":0,"check_failures":0});
    let mut checks = json!({});
    let mut outcome = json!({"status":"","text":""});
    let mut decisions = 0;
    let mut first_activity = false;
    let mut label = None;
    let mut description = None;
    let mut cursor: Option<(i64, String, i64, i64)> = None;
    let mut signal_ordinal = 0;
    let mut stream = crate::workflow::SignalStream::default();
    loop {
        let (has, time, recorded, pos) = cursor
            .as_ref()
            .map(|(h, t, r, p)| (*h, t.as_str(), *r, *p))
            .unwrap_or((-1, "", i64::MIN, i64::MIN));
        let rows=diesel::sql_query("SELECT position,(json_extract(document,'$.payload.occurred_at') IS NOT NULL) AS has_time,COALESCE(json_extract(document,'$.payload.occurred_at'),'') AS occurred_at,recorded_at_us,document FROM dashboard_events WHERE session_id=? AND partition='workflow' AND ((json_extract(document,'$.payload.occurred_at') IS NOT NULL),COALESCE(json_extract(document,'$.payload.occurred_at'),''),recorded_at_us,position) > (?,?,?,?) ORDER BY (json_extract(document,'$.payload.occurred_at') IS NOT NULL),COALESCE(json_extract(document,'$.payload.occurred_at'),''),recorded_at_us,position LIMIT 64")
            .bind::<Text,_>(id).bind::<BigInt,_>(has).bind::<Text,_>(time).bind::<BigInt,_>(recorded).bind::<BigInt,_>(pos).load::<TimelineSource>(conn)?;
        if rows.is_empty() {
            break;
        }
        for source in rows {
            cursor = Some((
                source.has_time,
                source.occurred_at,
                source.recorded_at_us,
                source.position,
            ));
            let row: Value = serde_json::from_str(&source.document)?;
            let p = &row["payload"];
            let kind = row["event_type"].as_str().unwrap_or("");
            if kind != "workflow.session_updated" {
                if !first_activity {
                    summary["first_at"] = p["occurred_at"].clone();
                    first_activity = true;
                }
                summary["last_at"] = p["occurred_at"].clone();
            } else if !first_activity {
                summary["first_at"] = p["occurred_at"].clone();
                summary["last_at"] = p["occurred_at"].clone();
            }
            if label.is_none() && kind.starts_with("workflow.task_") {
                label = p["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty() && *s != "Session work")
                    .map(str::to_owned);
            }
            if description.is_none() && p["name"].as_str().is_some_and(|s| s != "Session work") {
                description = p["description"].as_str().map(str::to_owned);
            }
            let cached =
                diesel::sql_query("SELECT document FROM dashboard_workflow_annotations WHERE id=?")
                    .bind::<Text, _>(row["id"].as_str().unwrap_or(""))
                    .get_result::<Document>(conn)
                    .optional()?;
            let event = if let Some(cached) = cached {
                serde_json::from_str(&cached.document)?
            } else {
                let task = entity(conn, id, "task", p["task_id"].as_str().unwrap_or(""))?;
                let tool = entity(conn, id, "tool", p["tool_call_id"].as_str().unwrap_or(""))?;
                let mut one = json!({"tasks":{},"events":[{"event_type":row["event_type"],"event_id":row["id"],"recorded_at_us":row["recorded_at_us"],"payload":p}]});
                if let Some(task_id) = p["task_id"].as_str() {
                    if !task.is_null() {
                        one["tasks"][task_id] = task["task"].clone();
                    }
                }
                if kind == "workflow.tool_started" && tool["returned"] == true {
                    one["events"].as_array_mut().unwrap().push(json!({"event_type":"workflow.tool_completed","payload":{"tool_call_id":p["tool_call_id"]}}));
                }
                crate::workflow::annotate_timeline(&mut one);
                let mut event = one["events"][0].take();
                if kind.starts_with("workflow.plan_") && task["last_plan_id"] != row["id"] {
                    let (signal, headline, detail, tone) = crate::workflow::signal_view(
                        kind.trim_start_matches("workflow."),
                        p,
                        event["result_status"].as_str().unwrap_or(""),
                        event["task_name"].as_str().unwrap_or(""),
                        false,
                    );
                    event["signal"] = json!(signal);
                    event["headline"] = json!(headline);
                    event["detail"] = json!(detail);
                    event["tone"] = json!(tone);
                }
                if let Some(prompt_id) = p["prompt_id"].as_str() {
                    let prompt = entity(conn, id, "prompt", prompt_id)?;
                    if prompt["complete"] == true {
                        let number = diesel::sql_query(
                            "SELECT number AS position FROM workflow_prompt_order WHERE entity=?",
                        )
                        .bind::<Text, _>(prompt_id)
                        .get_result::<Position>(conn)?
                        .position;
                        event["prompt_number"] = json!(number);
                        event["prompt_preview"] = prompt["preview"].clone();
                        event["prompt_text"] = prompt["text"].clone();
                    }
                }
                diesel::sql_query("INSERT INTO dashboard_workflow_annotations(id,session,position,document) VALUES(?,?,?,?)").bind::<Text,_>(row["id"].as_str().unwrap_or("")).bind::<Text,_>(id).bind::<BigInt,_>(source.position).bind::<Text,_>(event.to_string()).execute(conn)?;
                event
            };
            let status = event["result_status"].as_str().unwrap_or("");
            if kind == "workflow.tool_completed"
                || kind == "workflow.tool_failed"
                || status == "unobserved"
            {
                if let Some(n) = counts.get_mut(status) {
                    *n = json!(n.as_u64().unwrap() + 1);
                }
            }
            if event["category"] == "check" && status == "failed" {
                counts["check_failures"] = json!(counts["check_failures"].as_u64().unwrap() + 1);
            }
            if matches!(
                event["category"].as_str(),
                Some("prompt" | "todo" | "plan" | "check" | "decision")
            ) || (event["category"] == "task" && event["task_name"] != "Session work")
            {
                summary["empty"] = json!(false);
            }
            if matches!(kind, "workflow.task_completed" | "workflow.task_blocked") {
                outcome = json!({"status":if kind=="workflow.task_completed"{"completed"}else{"blocked"},"text":if p["outcome"].as_str().unwrap_or("").is_empty(){p["description"].as_str().unwrap_or("")}else{p["outcome"].as_str().unwrap_or("")},"task":event["task_name"],"at":p["occurred_at"]});
            }
            if matches!(kind, "workflow.verification" | "workflow.review") {
                let key = if kind == "workflow.review" {
                    "review"
                } else {
                    "verify"
                };
                if checks[key].is_null() {
                    checks[key] = json!({"passed":0,"total":0});
                }
                checks[key]["total"] = json!(checks[key]["total"].as_u64().unwrap() + 1);
                if status == "passed" {
                    checks[key]["passed"] = json!(checks[key]["passed"].as_u64().unwrap() + 1);
                }
                checks[key]["last"] = json!(status);
                checks[key]["last_at"] = p["occurred_at"].clone();
            }
            if kind == "workflow.decision" {
                decisions += 1;
            }
            if kind == "workflow.prompt_recorded" && p["part_index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            let mut error = None;
            let items = stream.push(&event, &mut |call| match tool_return(conn, id, call) {
                Ok(v) => v,
                Err(e) => {
                    error = Some(e);
                    None
                }
            });
            if let Some(error) = error {
                return Err(error);
            }
            for item in items {
                save_row(conn, id, "signal", signal_ordinal, &item)?;
                signal_ordinal += 1;
            }
        }
    }
    let mut error = None;
    let items = stream.finish(&mut |call| match tool_return(conn, id, call) {
        Ok(v) => v,
        Err(e) => {
            error = Some(e);
            None
        }
    });
    if let Some(error) = error {
        return Err(error);
    }
    for item in items {
        save_row(conn, id, "signal", signal_ordinal, &item)?;
        signal_ordinal += 1;
    }
    diesel::sql_query("DELETE FROM dashboard_timeline_rows WHERE session=? AND view='signal' AND ordering='oldest' AND ordinal>=?").bind::<Text,_>(id).bind::<BigInt,_>(signal_ordinal as i64).execute(conn)?;
    // Reversed chapter order is computed on demand from the single durable
    // signal ordering, so appends never rewrite every historical reversed row.
    summary["label"] = json!(crate::workflow::humanize(
        label
            .as_deref()
            .or(summary["session_name"].as_str())
            .or(summary["initial_goal"]
                .as_str()
                .filter(|s| !s.trim().is_empty()))
            .unwrap_or("Untitled session")
    )
    .split_whitespace()
    .take(5)
    .collect::<Vec<_>>()
    .join(" "));
    let (project, worktree) = crate::workflow::project(summary["cwd"].as_str().unwrap_or(""));
    summary["project"] = json!(project);
    summary["worktree"] = json!(worktree);
    summary["analysis"] = counts.clone();
    summary["analysis_metrics"] = json!([
        ("succeeded", "Tools succeeded"),
        ("failed", "Tools failed"),
        ("returned", "Returns · result unknown"),
        ("process_running", "Process handles returned"),
        ("unobserved", "Completion unobserved"),
        ("check_failures", "Failed checks")
    ]
    .map(|(k, label)| json!({"label":label,"count":counts[k]})));
    let total=scalar(conn,id,"SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='todo' AND json_extract(document,'$.todo.status')!='removed'")?;
    let done=scalar(conn,id,"SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='todo' AND json_extract(document,'$.todo.status')='completed'")?;
    let mut attention = Vec::new();
    if outcome["status"] == "blocked" {
        attention.push("task blocked");
    }
    if checks["verify"]["last"] == "failed" || checks["review"]["last"] == "failed" {
        attention.push("check failed");
    }
    summary["verdict"] = json!({"outcome":outcome,"checks":checks,"todos_done":done,"todos_total":total,"tool_calls":(["succeeded","failed","returned","process_running","unobserved"].iter().map(|k|counts[k].as_u64().unwrap()).sum::<u64>()),"tool_failures":counts["failed"],"decisions":decisions,"attention":attention,"unfinished":summary["status"]=="ended" && outcome["status"]!="completed" && done<total,"verified":checks["verify"]["last"]=="passed" && checks["review"]["last"]=="passed","duration":crate::workflow::elapsed(summary["first_at"].as_str().unwrap_or(""),summary["last_at"].as_str().unwrap_or(""))});
    summary["task_count"]=json!(scalar(conn,id,"SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='task'")?);
    summary["completed_tasks"]=json!(scalar(conn,id,"SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='task' AND json_extract(document,'$.task.status')='completed'")?);
    summary["prompt_count"]=json!(scalar(conn,id,"SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='prompt' AND json_extract(document,'$.complete')=1")?);
    let phases=diesel::sql_query("SELECT DISTINCT json_extract(document,'$.task.phase') AS document FROM dashboard_session_entities WHERE session=? AND category='task' AND json_extract(document,'$.task.phase')!='' ORDER BY document").bind::<Text,_>(id).load::<Document>(conn)?;
    summary["phases"] = json!(phases.into_iter().map(|r| r.document).collect::<Vec<_>>());
    let description = description
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(summary["initial_goal"].as_str().unwrap_or(""));
    let preview = description.chars().take(180).collect::<String>();
    summary["preview"] = json!(if description.chars().count() > 180 {
        format!("{preview}…")
    } else {
        preview
    });
    // The searchable event bodies remain in indexed individual rows, never in
    // a second session-sized concatenated string.
    summary["search_text"] = json!([
        "label",
        "session_name",
        "herdr_tab",
        "agent",
        "model",
        "cwd",
        "status",
        "session_id"
    ]
    .iter()
    .filter_map(|k| summary[k].as_str())
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase());
    summary.as_object_mut().unwrap().remove("initial_goal");
    save(conn, &format!("directory:{id}"), &summary)?;
    save(conn, &format!("session:{id}"), &summary)?;
    Ok(())
}
fn tool_return(conn: &mut SqliteConnection, id: &str, call: &str) -> Result<Option<Value>> {
    let head = entity(conn, id, "tool", call)?;
    if head["returned"] != true {
        return Ok(None);
    }
    let row = &head["return"];
    let p = &row["payload"];
    let mut one = json!({"tasks":{},"events":[{"event_type":row["event_type"],"payload":p}]});
    crate::workflow::annotate_timeline(&mut one);
    Ok(Some(
        json!({"status":one["events"][0]["result_status"],"duration_ms":p["duration_ms"],"exit_code":p["exit_code"],"label":p["tool_label"],"command":p["tool_command"]}),
    ))
}

pub(super) fn task(
    conn: &mut SqliteConnection,
    id: &str,
    task_id: &str,
    offset: usize,
    limit: usize,
) -> Result<Option<Value>> {
    let head = entity(conn, id, "task", task_id)?;
    if head.is_null() {
        return Ok(None);
    }
    let mut task = head["task"].clone();
    let entity_order = entity_order();
    task["sequence"] = json!(diesel::sql_query(format!("SELECT position FROM (SELECT h.entity, ROW_NUMBER() OVER (ORDER BY {entity_order}) AS position FROM dashboard_session_entities h WHERE h.session=? AND h.category='task') WHERE entity=?"))
        .bind::<Text,_>(id).bind::<Text,_>(task_id).get_result::<Position>(conn)?.position);
    let todos=documents(diesel::sql_query(format!("SELECT h.document FROM dashboard_session_entities h WHERE h.session=? AND h.category='todo' AND json_extract(h.document,'$.task_id')=? ORDER BY {entity_order} LIMIT ? OFFSET ?"))
        .bind::<Text,_>(id).bind::<Text,_>(task_id).bind::<BigInt,_>(limit as i64).bind::<BigInt,_>(offset as i64).load::<Document>(conn)?)?;
    let mut todo_list = Vec::new();
    for (n, head) in todos.into_iter().enumerate() {
        let mut todo = head["todo"].clone();
        todo["sequence"] = json!(offset + n + 1);
        let key = json!([task_id, todo["id"]]).to_string();
        todo["history"] = json!(facts(
            conn,
            id,
            "todo",
            &key,
            &format!("ORDER BY {ORDER} LIMIT {limit}")
        )?
        .into_iter()
        .map(|r| r["payload"].clone())
        .collect::<Vec<_>>());
        todo["history_count"] = head["events"].clone();
        todo_list.push(todo);
    }
    task["todo_list"] = json!(todo_list);
    task["todos"] = Value::Object(
        task["todo_list"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["id"].as_str().unwrap_or("").to_owned(), t.clone()))
            .collect(),
    );
    task["plan_revisions"]=json!(facts(conn,id,"task",task_id,&format!("AND e.event_type LIKE 'workflow.plan_%' ORDER BY {ORDER} LIMIT {limit} OFFSET {offset}"))?.into_iter().map(|r|r["payload"].clone()).collect::<Vec<_>>());
    for (field, condition) in [
        (
            "todos_done",
            "json_extract(document,'$.todo.status')='completed'",
        ),
        (
            "todos_total",
            "json_extract(document,'$.todo.status')!='removed'",
        ),
        ("todo_count", "1"),
    ] {
        task[field]=json!(diesel::sql_query(format!("SELECT COUNT(*) AS position FROM dashboard_session_entities WHERE session=? AND category='todo' AND json_extract(document,'$.task_id')=? AND {condition}"))
            .bind::<Text,_>(id).bind::<Text,_>(task_id).get_result::<Position>(conn)?.position);
    }
    task["plan_count"]=json!(diesel::sql_query("SELECT COUNT(*) AS position FROM dashboard_session_facts f JOIN dashboard_events e ON e.id=f.id WHERE f.session=? AND f.category='task' AND f.entity=? AND e.event_type LIKE 'workflow.plan_%'").bind::<Text,_>(id).bind::<Text,_>(task_id).get_result::<Position>(conn)?.position);
    let phases=facts(conn,id,"task",task_id,&format!("AND json_type(e.document,'$.payload.phase')='text' AND f.position=(SELECT f2.position FROM dashboard_session_facts f2 JOIN dashboard_events e2 ON e2.id=f2.id WHERE f2.session=f.session AND f2.category=f.category AND f2.entity=f.entity AND json_extract(e2.document,'$.payload.phase')=json_extract(e.document,'$.payload.phase') ORDER BY json_extract(e2.document,'$.payload.occurred_at') DESC,e2.recorded_at_us DESC,f2.position DESC LIMIT 1) ORDER BY {ORDER} LIMIT {limit} OFFSET {offset}"))?;
    for phase in phases {
        if let Some(name) = phase["payload"]["phase"].as_str() {
            task["phases"][name] = phase["payload"]["phase_status"].clone();
        }
    }
    task["detail_more"] = json!(
        task["todo_count"]
            .as_u64()
            .unwrap_or(0)
            .max(task["plan_count"].as_u64().unwrap_or(0))
            > offset.saturating_add(limit) as u64
    );
    task["detail_offset"] = json!(offset);
    task["detail_limit"] = json!(limit);
    Ok(Some(task))
}

impl ReportStore {
    pub fn workflow_page(
        &self,
        id: &str,
        prompts: usize,
        tasks: usize,
        expanded: Option<&str>,
        detail_offset: usize,
    ) -> Result<Option<Value>> {
        let Some(mut value) = self.session_without_events(id)? else {
            return Ok(None);
        };
        value["prompts"] = json!(
            self.session_prompts(id, 0, prompts.clamp(1, 100))?
                .unwrap()
                .0
        );
        value["task_list"] = json!(self.session_tasks(id, 0, tasks.clamp(1, 100))?.unwrap().0);
        if let Some(task) = expanded {
            if let Some(detail) = self.session_task(id, task, detail_offset, 50)? {
                let list = value["task_list"].as_array_mut().unwrap();
                if let Some(row) = list
                    .iter_mut()
                    .find(|row| row["task_id"] == detail["task_id"])
                {
                    *row = detail.clone();
                } else {
                    list.push(detail.clone());
                }
                value["expanded_task"] = detail;
            }
        }
        Ok(Some(value))
    }
    pub fn session_todos(&self, id: &str, offset: usize, limit: usize) -> Result<Vec<Value>> {
        let mut conn = self.connection()?;
        let rows=documents(diesel::sql_query("SELECT document FROM dashboard_session_entities WHERE session=? AND category='todo' AND json_extract(document,'$.todo.status')!='removed' ORDER BY json_extract(document,'$.first_at'),entity LIMIT ? OFFSET ?")
            .bind::<Text,_>(id).bind::<BigInt,_>(limit.min(100) as i64).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(&mut conn)?)?;
        Ok(rows
            .into_iter()
            .map(|head| {
                let mut todo = head["todo"].clone();
                todo["task_id"] = head["task_id"].clone();
                todo
            })
            .collect())
    }
    pub fn filtered_timeline<F>(
        &self,
        id: &str,
        signal: bool,
        newest: bool,
        offset: usize,
        limit: usize,
        filter: F,
    ) -> Result<Option<(Vec<Value>, usize)>>
    where
        F: Fn(&Value) -> bool + Send + std::panic::UnwindSafe + 'static,
    {
        let mut conn = self.connection()?;
        if cached(&mut conn, &format!("directory:{id}"))?.is_none() {
            return Ok(None);
        }
        conn.register_sql_function::<Text, BigInt, String, i64, _>(
            "timeline_match",
            true,
            move |s| {
                serde_json::from_str::<Value>(&s)
                    .ok()
                    .map_or(0, |v| i64::from(filter(&v)))
            },
        )?;
        conn.transaction::<_,anyhow::Error,_>(|conn| {
            conn.batch_execute("CREATE TEMP TABLE filtered_source(ordinal INTEGER PRIMARY KEY,document TEXT NOT NULL); CREATE TEMP TABLE filtered_signal(ordinal INTEGER PRIMARY KEY,document TEXT NOT NULL);")?;
            diesel::sql_query("INSERT INTO filtered_source SELECT ROW_NUMBER() OVER (ORDER BY json_extract(document,'$.payload.occurred_at'),json_extract(document,'$.recorded_at_us'),position)-1,document FROM dashboard_workflow_annotations WHERE session=? AND (json_extract(document,'$.event_type')!='workflow.prompt_recorded' OR COALESCE(json_extract(document,'$.payload.part_index'),0)=0) AND timeline_match(document)=1").bind::<Text,_>(id).execute(conn)?;
            let table=if signal {
                let mut cursor=-1i64;let mut ordinal=0;let mut stream=crate::workflow::SignalStream::default();
                loop {
                    #[derive(QueryableByName)] struct Item {#[diesel(sql_type=BigInt)] position:i64,#[diesel(sql_type=Text)] document:String}
                    let rows=diesel::sql_query("SELECT ordinal AS position,document FROM filtered_source WHERE ordinal>? ORDER BY ordinal LIMIT 64").bind::<BigInt,_>(cursor).load::<Item>(conn)?;if rows.is_empty(){break;}
                    for row in rows {cursor=row.position;let event=serde_json::from_str(&row.document)?;let mut error=None;let items=stream.push(&event,&mut |call|match filtered_return(conn,call){Ok(v)=>v,Err(e)=>{error=Some(e);None}});if let Some(e)=error{return Err(e);}for item in items{insert_filtered(conn,ordinal,&item)?;ordinal+=1;}}
                }
                let mut error=None;let items=stream.finish(&mut |call|match filtered_return(conn,call){Ok(v)=>v,Err(e)=>{error=Some(e);None}});if let Some(e)=error{return Err(e);}for item in items{insert_filtered(conn,ordinal,&item)?;ordinal+=1;}
                "filtered_signal"
            }else{"filtered_source"};
            let count=diesel::sql_query(format!("SELECT COUNT(*) AS position FROM {table}")).get_result::<Position>(conn)?.position as usize;
            let sql=if newest && signal {format!("SELECT document FROM (SELECT *,SUM(CASE WHEN json_extract(document,'$.category')='prompt' THEN 1 ELSE 0 END) OVER (ORDER BY ordinal) AS chapter FROM {table}) ORDER BY chapter DESC, CASE WHEN json_extract(document,'$.category')='prompt' THEN 0 ELSE 1 END,ordinal DESC LIMIT ? OFFSET ?")}else{format!("SELECT document FROM {table} ORDER BY ordinal {} LIMIT ? OFFSET ?",if newest{"DESC"}else{"ASC"})};
            let rows=documents(diesel::sql_query(sql).bind::<BigInt,_>(limit.min(1000) as i64).bind::<BigInt,_>(offset.min(i64::MAX as usize) as i64).load::<Document>(conn)?)?;
            Ok(Some((rows,count)))
        })
    }
}
fn insert_filtered(conn: &mut SqliteConnection, ordinal: i64, item: &Value) -> Result<()> {
    diesel::sql_query("INSERT INTO filtered_signal VALUES(?,?)")
        .bind::<BigInt, _>(ordinal)
        .bind::<Text, _>(item.to_string())
        .execute(conn)?;
    Ok(())
}
fn filtered_return(conn: &mut SqliteConnection, call: &str) -> Result<Option<Value>> {
    let row=diesel::sql_query("SELECT document FROM filtered_source WHERE json_extract(document,'$.category')='tool' AND json_extract(document,'$.event_type')!='workflow.tool_started' AND json_extract(document,'$.payload.tool_call_id')=? ORDER BY ordinal DESC LIMIT 1").bind::<Text,_>(call).get_result::<Document>(conn).optional()?;
    Ok(row.map(|r|serde_json::from_str::<Value>(&r.document)).transpose()?.map(|e|json!({"status":e["result_status"],"duration_ms":e["payload"]["duration_ms"],"exit_code":e["payload"]["exit_code"],"label":e["payload"]["tool_label"],"command":e["payload"]["tool_command"]})))
}

pub(super) fn invalidate(
    conn: &mut SqliteConnection,
    id: &str,
    category: &str,
    key: &str,
    head: &Value,
) -> Result<()> {
    let old = entity(conn, id, category, key)?;
    if category == "task" {
        if old["task"]["name"] != head["task"]["name"] {
            diesel::sql_query("DELETE FROM dashboard_workflow_annotations WHERE session=? AND json_extract(document,'$.payload.task_id')=?").bind::<Text,_>(id).bind::<Text,_>(key).execute(conn)?;
        } else if old["last_plan_id"] != head["last_plan_id"] {
            // Only the former/new latest plan can change visibility.
            for plan in [&old["last_plan_id"], &head["last_plan_id"]] {
                if let Some(plan) = plan.as_str() {
                    diesel::sql_query("DELETE FROM dashboard_workflow_annotations WHERE id=?")
                        .bind::<Text, _>(plan)
                        .execute(conn)?;
                }
            }
        }
    } else if category == "tool" && old["returned"] != head["returned"] {
        diesel::sql_query("DELETE FROM dashboard_workflow_annotations WHERE session=? AND json_extract(document,'$.payload.tool_call_id')=? AND json_extract(document,'$.event_type')='workflow.tool_started'").bind::<Text,_>(id).bind::<Text,_>(key).execute(conn)?;
    } else if category == "prompt" && old != *head {
        // Completion, replacement or an earlier prompt can renumber chapters.
        diesel::sql_query("DELETE FROM dashboard_workflow_annotations WHERE session=? AND json_extract(document,'$.event_type')='workflow.prompt_recorded'").bind::<Text,_>(id).execute(conn)?;
    }
    Ok(())
}
