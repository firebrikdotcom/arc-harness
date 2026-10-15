//! Compact orchestration facts. Lifecycle events merge by identity, never by event count.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn metric(items: &[Value], key: &str) -> Value {
    let mut values: Vec<f64> = items.iter().filter_map(|v| number(v, key)).collect();
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = if n == 0 {
        None
    } else if n % 2 == 1 {
        Some(values[n / 2])
    } else {
        Some((values[n / 2 - 1] + values[n / 2]) / 2.0)
    };
    json!({"total": if n == 0 {None} else {Some(values.iter().sum::<f64>())}, "samples": n, "population": items.len(), "median": median})
}
fn instant(v: &Value) -> Option<i64> {
    if let Some(seconds) = v.as_f64().filter(|s| s.is_finite()) {
        return Some((seconds * 1000.0) as i64);
    }
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
}
fn collapse(rows: &[&Value], prefix: &str, id_key: &str, fields: &[&str]) -> Vec<Value> {
    let mut entities = BTreeMap::<(String, String), Map<String, Value>>::new();
    for row in rows {
        let Some(kind) = row["event_type"]
            .as_str()
            .and_then(|s| s.strip_prefix(prefix))
        else {
            continue;
        };
        let p = &row["payload"];
        let Some(id) = p[id_key]
            .as_str()
            .or_else(|| {
                if id_key == "decision_id" {
                    p["id"].as_str()
                } else {
                    None
                }
            })
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let scope = p["run_id"]
            .as_str()
            .or(p["parent_session_id"].as_str())
            .unwrap_or("")
            .to_owned();
        let entity = entities.entry((scope, id.to_owned())).or_default();
        for field in fields {
            if let Some(value) = p.get(*field).filter(|v| !v.is_null()) {
                entity.insert((*field).into(), value.clone());
            }
        }
        entity.insert(id_key.into(), json!(id));
        // Event names are authoritative lifecycle evidence, including nonzero exits.
        let status = match kind {
            "planned" => Some("planned"),
            "created" => Some("created"),
            "started" => Some("running"),
            "completed" => p["status"]
                .as_str()
                .filter(|s| {
                    matches!(
                        *s,
                        "completed" | "failed" | "cancelled" | "disabled" | "reserved" | "blocked"
                    )
                })
                .or(Some("completed")),
            "failed" => Some("failed"),
            "cancelled" => Some("cancelled"),
            _ => None,
        };
        if let Some(status) = status {
            entity.insert("status".into(), json!(status));
        }
    }
    entities
        .into_values()
        .map(|mut entity| {
            for (key, start, end) in [
                ("duration_ms", "started_at", "finished_at"),
                ("queue_ms", "queued_at", "started_at"),
            ] {
                if !entity.contains_key(key) {
                    if let Some(delta) = entity
                        .get(start)
                        .and_then(instant)
                        .zip(entity.get(end).and_then(instant))
                        .and_then(|(a, b)| b.checked_sub(a))
                        .filter(|d| *d >= 0)
                    {
                        entity.insert(key.into(), json!(delta));
                    }
                }
            }
            Value::Object(entity)
        })
        .collect()
}

