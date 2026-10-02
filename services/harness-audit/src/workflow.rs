use serde_json::{json, Value};
use std::collections::BTreeMap;

const TYPES: &[&str] = &[
    "prompt_recorded",
    "session_updated",
    "session_started",
    "session_ended",
    "task_started",
    "task_updated",
    "task_completed",
    "task_blocked",
    "decision",
    "tool_started",
    "tool_completed",
    "tool_failed",
    "phase_changed",
    "verification",
    "review",
];
const KEYS: &[&str] = &[
    "prompt_id",
    "part_index",
    "part_count",
    "text",
    "cwd",
    "session_name",
    "agent",
    "model",
    "schema_version",
    "session_id",
    "task_id",
    "parent_task_id",
    "name",
    "description",
    "status",
    "outcome",
    "options",
    "selected",
    "tool_name",
    "tool_call_id",
    "started_at",
    "duration_ms",
    "process_id",
    "exit_code",
    "occurred_at",
    "run_id",
    "phase",
    "phase_status",
    "check",
    "ran",
    "skipped",
    "failures",
    "source",
    "project_id",
];

pub fn validate(event_type: &str, payload: &Value) -> Result<(), &'static str> {
    if !TYPES.contains(&event_type.trim_start_matches("workflow.")) {
        return Err("unknown Workflow event type");
    }
    let object = payload.as_object().ok_or("payload must be an object")?;
    if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return Err("unsupported Workflow field");
    }
    if payload["schema_version"] != 1 {
        return Err("unsupported Workflow schema version");
    }
    for key in ["session_id", "occurred_at"] {
        if payload[key]
            .as_str()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 256)
        {
            return Err("missing or invalid session ID / timestamp");
        }
    }
    if !event_type.starts_with("workflow.session_")
        && payload["task_id"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 256)
    {
        return Err("task ID required");
    }
    if object.contains_key("cwd")
        && payload["cwd"]
            .as_str()
            .is_none_or(|s| !std::path::Path::new(s).is_absolute())
    {
        return Err("working directory must be an absolute path");
    }
    for key in ["agent", "model", "session_name"] {
        if object.contains_key(key) && payload[key].as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err("invalid session metadata");
        }
    }
    for (key, value) in object {
        if let Some(s) = value.as_str() {
            if s.len() > 2000 {
                return Err("Workflow text exceeds 2000 bytes");
            }
        } else if key == "options" {
            let options = value.as_array().ok_or("options must be an array")?;
            if options.len() > 20
                || options
                    .iter()
                    .any(|v| v.as_str().is_none_or(|s| s.len() > 200))
            {
                return Err("invalid decision options");
            }
        } else if !value.is_null() && !value.is_number() {
            return Err("invalid Workflow field value");
        }
    }
    if event_type != "workflow.prompt_recorded"
        && ["prompt_id", "part_index", "part_count", "text"]
            .iter()
            .any(|key| object.contains_key(*key))
    {
        return Err("prompt fields require a prompt event");
    }
    if event_type == "workflow.prompt_recorded" {
        let count = payload["part_count"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 2000)
            .ok_or("invalid prompt part count")?;
        let index = payload["part_index"]
            .as_u64()
            .ok_or("invalid prompt part index")?;
        if index >= count
            || payload["prompt_id"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 256)
            || !payload["text"].is_string()
        {
            return Err("invalid prompt chunk");
        }
    }
    if object.contains_key("duration_ms") && payload["duration_ms"].as_u64().is_none() {
        return Err("duration must be a nonnegative integer");
    }
    if object.contains_key("exit_code")
        && !payload["exit_code"].is_null()
        && payload["exit_code"].as_i64().is_none()
    {
        return Err("exit code must be an integer");
    }
    if event_type == "workflow.decision" {
        let selected = payload["selected"]
            .as_str()
            .ok_or("selected option required")?;
        if !payload["options"]
            .as_array()
            .is_some_and(|options| options.iter().any(|v| v == selected))
        {
            return Err("selected option must be one of the options");
        }
    }
    Ok(())
}

