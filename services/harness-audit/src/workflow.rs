use serde_json::{json, Value};
use std::collections::BTreeMap;

const TYPES: &[&str] = &[
    "plan_required",
    "plan_registered",
    "plan_revised",
    "plan_confirmed",
    "plan_exempted",
    "todo_created",
    "todo_updated",
    "todo_removed",
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
    "todo_id",
    "request_id",
    "criterion",
    "todo_required",
    "reason",
    "revision",
    "todo_count",
    "evidence",
    "previous_status",
    "previous_description",
    "previous_criterion",
    "prompt_id",
    "part_index",
    "part_count",
    "text",
    "cwd",
    "session_name",
    "herdr_tab",
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
    "tool_label",
    "tool_command",
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
    if object.contains_key("tool_label")
        && payload["tool_label"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 300)
    {
        return Err("tool label must be short text");
    }
    if object.contains_key("tool_command")
        && payload["tool_command"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err("tool command must be text");
    }
    if object.contains_key("cwd")
        && payload["cwd"]
            .as_str()
            .is_none_or(|s| !std::path::Path::new(s).is_absolute())
    {
        return Err("working directory must be an absolute path");
    }
    for key in ["agent", "model", "session_name", "herdr_tab"] {
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
    if event_type.starts_with("workflow.todo_") {
        for key in ["todo_id", "description", "criterion", "reason"] {
            if payload[key].as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err("todo identity, criterion and reason required");
            }
        }
        if !["pending", "in_progress", "completed", "blocked", "removed"]
            .contains(&payload["status"].as_str().unwrap_or(""))
        {
            return Err("invalid todo status");
        }
        if payload["status"] == "completed"
            && payload["evidence"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err("completion evidence required");
        }
        if payload["revision"].as_u64().is_none_or(|n| n == 0) {
            return Err("positive todo revision required");
        }
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

/// The collector's stand-in description for a task nobody has named yet.
const PLACEHOLDER: &str = "Task summary has not been supplied.";

fn text<'a>(p: &'a Value, key: &str) -> &'a str {
    p[key]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_start_matches(PLACEHOLDER)
}