pub fn summarize(rows: &[Value]) -> Value {
    let mut ordered: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            r["event_type"]
                .as_str()
                .is_some_and(|s| s.starts_with("orchestration."))
        })
        .collect();
    // Stable source order breaks equal timestamp ties.
    ordered.sort_by_key(|r| r["recorded_at_us"].as_i64().unwrap_or(0));
    let fields = [
        "run_id",
        "parent_session_id",
        "role",
        "provider",
        "model",
        "effort",
        "status",
        "queued_at",
        "started_at",
        "finished_at",
        "duration_ms",
        "queue_ms",
        "context_bytes",
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "total_tokens",
        "cost_usd",
        "exit_code",
        "outcome",
        "quality_passed",
    ];
    let workers = collapse(&ordered, "orchestration.worker_", "worker_id", &fields);
    let runs = collapse(&ordered, "orchestration.run_", "run_id", &fields);
    let mut decisions = collapse(
        &ordered,
        "orchestration.decision_",
        "decision_id",
        &[
            "parent_session_id",
            "family",
            "mode",
            "status",
            "action",
            "baseline",
            "cache_hit",
            "duration_ms",
            "latency_ms",
            "outcome",
            "usage",
            "action_duration_ms",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cost_usd",
        ],
    );
    for decision in &mut decisions {
        let usage = decision
            .as_object_mut()
            .unwrap()
            .remove("usage")
            .unwrap_or(Value::Null);
        for key in ["input_tokens", "output_tokens", "total_tokens", "cost_usd"] {
            // Flat producer metrics take precedence; nested usage remains compatible.
            if let Some(value) = number(decision, key).or_else(|| number(&usage, key)) {
                decision[key] = json!(value);
            } else {
                decision.as_object_mut().unwrap().remove(key);
            }
        }
    }
    // JSON values lack Ord; dimension strings keep unknown distinct from empty strings.
    let mut keyed = BTreeMap::<Vec<Option<String>>, Vec<Value>>::new();
    for worker in &workers {
        keyed
            .entry(
                ["role", "provider", "model", "effort"]
                    .iter()
                    .map(|k| worker[*k].as_str().map(str::to_owned))
                    .collect(),
            )
            .or_default()
            .push(worker.clone());
    }
    let grouped: Vec<Value> = keyed.into_iter().map(|(key, items)| json!({"role":key[0],"provider":key[1],"model":key[2],"effort":key[3],"count":items.len(),"statuses":statuses(&items),"duration_ms":metric(&items,"duration_ms"),"queue_ms":metric(&items,"queue_ms"),"context_bytes":metric(&items,"context_bytes"),"tokens":metric(&items,"total_tokens"),"cached_input_tokens":metric(&items,"cached_input_tokens"),"cost_usd":metric(&items,"cost_usd")})).collect();
    let mut intervals: Vec<(i64, i32)> = workers
        .iter()
        .filter_map(|w| instant(&w["started_at"]).zip(instant(&w["finished_at"])))
        .filter(|(a, b)| b > a)
        .flat_map(|(a, b)| [(a, 1), (b, -1)])
        .collect();
    intervals.sort();
    let (mut active, mut peak) = (0, 0);
    for (_, delta) in &intervals {
        active += delta;
        peak = peak.max(active);
    }
    json!({"events":ordered.len(),"runs":runs,"workers":workers,"worker_groups":grouped,"worker_count":workers.len(),"statuses":statuses(&workers),"decisions":decisions,"decision_count":decisions.len(),"decision_tokens":metric(&decisions,"total_tokens"),"decision_cost_usd":metric(&decisions,"cost_usd"),"decision_latency_ms":metric(&decisions,"latency_ms"),"decision_duration_ms":metric(&decisions,"duration_ms"),"cache_hits":decisions.iter().filter(|d| d["cache_hit"] == true).count(),"cache_samples":decisions.iter().filter(|d| d["cache_hit"].is_boolean()).count(),"peak_concurrency":if intervals.is_empty(){None}else{Some(peak)},"concurrency_samples":intervals.len()/2})
}
fn statuses(items: &[Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in items {
        *counts
            .entry(item["status"].as_str().unwrap_or("unknown").to_owned())
            .or_default() += 1;
    }
    counts
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extreme_timestamp_intervals_remain_unknown_without_panicking() {
        let rows = vec![
            json!({"event_type":"orchestration.worker_completed","recorded_at_us":1,
            "payload":{"run_id":"r","worker_id":"w","started_at":-1e300,"finished_at":1e300}}),
        ];
        let result = summarize(&rows);
        assert!(result["workers"][0]["duration_ms"].is_null());
    }

    #[test]
    fn producer_shaped_flat_decision_usage_preserves_unknown_and_zero() {
        let rows = vec![
            json!({"event_type":"orchestration.decision_evaluated","recorded_at_us":1,"payload":{"decision_id":"d","parent_session_id":"s","family":"tool_selection","status":"evaluated","input_tokens":3,"output_tokens":0,"total_tokens":3,"cost_usd":null}}),
            json!({"event_type":"orchestration.decision_outcome","recorded_at_us":2,"payload":{"decision_id":"d","parent_session_id":"s","outcome":"unknown","action_duration_ms":420,"duration_ms":13,"input_tokens":3,"output_tokens":0,"total_tokens":3,"exit_code":0}}),
            json!({"event_type":"orchestration.decision_evaluated","recorded_at_us":3,"payload":{"id":"cached","parent_session_id":"s","cache_hit":true,"latency_ms":0,"input_tokens":null,"output_tokens":null,"total_tokens":null}}),
        ];
        let result = summarize(&rows);
        assert_eq!(result["decision_count"], 2);
        assert_eq!(result["decision_tokens"]["total"], 3.0);
        assert_eq!(result["decision_tokens"]["samples"], 1);
        assert_eq!(result["decision_tokens"]["population"], 2);
        assert_eq!(result["decision_cost_usd"]["total"], Value::Null);
        let decision = result["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["decision_id"] == "d")
            .unwrap();
        assert_eq!(decision["outcome"], "unknown");
        assert_eq!(decision["output_tokens"], 0.0);
        assert_eq!(decision["action_duration_ms"], 420);
        assert_eq!(result["decision_duration_ms"]["total"], 13.0);
        assert!(decision.get("usage").is_none());
    }

    #[test]
    fn timing_concurrency_and_latest_decision_outcomes_have_measured_coverage() {
        let worker = |id: &str, start: &str, end: &str| json!({"event_type":"orchestration.worker_completed","payload":{"run_id":"r","worker_id":id,"started_at":start,"finished_at":end,"queued_at":"2026-10-14T00:00:00Z","total_tokens":0,"cached_input_tokens":750,"cost_usd":0}});
        let rows = vec![
            worker("a", "2026-10-14T00:00:01Z", "2026-10-14T00:00:03Z"),
            worker("b", "2026-10-14T00:00:02Z", "2026-10-14T00:00:04Z"),
            json!({"event_type":"orchestration.decision_evaluated","recorded_at_us":1,"payload":{"decision_id":"d","parent_session_id":"s","cache_hit":false,"latency_ms":0,"usage":{"total_tokens":0,"raw":"omit"}}}),
            json!({"event_type":"orchestration.decision_outcome","recorded_at_us":2,"payload":{"decision_id":"d","parent_session_id":"s","outcome":"correct"}}),
        ];
        let result = summarize(&rows);
        assert_eq!(instant(&json!(1.25)), Some(1250));
        assert_eq!(result["peak_concurrency"], 2);
        assert_eq!(
            result["worker_groups"][0]["cached_input_tokens"]["total"],
            1500.0
        );
        assert_eq!(
            result["worker_groups"][0]["cached_input_tokens"]["samples"],
            2
        );
        assert_eq!(result["workers"][0]["queue_ms"], 1000);
        assert_eq!(result["workers"][0]["duration_ms"], 2000);
        assert_eq!(result["decision_count"], 1);
        assert_eq!(result["decisions"][0]["outcome"], "correct");
        assert_eq!(result["decision_tokens"]["samples"], 1);
        assert_eq!(result["decision_tokens"]["total"], 0.0);
        assert_eq!(result["decision_cost_usd"]["total"], Value::Null);
        assert!(!result.to_string().contains("omit"));
    }

    #[test]
    fn lifecycle_counts_once_and_preserves_unknowns_and_failures() {
        let row = |kind: &str, at: i64, p: Value| json!({"event_type":format!("orchestration.{kind}"),"recorded_at_us":at,"payload":p});
        let rows = vec![
            row(
                "worker_failed",
                3,
                json!({"worker_id":"w","run_id":"r","exit_code":2}),
            ),
            row(
                "worker_planned",
                1,
                json!({"worker_id":"w","run_id":"r","role":"review"}),
            ),
            row(
                "worker_started",
                2,
                json!({"worker_id":"w","run_id":"r","context_bytes":0}),
            ),
            row("worker_cancelled", 4, json!({"worker_id":"x","run_id":"r"})),
        ];
        let s = summarize(&rows);
        assert_eq!(s["worker_count"], 2);
        assert_eq!(s["statuses"]["failed"], 1);
        assert_eq!(s["statuses"]["cancelled"], 1);
        let g = &s["worker_groups"][1];
        assert_eq!(g["cost_usd"]["total"], Value::Null);
        assert_eq!(g["context_bytes"]["total"], 0.0);
        assert_eq!(summarize(&[])["peak_concurrency"], Value::Null);
    }
}