fn annotate_timeline(session: &mut Value) {
    let tasks = session["tasks"].clone();
    let completed: std::collections::HashSet<String> = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["event_type"] == "workflow.tool_completed"
                || e["event_type"] == "workflow.tool_failed"
        })
        .filter_map(|e| e["payload"]["tool_call_id"].as_str().map(str::to_owned))
        .collect();
    let mut counts = BTreeMap::<&str, usize>::from([
        ("succeeded", 0),
        ("failed", 0),
        ("returned", 0),
        ("process_running", 0),
        ("unobserved", 0),
        ("check_failures", 0),
    ]);
    for event in session["events"].as_array_mut().unwrap() {
        let p = &event["payload"];
        let kind = event["event_type"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("workflow.");
        let (title, category, status, explanation) = match kind {
            "tool_started" => ("Tool started", "tool", "started", "The runtime dispatched this tool. A later return records the result when available."),
            "tool_completed" => match p["outcome"].as_str() {
                Some("succeeded") => ("Tool succeeded", "tool", "succeeded", "The tool returned exit code 0. This does not prove that the overall task is complete."),
                Some("process_running") => ("Tool returned · process running", "tool", "process_running", "The tool returned a process handle. The process has not reported a final exit code."),
                _ => ("Tool returned", "tool", "returned", "The runtime reported a tool return. No explicit success or exit-code evidence was captured."),
            },
            "tool_failed" => ("Tool failed", "tool", "failed", "The runtime reported an error or a nonzero exit code. Review the task's later checks and outcome for recovery."),
            "verification" | "review" => if p["exit_code"].as_i64() == Some(0) {
                (if kind == "review" {"Review passed"} else {"Verification passed"}, "check", "passed", "The recorded check command exited successfully. Run and skipped counts show the coverage reported by the sensor.")
            } else if p["exit_code"].as_i64().is_some() {
                (if kind == "review" {"Review failed"} else {"Verification failed"}, "check", "failed", "The recorded check command failed. A later passing check is needed before completion.")
            } else { ("Check recorded", "check", "unknown", "No check exit code was captured.") },
            "phase_changed" => ("Harness phase changed", "phase", p["phase_status"].as_str().unwrap_or("unknown"), "This records a harness phase transition; it is separate from the final task outcome."),
            "decision" => ("Decision recorded", "decision", "recorded", "The supplied options, selection and description record the reason for this decision."),
            "prompt_recorded" => ("User prompt captured", "prompt", "recorded", "A user submission was captured while prompt storage was enabled. The complete text is shown under User prompts."),
            "task_completed" => ("Task marked completed", "task", "completed", "An explicit task outcome was supplied. Review the associated verification and review events as evidence."),
            "task_blocked" => ("Task blocked", "task", "blocked", "An explicit blocker was recorded; the description or outcome explains the remaining work."),
            "task_started" => ("Task started", "task", "running", "A task was registered in this session. Its name and description provide the intended scope."),
            "task_updated" => ("Task updated", "task", p["status"].as_str().unwrap_or("recorded"), "The task's supplied context or status changed."),
            "session_started" => ("Session started or resumed", "session", "running", "A runtime session was observed. Resume events retain the existing task binding."),
            "session_ended" => ("Session ended", "session", "ended", "The runtime ended this session. This does not mark its tasks completed."),
            "session_updated" => ("Session metadata updated", "session", "recorded", "The agent, model, title or working directory metadata changed or was recovered."),
            _ => ("Event recorded", "other", "unknown", "No additional interpretation is available for this event."),
        };
        let mut status = status.to_owned();
        if kind == "tool_started"
            && !p["tool_call_id"]
                .as_str()
                .is_some_and(|id| completed.contains(id))
        {
            status = "unobserved".to_owned();
        }
        if kind == "tool_completed" || kind == "tool_failed" || status == "unobserved" {
            if let Some(count) = counts.get_mut(status.as_str()) {
                *count += 1;
            }
        }
        if category == "check" && status == "failed" {
            *counts.get_mut("check_failures").unwrap() += 1;
        }
        let task_name = p["task_id"]
            .as_str()
            .and_then(|id| tasks[id]["name"].as_str())
            .unwrap_or("Session context")
            .to_owned();
        event["title"] = json!(title);
        event["category"] = json!(category);
        event["result_status"] = json!(status);
        event["explanation"] = json!(explanation);
        event["task_name"] = json!(task_name);
    }
    session["analysis_metrics"] = json!([
        ("succeeded", "Tools succeeded"),
        ("failed", "Tools failed"),
        ("returned", "Returns · result unknown"),
        ("process_running", "Process handles returned"),
        ("unobserved", "Completion unobserved"),
        ("check_failures", "Failed checks")
    ]
    .map(|(key, label)| json!({"label":label,"count":counts[key]})));
    session["analysis"] = json!(counts);
}