/// `a · b` from the non-empty parts.
fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// How the signal view shows one event: whether it is kept at all, a one-line
/// headline, an optional detail line, and a tone for its marker. Tool calls
/// that did not fail are folded into bursts by `signal_items` instead.
fn signal_view(
    kind: &str,
    p: &Value,
    status: &str,
    task_name: &str,
    unresolved_plan: bool,
) -> (bool, String, String, &'static str) {
    let count = |key: &str| p[key].as_u64().map(|n| n.to_string());
    match kind {
        "prompt_recorded" => (true, String::new(), String::new(), "prompt"),
        "plan_registered" | "plan_revised" => {
            let todos = count("todo_count").map(|n| format!("{n} todos"));
            let head = if kind == "plan_revised" {
                "Plan revised"
            } else {
                "Plan registered"
            };
            (
                true,
                joined(&[head, todos.as_deref().unwrap_or("")]),
                joined(&[text(p, "description"), text(p, "reason")]),
                "neutral",
            )
        }
        "plan_required" => (
            unresolved_plan,
            "Waiting on a todo plan".into(),
            String::new(),
            "warn",
        ),
        "plan_exempted" => (
            true,
            "No execution needed".into(),
            joined(&[text(p, "description"), text(p, "reason")]),
            "neutral",
        ),
        "todo_created" | "todo_updated" | "todo_removed" => match status {
            "completed" => (
                true,
                format!("Todo done: {}", text(p, "description")),
                text(p, "evidence").to_owned(),
                "ok",
            ),
            "blocked" => (
                true,
                format!("Todo blocked: {}", text(p, "description")),
                text(p, "reason").to_owned(),
                "fail",
            ),
            "removed" => (
                true,
                format!("Todo removed: {}", text(p, "description")),
                text(p, "reason").to_owned(),
                "neutral",
            ),
            _ => (false, String::new(), String::new(), "neutral"),
        },
        "tool_started" | "tool_completed" | "tool_failed" if status == "failed" => {
            let tool = p["tool_name"].as_str().unwrap_or("Tool");
            let exit = p["exit_code"]
                .as_i64()
                .map(|code| format!(" (exit {code})"))
                .unwrap_or_default();
            // The command says exactly what failed; the label is the fallback.
            let detail = [text(p, "tool_command"), text(p, "tool_label")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_owned();
            (true, format!("{tool} failed{exit}"), detail, "fail")
        }
        "verification" | "review" => {
            let check = if kind == "review" { "review" } else { "verify" };
            let ran = count("ran").map(|n| format!("ran {n}"));
            let skipped = count("skipped").map(|n| format!("skipped {n}"));
            let failures = p["failures"]
                .as_u64()
                .filter(|n| *n > 0)
                .map(|n| format!("{n} failures"));
            let tone = match status {
                "passed" => "ok",
                "failed" => "fail",
                _ => "neutral",
            };
            let result = if status == "unknown" {
                "recorded"
            } else {
                status
            };
            (
                true,
                joined(&[
                    &format!("{check} {result}"),
                    ran.as_deref().unwrap_or(""),
                    skipped.as_deref().unwrap_or(""),
                    failures.as_deref().unwrap_or(""),
                ]),
                String::new(),
                tone,
            )
        }
        "phase_changed" => {
            let phase = text(p, "phase");
            match status {
                "active" => (true, format!("Phase: {phase}"), String::new(), "neutral"),
                "done" if phase == "review" => {
                    (true, "Review phase done".into(), String::new(), "ok")
                }
                _ => (false, String::new(), String::new(), "neutral"),
            }
        }
        "decision" => {
            let selected = text(p, "selected");
            let others: Vec<&str> = p["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|o| *o != selected)
                .collect();
            let over = if others.is_empty() {
                String::new()
            } else {
                format!("over {}", others.join(", "))
            };
            (
                true,
                format!("Decision: {selected}"),
                joined(&[text(p, "description"), &over]),
                "decision",
            )
        }
        "task_started" if task_name != "Session work" => (
            true,
            format!("Task started: {task_name}"),
            text(p, "description").to_owned(),
            "neutral",
        ),
        "task_completed" => (
            true,
            format!("Task completed: {task_name}"),
            joined(&[text(p, "outcome"), text(p, "description")]),
            "ok",
        ),
        "task_blocked" => (
            true,
            format!("Task blocked: {task_name}"),
            joined(&[text(p, "outcome"), text(p, "description")]),
            "fail",
        ),
        _ => (false, String::new(), String::new(), "neutral"),
    }
}

/// Elapsed time between two RFC 3339 instants, as `45s`, `12m`, `3h 5m` or `2d 4h`.
pub fn elapsed(from: &str, to: &str) -> String {
    let (Ok(from), Ok(to)) = (
        chrono::DateTime::parse_from_rfc3339(from),
        chrono::DateTime::parse_from_rfc3339(to),
    ) else {
        return String::new();
    };
    let secs = (to - from).num_seconds().max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..172_800 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// The signal view of a timeline: kept events in order, with each run of tool
/// calls between them folded into one burst row. A burst counts its calls from
/// the starts it saw, or from the returns when starts were not collected.
pub fn signal_items(events: &[Value]) -> Vec<Value> {
    #[derive(Default)]
    struct Burst {
        starts: BTreeMap<String, usize>,
        returns: BTreeMap<String, usize>,
        start_calls: Vec<Value>,
        return_calls: Vec<Value>,
        first_at: String,
        last_at: String,
    }
    // The flow diagram lists a burst's calls; very long runs keep the first ones.
    const MAX_CALLS: usize = 200;
    fn flush(burst: &mut Option<Burst>, items: &mut Vec<Value>, returns: &BTreeMap<&str, Value>) {
        let Some(b) = burst.take() else { return };
        let total = |m: &BTreeMap<String, usize>| m.values().sum::<usize>();
        let mix = if total(&b.starts) >= total(&b.returns) {
            &b.starts
        } else {
            &b.returns
        };
        let calls: Vec<Value> = if b.start_calls.is_empty() {
            b.return_calls.clone()
        } else {
            b.start_calls
                .iter()
                .map(|start| {
                    let mut call = start.clone();
                    match start["id"].as_str().and_then(|id| returns.get(id)) {
                        Some(done) => {
                            for key in ["status", "duration_ms", "exit_code"] {
                                call[key] = done[key].clone();
                            }
                            for key in ["label", "command"] {
                                if call[key].is_null() {
                                    call[key] = done[key].clone();
                                }
                            }
                        }
                        None => call["status"] = json!("no return"),
                    }
                    call
                })
                .collect()
        };
        let mut tools: Vec<(&String, &usize)> = mix.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        items.push(json!({
            "category": "burst",
            "calls": total(mix),
            "tools": tools.iter().map(|(name, count)| json!({"name": name, "count": count})).collect::<Vec<_>>(),
            "first_at": b.first_at,
            "last_at": b.last_at,
            "duration": elapsed(&b.first_at, &b.last_at),
            "call_list": calls,
        }));
    }
    // Each started call takes its return's result and timing when the IDs pair,
    // even when the return lands after a kept event or is itself a failure row.
    let returns: BTreeMap<&str, Value> = events
        .iter()
        .filter(|e| e["category"] == "tool" && e["event_type"] != "workflow.tool_started")
        .filter_map(|e| {
            let p = &e["payload"];
            p["tool_call_id"].as_str().map(|id| {
                (id, json!({"status": e["result_status"], "duration_ms": p["duration_ms"], "exit_code": p["exit_code"], "label": p["tool_label"], "command": p["tool_command"]}))
            })
        })
        .collect();
    let mut items = Vec::new();
    let mut burst: Option<Burst> = None;
    let mut last_task = Value::Null;
    for event in events {
        if event["signal"] == true {
            flush(&mut burst, &mut items, &returns);
            let mut item = event.clone();
            // Name the task only where the story moves to another one.
            let task = &event["payload"]["task_id"];
            item["task_changed"] = json!(!task.is_null() && *task != last_task);
            if !task.is_null() {
                last_task = task.clone();
            }
            items.push(item);
        } else if event["category"] == "tool" {
            let kind = event["event_type"].as_str().unwrap_or("");
            let at = event["payload"]["occurred_at"].as_str().unwrap_or("");
            let name = event["payload"]["tool_name"]
                .as_str()
                .unwrap_or("Tool")
                .to_owned();
            let b = burst.get_or_insert_with(|| Burst {
                first_at: at.to_owned(),
                ..Burst::default()
            });
            b.last_at = at.to_owned();
            let (side, calls) = if kind == "workflow.tool_started" {
                (&mut b.starts, &mut b.start_calls)
            } else {
                (&mut b.returns, &mut b.return_calls)
            };
            if calls.len() < MAX_CALLS {
                let p = &event["payload"];
                calls.push(json!({
                    "tool": name,
                    "id": p["tool_call_id"],
                    "label": p["tool_label"],
                    "command": p["tool_command"],
                    "at": at,
                    "status": event["result_status"],
                    "duration_ms": p["duration_ms"],
                    "exit_code": p["exit_code"],
                }));
            }
            *side.entry(name).or_default() += 1;
        }
    }
    flush(&mut burst, &mut items, &returns);
    items
}

/// Newest first, chapter by chapter: the latest prompt's chapter comes first,
/// each prompt stays the header above its own entries, and the entries under
/// it run newest first. Entries recorded before the first prompt form the last
/// group. Items are signal rows from `signal_items`, in source order.
pub fn newest_first(items: Vec<Value>) -> Vec<Value> {
    let mut chapters: Vec<Vec<Value>> = vec![Vec::new()];
    for item in items {
        if item["category"] == "prompt" {
            chapters.push(Vec::new());
        }
        chapters.last_mut().unwrap().push(item);
    }
    chapters
        .into_iter()
        .rev()
        .flat_map(|mut chapter| {
            let header = (chapter.first().is_some_and(|i| i["category"] == "prompt"))
                .then(|| chapter.remove(0));
            header.into_iter().chain(chapter.into_iter().rev())
        })
        .collect()
}

/// The verdict strip: latest explicit outcome, latest result of each check,
/// todo progress and activity, plus the attention flags the directory filters on.
fn verdict(session: &Value) -> Value {
    let events = session["events"].as_array().unwrap();
    let mut outcome = json!({"status": "", "text": ""});
    let mut checks = serde_json::Map::new();
    let mut decisions = 0;
    for e in events {
        let p = &e["payload"];
        match e["event_type"].as_str().unwrap_or("") {
            "workflow.task_completed" | "workflow.task_blocked" => {
                outcome = json!({
                    "status": if e["event_type"] == "workflow.task_completed" { "completed" } else { "blocked" },
                    "text": if text(p, "outcome").is_empty() { text(p, "description") } else { text(p, "outcome") },
                    "task": e["task_name"],
                    "at": p["occurred_at"],
                });
            }
            "workflow.verification" | "workflow.review" => {
                let key = if e["event_type"] == "workflow.review" {
                    "review"
                } else {
                    "verify"
                };
                let entry = checks
                    .entry(key)
                    .or_insert_with(|| json!({"passed": 0, "total": 0}));
                entry["total"] = json!(entry["total"].as_u64().unwrap_or(0) + 1);
                if e["result_status"] == "passed" {
                    entry["passed"] = json!(entry["passed"].as_u64().unwrap_or(0) + 1);
                }
                entry["last"] = e["result_status"].clone();
                entry["last_at"] = p["occurred_at"].clone();
            }
            "workflow.decision" => decisions += 1,
            _ => {}
        }
    }
    let (todos_done, todos_total) =
        session["task_list"]
            .as_array()
            .into_iter()
            .flatten()
            .fold((0, 0), |(done, total), t| {
                (
                    done + t["todos_done"].as_u64().unwrap_or(0),
                    total + t["todos_total"].as_u64().unwrap_or(0),
                )
            });
    let a = &session["analysis"];
    let n = |key: &str| a[key].as_u64().unwrap_or(0);
    let tool_calls =
        n("succeeded") + n("failed") + n("returned") + n("process_running") + n("unobserved");
    let last = |key: &str| checks.get(key).map(|c| c["last"].clone());
    let mut attention = Vec::new();
    if outcome["status"] == "blocked" {
        attention.push("task blocked");
    }
    if [last("verify"), last("review")]
        .iter()
        .any(|l| l.as_ref().is_some_and(|l| l == "failed"))
    {
        attention.push("check failed");
    }
    let unfinished = session["status"] == "ended"
        && outcome["status"] != "completed"
        && todos_done < todos_total;
    let verified = last("verify").is_some_and(|l| l == "passed")
        && last("review").is_some_and(|l| l == "passed");
    json!({
        "outcome": outcome,
        "checks": checks,
        "todos_done": todos_done,
        "todos_total": todos_total,
        "tool_calls": tool_calls,
        "tool_failures": n("failed"),
        "decisions": decisions,
        "attention": attention,
        "unfinished": unfinished,
        "verified": verified,
        "duration": elapsed(session["first_at"].as_str().unwrap_or(""), session["last_at"].as_str().unwrap_or("")),
    })
}

/// `pr-2818-rebase` reads as `PR 2818 rebase`; ordinary titles are unchanged.
fn humanize(label: &str) -> String {
    if label.contains(char::is_whitespace) || !label.contains(['-', '_']) {
        return label.to_owned();
    }
    let words: Vec<String> = label
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, word)| match word {
            "pr" | "ui" | "api" | "mfa" => word.to_uppercase(),
            _ if i == 0 => {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
            _ => word.to_owned(),
        })
        .collect();
    words.join(" ")
}

/// The project a working directory belongs to: its last path component, and
/// whether it sits inside a worktree folder (`.../worktrees/...`, `x.worktrees/...`).
fn project(cwd: &str) -> (String, bool) {
    if !cwd.starts_with('/') {
        return ("Unknown".into(), false);
    }
    let parts: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    let worktree = parts
        .iter()
        .any(|p| *p == "worktrees" || p.ends_with(".worktrees"));
    (parts.last().unwrap_or(&"/").to_string(), worktree)
}

fn annotate_timeline(session: &mut Value) {
    let tasks = session["tasks"].clone();
    // A plan prompt is only worth showing while it is still the task's latest plan event.
    let mut last_plan = BTreeMap::<String, usize>::new();
    for (index, e) in session["events"].as_array().unwrap().iter().enumerate() {
        if e["event_type"]
            .as_str()
            .is_some_and(|k| k.starts_with("workflow.plan_"))
        {
            if let Some(task) = e["payload"]["task_id"].as_str() {
                last_plan.insert(task.to_owned(), index);
            }
        }
    }
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
    for (index, event) in session["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let p = &event["payload"];
        let kind = event["event_type"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("workflow.");
        let (title, category, status, explanation) = match kind {
            "todo_created" | "todo_updated" | "todo_removed" => ("Todo changed", "todo", p["status"].as_str().unwrap_or("unknown"), "The plan item changed. Its criterion, reason, prior state and evidence preserve the revision history."),
            "plan_required" => ("Plan confirmation required", "plan", "required", "A new prompt requires confirmation or revision before execution can continue."),
            "plan_registered" | "plan_revised" | "plan_confirmed" => ("Todo plan registered or revised", "plan", "confirmed", "The agent supplied or confirmed the complete current todo plan and the reason for this revision."),
            "plan_exempted" => ("No execution needed", "plan", "question", "An explicit, audited exemption allows an answer without execution tools."),
            "tool_started" => ("Tool started", "tool", "started", "The runtime dispatched this tool. A later return records the result when available."),
            "tool_completed" => match p["outcome"].as_str() {
                _ if p["outcome"] == "failed" || p["exit_code"].as_i64().is_some_and(|code| code != 0) => ("Tool failed", "tool", "failed", "The runtime reported an error or a nonzero exit code. Review the task's later checks and outcome for recovery."),
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
            "session_updated" => ("Session metadata updated", "session", "recorded", "The agent, model, title, Herdr tab or working directory metadata changed or was recovered."),
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
        let unresolved_plan = p["task_id"]
            .as_str()
            .is_some_and(|task| last_plan.get(task) == Some(&index));
        let (signal, headline, detail, tone) =
            signal_view(kind, p, &status, &task_name, unresolved_plan);
        event["signal"] = json!(signal);
        event["headline"] = json!(headline);
        event["detail"] = json!(detail);
        event["tone"] = json!(tone);
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

/// One line of a prompt for its collapsed list row.
fn preview(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line,
    }
}

fn by_sequence(map: &Value) -> Vec<Value> {
    let mut items: Vec<Value> = map
        .as_object()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    items.sort_by_key(|item| item["sequence"].as_u64());
    items
}

/// Tasks in the order they were first seen, each carrying its todos in plan order
/// and a count of completed, still-planned todos for the compact list row.
fn ordered_tasks(tasks: &Value) -> Vec<Value> {
    by_sequence(tasks)
        .into_iter()
        .map(|mut task| {
            let todos = by_sequence(&task["todos"]);
            let planned = todos.iter().filter(|t| t["status"] != "removed");
            let (done, total) = planned.fold((0, 0), |(done, total), t| {
                (done + usize::from(t["status"] == "completed"), total + 1)
            });
            task["todo_list"] = json!(todos);
            task["todos_done"] = json!(done);
            task["todos_total"] = json!(total);
            task
        })
        .collect()
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
        for key in ["session_name", "herdr_tab", "agent", "model", "cwd"] {
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
            // JSON objects are keyed maps, so first-seen order is kept explicitly.
            let sequence = tasks.len() + 1;
            let task = tasks.entry(task_id.to_owned()).or_insert_with(|| json!({"task_id": task_id, "sequence": sequence, "first_at": p["occurred_at"], "name": "Session work", "description": "", "status": "observed", "outcome": "", "phase": "", "parent_task_id": "", "phases": {}, "todos": {}, "plan_revisions": [], "plan_status":"not_registered"}));
            task["last_at"] = p["occurred_at"].clone();
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
            let kind = row["event_type"].as_str().unwrap_or("");
            if kind.starts_with("workflow.plan_") {
                task["plan_revisions"]
                    .as_array_mut()
                    .unwrap()
                    .push(p.clone());
                task["plan_status"] = json!(if kind == "workflow.plan_required" {
                    "required"
                } else if kind == "workflow.plan_exempted" {
                    "question"
                } else {
                    "confirmed"
                });
            }
            if kind.starts_with("workflow.todo_") {
                if let Some(todo_id) = p["todo_id"].as_str() {
                    let todos = task["todos"].as_object_mut().unwrap();
                    let sequence = todos.len() + 1;
                    let todo = todos.entry(todo_id.to_owned()).or_insert_with(|| json!({"id":todo_id,"sequence":sequence,"first_at":p["occurred_at"],"description":"","criterion":"","status":"pending","evidence":"","history":[]}));
                    todo["last_at"] = p["occurred_at"].clone();
                    for key in [
                        "description",
                        "criterion",
                        "status",
                        "evidence",
                        "todo_required",
                        "revision",
                    ] {
                        if !p[key].is_null() {
                            todo[key] = p[key].clone();
                        }
                    }
                    todo["history"].as_array_mut().unwrap().push(p.clone());
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
            let prompts = session["prompts"].as_array_mut().unwrap();
            let number = prompts.len() + 1;
            prompts.push(json!({"prompt_id":id,"number":number,"preview":preview(&text),"text":text,"occurred_at":time}));
        }
        // Timeline rows link to their prompt by position, since prompts load in pages.
        let numbers: BTreeMap<String, Value> = session["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p["prompt_id"].as_str().unwrap_or("").to_owned(), p.clone()))
            .collect();
        for event in session["events"].as_array_mut().unwrap() {
            if let Some(prompt) = event["payload"]["prompt_id"]
                .as_str()
                .and_then(|id| numbers.get(id))
            {
                event["prompt_number"] = prompt["number"].clone();
                // The signal view opens a chapter with the whole prompt.
                event["prompt_preview"] = prompt["preview"].clone();
                event["prompt_text"] = prompt["text"].clone();
            }
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
        session["label"] = json!(humanize(label)
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" "));
        let (name, worktree) = project(session["cwd"].as_str().unwrap_or(""));
        session["project"] = json!(name);
        session["worktree"] = json!(worktree);
    }
    for session in sessions.values_mut() {
        annotate_timeline(session);
        session["task_list"] = json!(ordered_tasks(&session["tasks"]));
        session["verdict"] = verdict(session);
        // Nothing to review: no prompt, no named task, no plan, check or decision.
        let substantive = session["events"].as_array().unwrap().iter().any(|e| {
            matches!(
                e["category"].as_str(),
                Some("prompt" | "todo" | "plan" | "check" | "decision")
            ) || (e["category"] == "task" && e["task_name"] != "Session work")
        });
        session["empty"] = json!(!substantive);
    }
    let mut values: Vec<Value> = sessions.into_values().collect();
    values.sort_by(|a, b| b["last_at"].as_str().cmp(&a["last_at"].as_str()));
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn todo_projection_retains_criteria_revision_history_and_tool_links() {
        let base = json!({"schema_version":1,"session_id":"s","task_id":"t","todo_id":"a","description":"Repair <script>","criterion":"Test passes","reason":"Initial plan","revision":1,"status":"pending","occurred_at":"01"});
        assert!(validate("workflow.todo_created", &base).is_ok());
        let mut done = base.clone();
        done["status"] = json!("completed");
        assert!(validate("workflow.todo_updated", &done).is_err());
        done["evidence"] = json!("Regression passed");
        done["previous_status"] = json!("pending");
        done["occurred_at"] = json!("03");
        done["revision"] = json!(2);
        assert!(validate("workflow.todo_updated", &done).is_ok());
        let rows = vec![
            json!({"event_type":"workflow.todo_created","payload":base}),
            json!({"event_type":"workflow.plan_registered","payload":{"session_id":"s","task_id":"t","description":"Initial","occurred_at":"02"}}),
            json!({"event_type":"workflow.todo_updated","payload":done}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","todo_id":"a","outcome":"succeeded","occurred_at":"04"}}),
        ];
        let grouped = sessions(&rows);
        let todo = &grouped[0]["tasks"]["t"]["todos"]["a"];
        assert_eq!(todo["status"], "completed");
        assert_eq!(todo["criterion"], "Test passes");
        assert_eq!(todo["history"].as_array().unwrap().len(), 2);
        assert_eq!(grouped[0]["tasks"]["t"]["plan_status"], "confirmed");
        assert_eq!(grouped[0]["events"][3]["payload"]["todo_id"], "a");
    }
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
        // A tool label is short text; blank or oversized labels are refused.
        let mut t = json!({"schema_version":1,"session_id":"s","task_id":"t","occurred_at":"2026-10-02T00:00:00Z","tool_name":"Bash","tool_label":"Run the test suite"});
        assert!(validate("workflow.tool_failed", &t).is_ok());
        t["tool_label"] = json!(" ");
        assert!(validate("workflow.tool_failed", &t).is_err());
        t["tool_label"] = json!("x".repeat(301));
        assert!(validate("workflow.tool_failed", &t).is_err());
        t["tool_label"] = json!(7);
        assert!(validate("workflow.tool_failed", &t).is_err());
        // The command is text up to the 2000-byte field limit.
        let mut c = json!({"schema_version":1,"session_id":"s","task_id":"t","occurred_at":"2026-10-02T00:00:00Z","tool_name":"Bash","tool_command":"git config core.hooksPath"});
        assert!(validate("workflow.tool_failed", &c).is_ok());
        c["tool_command"] = json!(" ");
        assert!(validate("workflow.tool_failed", &c).is_err());
        c["tool_command"] = json!("x".repeat(2001));
        assert!(validate("workflow.tool_failed", &c).is_err());
    }
    #[test]
    fn failed_tools_name_their_command_and_bursts_list_each_call() {
        // A started call takes its paired return's result; an unpaired start says so.
        let paired = vec![
            json!({"event_type":"workflow.tool_started","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"a","tool_label":"List files","outcome":"running","occurred_at":"2026-10-02T00:00:01Z"}}),
            json!({"event_type":"workflow.tool_started","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"b","outcome":"running","occurred_at":"2026-10-02T00:00:02Z"}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"a","outcome":"succeeded","exit_code":0,"duration_ms":9,"occurred_at":"2026-10-02T00:00:03Z"}}),
        ];
        let session = sessions(&paired).remove(0);
        let items = signal_items(session["events"].as_array().unwrap());
        let calls = items[0]["call_list"].as_array().unwrap();
        assert_eq!(items[0]["calls"], 2);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["status"], "succeeded");
        assert_eq!(calls[0]["duration_ms"], 9);
        assert_eq!(calls[0]["label"], "List files");
        assert_eq!(calls[1]["status"], "no return");
        // A return after a kept event or a failure row still pairs with its start.
        let split = vec![
            json!({"event_type":"workflow.tool_started","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"a","outcome":"running","occurred_at":"2026-10-02T00:00:01Z"}}),
            json!({"event_type":"workflow.tool_started","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"b","outcome":"running","occurred_at":"2026-10-02T00:00:02Z"}}),
            json!({"event_type":"workflow.decision","payload":{"session_id":"s","task_id":"t","description":"Pick","options":["x","y"],"selected":"x","occurred_at":"2026-10-02T00:00:03Z"}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"a","outcome":"succeeded","exit_code":0,"occurred_at":"2026-10-02T00:00:04Z"}}),
            json!({"event_type":"workflow.tool_failed","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_call_id":"b","outcome":"failed","exit_code":1,"occurred_at":"2026-10-02T00:00:05Z"}}),
        ];
        let session = sessions(&split).remove(0);
        let items = signal_items(session["events"].as_array().unwrap());
        assert_eq!(items[0]["call_list"][0]["status"], "succeeded");
        assert_eq!(items[0]["call_list"][1]["status"], "failed");
        assert_eq!(items[0]["call_list"][1]["exit_code"], 1);
        let rows = vec![
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","tool_name":"Read","tool_label":"ui.rs","outcome":"succeeded","exit_code":0,"duration_ms":4,"occurred_at":"2026-10-02T00:00:01Z"}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","tool_name":"mcp__browser__navigate","outcome":"returned","occurred_at":"2026-10-02T00:00:02Z"}}),
            json!({"event_type":"workflow.tool_failed","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_label":"Run the test suite","outcome":"failed","exit_code":2,"occurred_at":"2026-10-02T00:00:03Z"}}),
        ];
        let session = sessions(&rows).remove(0);
        let items = signal_items(session["events"].as_array().unwrap());
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["category"], "burst");
        let calls = items[0]["call_list"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["tool"], "Read");
        assert_eq!(calls[0]["label"], "ui.rs");
        assert_eq!(calls[0]["duration_ms"], 4);
        assert_eq!(calls[1]["label"], Value::Null);
        assert_eq!(items[1]["headline"], "Bash failed (exit 2)");
        assert_eq!(items[1]["detail"], "Run the test suite");
        // With the command recorded, the failure row names it instead.
        let mut rows = rows;
        rows[2]["payload"]["tool_command"] = json!("make test");
        let session = sessions(&rows).remove(0);
        let items = signal_items(session["events"].as_array().unwrap());
        assert_eq!(items[1]["detail"], "make test");
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
            json!({"event_type":"workflow.session_updated","payload":{"session_id":"s","herdr_tab":"Audit redesign","occurred_at":"04"}}),
        ];
        let grouped = sessions(&rows);
        assert_eq!(grouped[0]["herdr_tab"], "Audit redesign");
        assert_eq!(grouped[0]["label"], "Repair the audit dashboard session");
        assert_eq!(grouped[0]["agent"], "Codex");
        assert_eq!(grouped[0]["model"], "model-v2");
        assert_eq!(grouped[0]["cwd"], "/example/Working Tree");
        assert_eq!(grouped[0]["status"], "ended");
        assert_eq!(grouped[0]["last_at"], "02");
        assert_eq!(grouped[0]["first_at"], "01");
        assert_eq!(grouped[0]["metadata_history"].as_array().unwrap().len(), 3);
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
    fn tasks_and_todos_keep_first_seen_order_with_their_own_timestamps() {
        // IDs sort the other way round, so map order alone would reverse them.
        let todo = |task: &str, id: &str, status: &str, at: &str| json!({"event_type":"workflow.todo_updated","payload":{"session_id":"s","task_id":task,"todo_id":id,"description":id,"status":status,"occurred_at":at}});
        let rows = vec![
            json!({"event_type":"workflow.task_started","payload":{"session_id":"s","task_id":"z","name":"First","occurred_at":"2026-10-02T01:00:00Z"}}),
            todo("z", "y", "pending", "2026-10-02T01:01:00Z"),
            todo("z", "b", "completed", "2026-10-02T01:02:00Z"),
            todo("z", "a", "removed", "2026-10-02T01:03:00Z"),
            json!({"event_type":"workflow.task_started","payload":{"session_id":"s","task_id":"a","name":"Second","occurred_at":"2026-10-02T02:00:00Z"}}),
            todo("z", "y", "completed", "2026-10-02T03:00:00Z"),
            json!({"event_type":"workflow.prompt_recorded","payload":{"session_id":"s","task_id":"a","prompt_id":"p","part_index":0,"part_count":1,"text":format!("line one\n\n{}", "word ".repeat(40)),"occurred_at":"2026-10-02T04:00:00Z"}}),
        ];
        let session = &sessions(&rows)[0];
        let tasks = session["task_list"].as_array().unwrap();
        assert_eq!(tasks[0]["name"], "First");
        assert_eq!(tasks[0]["sequence"], 1);
        assert_eq!(tasks[0]["first_at"], "2026-10-02T01:00:00Z");
        assert_eq!(tasks[0]["last_at"], "2026-10-02T03:00:00Z");
        assert_eq!(tasks[1]["name"], "Second");
        assert_eq!(tasks[1]["last_at"], "2026-10-02T04:00:00Z");
        let todos: Vec<&str> = tasks[0]["todo_list"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(todos, ["y", "b", "a"]);
        assert_eq!(tasks[0]["todo_list"][0]["first_at"], "2026-10-02T01:01:00Z");
        assert_eq!(tasks[0]["todo_list"][0]["last_at"], "2026-10-02T03:00:00Z");
        // Removed todos are not part of the plan's progress.
        assert_eq!(
            (
                tasks[0]["todos_done"].as_u64(),
                tasks[0]["todos_total"].as_u64()
            ),
            (Some(2), Some(2))
        );
        let preview = session["prompts"][0]["preview"].as_str().unwrap();
        assert!(preview.starts_with("line one word word") && preview.ends_with('…'));
        assert_eq!(preview.chars().count(), 121);
        assert_eq!(session["events"][6]["prompt_number"], 1);
    }

    fn row(kind: &str, at: u32, extra: Value) -> Value {
        let mut payload = json!({"session_id":"s","task_id":"t","occurred_at":format!("2026-10-02T00:{at:02}:00Z")});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"event_type": format!("workflow.{kind}"), "payload": payload})
    }

    #[test]
    fn signal_view_folds_tool_runs_keeps_failures_and_drops_bookkeeping() {
        let tool = |kind: &str, at: u32, call: &str, name: &str| {
            row(kind, at, json!({"tool_call_id": call, "tool_name": name}))
        };
        let rows = vec![
            row(
                "task_started",
                0,
                json!({"name": "Fix parser", "description": "Repair imports"}),
            ),
            row("plan_required", 0, json!({})),
            row(
                "plan_registered",
                1,
                json!({"description": "Initial", "todo_count": 2}),
            ),
            row(
                "todo_created",
                1,
                json!({"todo_id": "a", "description": "Patch", "status": "pending"}),
            ),
            row(
                "todo_updated",
                1,
                json!({"todo_id": "a", "description": "Patch", "status": "in_progress"}),
            ),
            row("session_updated", 1, json!({"model": "m"})),
            tool("tool_started", 2, "1", "Bash"),
            tool("tool_completed", 3, "1", "Bash"),
            tool("tool_started", 4, "2", "Edit"),
            tool("tool_completed", 5, "2", "Edit"),
            tool("tool_started", 6, "3", "Bash"),
            tool("tool_completed", 7, "3", "Bash"),
            row(
                "tool_completed",
                8,
                json!({"tool_call_id": "4", "tool_name": "Bash", "exit_code": 101}),
            ),
            // Returns without starts, as older sessions recorded them.
            tool("tool_completed", 9, "5", "Read"),
            tool("tool_completed", 21, "6", "Read"),
            row(
                "decision",
                22,
                json!({"description": "Scope", "options": ["focused", "full"], "selected": "full"}),
            ),
            row(
                "verification",
                23,
                json!({"check": "verify", "exit_code": 0, "ran": 3, "skipped": 1}),
            ),
            row(
                "todo_updated",
                24,
                json!({"todo_id": "a", "description": "Patch", "status": "completed", "evidence": "Suite green"}),
            ),
            row(
                "phase_changed",
                25,
                json!({"phase": "build", "phase_status": "active"}),
            ),
            row(
                "phase_changed",
                26,
                json!({"phase": "build", "phase_status": "done"}),
            ),
            row("task_completed", 27, json!({"outcome": "Imports repaired"})),
        ];
        let session = &sessions(&rows)[0];
        let events = session["events"].as_array().unwrap();
        let items = signal_items(events);
        let lines: Vec<String> = items
            .iter()
            .map(|i| {
                if i["category"] == "burst" {
                    format!(
                        "burst {} {}",
                        i["calls"],
                        i["tools"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| format!("{}{}", t["name"].as_str().unwrap(), t["count"]))
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                } else {
                    i["headline"].as_str().unwrap().to_owned()
                }
            })
            .collect();
        assert_eq!(
            lines,
            [
                "Task started: Fix parser",
                "Plan registered · 2 todos",
                "burst 3 Bash2,Edit1",
                "Bash failed (exit 101)",
                "burst 2 Read2",
                "Decision: full",
                "verify passed · ran 3 · skipped 1",
                "Todo done: Patch",
                "Phase: build",
                "Task completed: Fix parser",
            ]
        );
        // The resolved plan prompt, metadata refresh and todo bookkeeping are gone.
        assert!(!lines.iter().any(|l| l.contains("Waiting on a todo plan")));
        assert_eq!(items[4]["duration"], "12m");
        assert_eq!(items[2]["first_at"], "2026-10-02T00:02:00Z");
        assert_eq!(items[5]["detail"], "Scope · over focused");
        assert_eq!(items[7]["detail"], "Suite green");
        // A nonzero exit on a return counts as a failed tool, not an unknown return.
        assert_eq!(session["analysis"]["failed"], 1);

        // A plan prompt that was never answered stays visible as a warning.
        let waiting = sessions(&[row("plan_required", 0, json!({}))]);
        let items = signal_items(waiting[0]["events"].as_array().unwrap());
        assert_eq!(items[0]["headline"], "Waiting on a todo plan");
        assert_eq!(items[0]["tone"], "warn");
    }

    #[test]
    fn newest_first_reverses_chapters_and_keeps_each_prompt_above_its_entries() {
        let item = |category: &str, name: &str| json!({"category": category, "name": name});
        let items = vec![
            item("task", "setup"),
            item("prompt", "P1"),
            item("check", "a"),
            item("burst", "b"),
            item("prompt", "P2"),
            item("decision", "c"),
            item("check", "d"),
        ];
        let names: Vec<String> = newest_first(items)
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["P2", "d", "c", "P1", "b", "a", "setup"]);
        assert!(newest_first(Vec::new()).is_empty());
    }

    #[test]
    fn verdict_reports_outcome_checks_todos_and_attention() {
        let todo = |id: &str, status: &str, at: u32| {
            row(
                "todo_updated",
                at,
                json!({"todo_id": id, "description": id, "status": status, "evidence": "ok"}),
            )
        };
        let mut rows = vec![
            row("session_started", 0, json!({})),
            row("task_started", 0, json!({"name": "Ship"})),
            todo("a", "completed", 1),
            todo("b", "pending", 1),
            row(
                "verification",
                2,
                json!({"check": "verify", "exit_code": 1}),
            ),
            row(
                "verification",
                3,
                json!({"check": "verify", "exit_code": 0}),
            ),
            row("review", 4, json!({"check": "review", "exit_code": 0})),
            row(
                "decision",
                5,
                json!({"options": ["a", "b"], "selected": "a"}),
            ),
            row("tool_failed", 6, json!({"tool_name": "Bash"})),
        ];
        let v = &sessions(&rows)[0]["verdict"];
        assert_eq!(
            v["checks"]["verify"],
            json!({"passed": 1, "total": 2, "last": "passed", "last_at": "2026-10-02T00:03:00Z"})
        );
        assert_eq!(v["checks"]["review"]["last"], "passed");
        assert_eq!(
            (v["todos_done"].as_u64(), v["todos_total"].as_u64()),
            (Some(1), Some(2))
        );
        assert_eq!(v["verified"], true);
        assert_eq!(v["attention"], json!([]));
        assert_eq!(
            v["unfinished"], false,
            "a running session is not unfinished"
        );
        assert_eq!(
            (
                v["decisions"].as_u64(),
                v["tool_failures"].as_u64(),
                v["tool_calls"].as_u64()
            ),
            (Some(1), Some(1), Some(1))
        );
        assert_eq!(v["outcome"]["status"], "");
        assert_eq!(v["duration"], "6m");

        rows.push(row("session_ended", 7, json!({})));
        assert_eq!(sessions(&rows)[0]["verdict"]["unfinished"], true);
        rows.push(row("review", 8, json!({"check": "review", "exit_code": 2})));
        rows.push(row(
            "task_blocked",
            9,
            json!({"outcome": "Waiting on credentials"}),
        ));
        let v = &sessions(&rows)[0]["verdict"];
        assert_eq!(v["attention"], json!(["task blocked", "check failed"]));
        assert_eq!(v["verified"], false);
        assert_eq!(v["outcome"]["text"], "Waiting on credentials");
        assert_eq!(v["outcome"]["task"], "Ship");
    }

    #[test]
    fn labels_projects_and_empty_sessions_read_cleanly() {
        assert_eq!(humanize("pr-2818-rebase"), "PR 2818 rebase");
        assert_eq!(
            humanize("healthcare_evaluation-planning"),
            "Healthcare evaluation planning"
        );
        assert_eq!(humanize("Add Workflow audit"), "Add Workflow audit");
        assert_eq!(humanize("v2"), "v2");
        assert_eq!(
            project("/Users/me/.codex/worktrees/e294/arc"),
            ("arc".into(), true)
        );
        assert_eq!(
            project("/Users/me/vault.worktrees/docs-site"),
            ("docs-site".into(), true)
        );
        assert_eq!(
            project("/Users/me/Code/DollarWise-Prototype"),
            ("DollarWise-Prototype".into(), false)
        );
        assert_eq!(project("Unknown"), ("Unknown".into(), false));
        assert_eq!(
            elapsed("2026-10-02T00:00:00Z", "2026-10-02T00:00:42Z"),
            "42s"
        );
        assert_eq!(
            elapsed("2026-10-02T00:00:00Z", "2026-10-02T03:05:00Z"),
            "3h 5m"
        );
        assert_eq!(
            elapsed("2026-10-02T00:00:00Z", "2026-10-05T04:00:00Z"),
            "3d 4h"
        );
        assert_eq!(elapsed("bad", "2026-10-05T04:00:00Z"), "");

        let probe = sessions(&[
            row("session_started", 0, json!({"cwd": "/Users/me/Probe"})),
            row("task_started", 0, json!({"name": "Session work"})),
            row("tool_completed", 1, json!({"tool_name": "Bash"})),
        ]);
        assert_eq!(probe[0]["empty"], true);
        assert_eq!(probe[0]["project"], "Probe");
        let named = sessions(&[row("task_started", 0, json!({"name": "pr-12-review"}))]);
        assert_eq!(named[0]["empty"], false);
        assert_eq!(named[0]["label"], "PR 12 review");
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