pub fn sessions(rows: &[Value]) -> Vec<Value> {
    let mut matching: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            r["event_type"]
                .as_str()
                .is_some_and(|s| s.starts_with("workflow."))
        })
        .collect();
    // Source time preserves order when an outage delays delivery; row time breaks ties.
    matching.sort_by(|a, b| {
        a["payload"]["occurred_at"]
            .as_str()
            .cmp(&b["payload"]["occurred_at"].as_str())
            .then(
                a["recorded_at_us"]
                    .as_i64()
                    .cmp(&b["recorded_at_us"].as_i64()),
            )
    });
    let mut sessions = BTreeMap::<String, Value>::new();
    for row in matching {
        let p = &row["payload"];
        let Some(id) = p["session_id"].as_str() else {
            continue;
        };
        let session = sessions.entry(id.to_owned()).or_insert_with(|| json!({"session_id": id, "label": "Untitled session", "agent": "Unknown", "model": "Unknown", "cwd": "Unknown", "metadata_history": [], "tasks": {}, "events": [], "prompts": [], "initial_goal": "", "status": "observed", "last_at": ""}));
        session["last_at"] = p["occurred_at"].clone();
        for key in ["session_name", "agent", "model", "cwd"] {
            if p[key].as_str().is_some_and(|s| !s.trim().is_empty()) {
                session[key] = p[key].clone();
            }
        }
        if row["event_type"] == "workflow.session_updated" {
            session["metadata_history"]
                .as_array_mut()
                .unwrap()
                .push(p.clone());
        }
        if row["event_type"] == "workflow.session_started" {
            session["status"] = json!("running");
        }
        if row["event_type"] == "workflow.session_ended" {
            session["status"] = json!("ended");
        }
        if let Some(task_id) = p["task_id"].as_str() {
            let tasks = session["tasks"].as_object_mut().unwrap();
            let task = tasks.entry(task_id.to_owned()).or_insert_with(|| json!({"task_id": task_id, "name": "Session work", "description": "", "status": "observed", "outcome": "", "phase": "", "parent_task_id": "", "phases": {}}));
            if row["event_type"]
                .as_str()
                .is_some_and(|s| s.starts_with("workflow.task_"))
            {
                for key in ["name", "description", "status", "outcome", "parent_task_id"] {
                    if !p[key].is_null() {
                        task[key] = p[key].clone();
                    }
                }
            }
            if let Some(phase) = p["phase"].as_str() {
                task["phase"] = p["phase"].clone();
                task["phases"][phase] = p["phase_status"].clone();
            }
        }
        session["events"]
            .as_array_mut()
            .unwrap()
            .push(json!({"event_type": row["event_type"], "event_id": row["id"], "recorded_at_us": row["recorded_at_us"], "payload": p}));
    }
    for session in sessions.values_mut() {
        let activity: Vec<&Value> = session["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event_type"] != "workflow.session_updated")
            .collect();
        let first = activity
            .first()
            .map(|e| e["payload"]["occurred_at"].clone())
            .unwrap_or_else(|| session["last_at"].clone());
        let last = activity
            .last()
            .map(|e| e["payload"]["occurred_at"].clone())
            .unwrap_or_else(|| session["last_at"].clone());
        session["first_at"] = first;
        session["last_at"] = last;
        let mut parts = BTreeMap::<String, (usize, BTreeMap<usize, String>, String)>::new();
        let mut order = Vec::new();
        for event in session["events"].as_array().unwrap() {
            if event["event_type"] != "workflow.prompt_recorded" {
                continue;
            }
            let p = &event["payload"];
            let Some(id) = p["prompt_id"].as_str() else {
                continue;
            };
            let Some(count) = p["part_count"].as_u64() else {
                continue;
            };
            let Some(index) = p["part_index"].as_u64() else {
                continue;
            };
            if !parts.contains_key(id) {
                order.push(id.to_owned());
            }
            let entry = parts.entry(id.to_owned()).or_insert_with(|| {
                (
                    count as usize,
                    BTreeMap::new(),
                    p["occurred_at"].as_str().unwrap_or("").to_owned(),
                )
            });
            entry
                .1
                .insert(index as usize, p["text"].as_str().unwrap_or("").to_owned());
        }
        for id in order {
            let (count, chunks, time) = &parts[&id];
            if chunks.len() != *count || !(0..*count).all(|i| chunks.contains_key(&i)) {
                continue;
            }
            let text = chunks.values().cloned().collect::<String>();
            if session["prompts"].as_array().unwrap().is_empty() {
                session["initial_goal"] = json!(text);
            }
            session["prompts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"prompt_id":id,"text":text,"occurred_at":time}));
        }
    }
    for session in sessions.values_mut() {
        let explicit = session["session_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty());
        let task_name = session["events"].as_array().unwrap().iter().find_map(|e| {
            if !e["event_type"]
                .as_str()
                .is_some_and(|s| s.starts_with("workflow.task_"))
            {
                return None;
            }
            e["payload"]["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && *s != "Session work")
        });
        let label = task_name
            .or(explicit)
            .or_else(|| {
                session["initial_goal"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or("Untitled session");
        session["label"] = json!(label
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" "));
    }
    for session in sessions.values_mut() {
        annotate_timeline(session);
    }
    let mut values: Vec<Value> = sessions.into_values().collect();
    values.sort_by(|a, b| b["last_at"].as_str().cmp(&a["last_at"].as_str()));
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_analysis_keeps_unknown_returns_separate_from_success_and_unmatched_starts() {
        let rows = vec![
            json!({"id":"event-1","recorded_at_us":123,"event_type":"workflow.tool_started","payload":{"session_id":"s","task_id":"t","tool_call_id":"a","tool_name":"Bash","occurred_at":"01"}}),
            json!({"event_type":"workflow.tool_started","payload":{"session_id":"s","task_id":"t","tool_call_id":"b","occurred_at":"02"}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","tool_call_id":"a","outcome":"succeeded","exit_code":0,"occurred_at":"03"}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","outcome":"returned","occurred_at":"04"}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","outcome":"process_running","occurred_at":"05"}}),
            json!({"event_type":"workflow.tool_failed","payload":{"session_id":"s","task_id":"t","outcome":"failed","occurred_at":"06"}}),
            json!({"event_type":"workflow.verification","payload":{"session_id":"s","task_id":"t","exit_code":1,"occurred_at":"07"}}),
        ];
        let result = sessions(&rows);
        let s = &result[0];
        for key in [
            "succeeded",
            "failed",
            "returned",
            "process_running",
            "unobserved",
            "check_failures",
        ] {
            assert_eq!(s["analysis"][key], 1);
        }
        assert_eq!(s["events"][0]["event_id"], "event-1");
        assert_eq!(s["events"][0]["result_status"], "started");
        assert_eq!(s["events"][1]["result_status"], "unobserved");
        assert_eq!(s["events"][3]["title"], "Tool returned");
        let mut p = json!({"schema_version":1,"session_id":"s","task_id":"t","occurred_at":"now","duration_ms":-1});
        assert!(validate("workflow.tool_completed", &p).is_err());
        p["duration_ms"] = json!(0);
        assert!(validate("workflow.tool_completed", &p).is_ok());
    }
    #[test]
    fn rejects_unsupported_fields_and_invalid_decisions() {
        let mut p = json!({"schema_version":1,"session_id":"s","task_id":"t","occurred_at":"2026-10-02T00:00:00Z","options":["run","hold"],"selected":"run"});
        assert!(validate("workflow.decision", &p).is_ok());
        p["selected"] = json!("other");
        assert!(validate("workflow.decision", &p).is_err());
        p["prompt"] = json!("private");
        assert!(validate("workflow.task_started", &p).is_err());
    }
    #[test]
    fn prompt_chunks_reassemble_only_when_complete_and_preserve_initial_goal() {
        let p = json!({"schema_version":1,"session_id":"s","task_id":"t","occurred_at":"2026-10-02T00:00:00Z","prompt_id":"p","part_index":0,"part_count":2,"text":"First "});
        assert!(validate("workflow.prompt_recorded", &p).is_ok());
        assert!(validate("workflow.task_updated", &p).is_err());
        let mut invalid = p.clone();
        invalid["part_index"] = json!(2);
        assert!(validate("workflow.prompt_recorded", &invalid).is_err());
        let first = json!({"event_type":"workflow.prompt_recorded","payload":p});
        assert!(sessions(&[first.clone()])[0]["prompts"]
            .as_array()
            .unwrap()
            .is_empty());
        let mut second = first.clone();
        second["payload"]["part_index"] = json!(1);
        second["payload"]["text"] = json!("goal <script>");
        second["payload"]["occurred_at"] = json!("2026-10-02T00:00:01Z");
        let mut later = first.clone();
        later["payload"]["prompt_id"] = json!("p2");
        later["payload"]["part_count"] = json!(1);
        later["payload"]["text"] = json!("Follow up");
        later["payload"]["occurred_at"] = json!("2026-10-02T00:00:02Z");
        let grouped = sessions(&[later, second, first]);
        assert_eq!(grouped[0]["initial_goal"], "First goal <script>");
        assert_eq!(grouped[0]["prompts"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn session_labels_are_short_and_runtime_changes_preserve_status_and_history() {
        let rows = vec![
            json!({"event_type":"workflow.task_started","payload":{"session_id":"s","task_id":"t","name":"Repair the audit dashboard session picker today","occurred_at":"01"}}),
            json!({"event_type":"workflow.session_ended","payload":{"session_id":"s","occurred_at":"02"}}),
            json!({"event_type":"workflow.session_updated","payload":{"session_id":"s","agent":"Codex","model":"model-v1","cwd":"/example/Working Tree","occurred_at":"03"}}),
            json!({"event_type":"workflow.session_updated","payload":{"session_id":"s","model":"model-v2","occurred_at":"04"}}),
        ];
        let grouped = sessions(&rows);
        assert_eq!(grouped[0]["label"], "Repair the audit dashboard session");
        assert_eq!(grouped[0]["agent"], "Codex");
        assert_eq!(grouped[0]["model"], "model-v2");
        assert_eq!(grouped[0]["cwd"], "/example/Working Tree");
        assert_eq!(grouped[0]["status"], "ended");
        assert_eq!(grouped[0]["last_at"], "02");
        assert_eq!(grouped[0]["first_at"], "01");
        assert_eq!(grouped[0]["metadata_history"].as_array().unwrap().len(), 2);
        let mut titled = rows;
        titled.push(json!({"event_type":"workflow.session_updated","payload":{"session_id":"s","session_name":"Session title takes precedence over task","occurred_at":"05"}}));
        assert_eq!(
            sessions(&titled)[0]["label"],
            "Repair the audit dashboard session"
        );
        let unknown = sessions(&[
            json!({"event_type":"workflow.session_started","payload":{"session_id":"u","occurred_at":"00"}}),
        ]);
        assert_eq!(unknown[0]["label"], "Untitled session");
        assert_eq!(unknown[0]["model"], "Unknown");
        assert_eq!(unknown[0]["cwd"], "Unknown");
        assert!(validate(
            "workflow.session_updated",
            &json!({"schema_version":1,"session_id":"s","occurred_at":"now","cwd":"relative/path"})
        )
        .is_err());
        assert!(validate(
            "workflow.session_updated",
            &json!({"schema_version":1,"session_id":"s","occurred_at":"now","model":5})
        )
        .is_err());
    }
    #[test]
    fn correlates_tasks_and_preserves_source_order() {
        let rows = vec![
            json!({"event_type":"workflow.task_completed","recorded_at_us":1,"payload":{"session_id":"s","task_id":"t","status":"completed","outcome":"Checks passed","occurred_at":"2026-10-02T02:00:00Z"}}),
            json!({"event_type":"workflow.task_started","recorded_at_us":2,"payload":{"session_id":"s","task_id":"t","name":"Example","status":"running","occurred_at":"2026-10-02T01:00:00Z"}}),
            json!({"event_type":"jev.route","payload":{}}),
        ];
        let grouped = sessions(&rows);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0]["tasks"]["t"]["name"], "Example");
        assert_eq!(grouped[0]["tasks"]["t"]["status"], "completed");
        assert_eq!(
            grouped[0]["events"][0]["event_type"],
            "workflow.task_started"
        );
    }
}
