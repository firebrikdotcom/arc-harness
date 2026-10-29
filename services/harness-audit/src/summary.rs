//! Grouped summaries over projected audit rows.
//!
//! Pure functions over `audit_events_view` rows so the projection can be tested
//! without a running service. Only compact facts already present in event
//! payloads are read; nothing here reaches outside the local database.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

// The task-routing regression suite also historically used this synthetic pin.
const FIXTURE_MODELS: &[&str] = &["fixture", "jev-9.9.9"];
const TOP_PAIRS: usize = 5;

type GroupKey = Vec<Option<String>>;

pub fn origin_label(origin: Option<&str>) -> &'static str {
    match origin {
        Some("automatic_tool") => "Automatic tool check",
        Some("explicit_decision") => "Explicit decision",
        Some("direct_advice") => "Direct advice",
        Some("worker_selection") => "Worker selection",
        Some("session_routing") => "Session routing",
        Some("task_routing") => "Task routing",
        Some("failure_followup") => "Failure follow-up",
        Some("prompt_routing") => "Prompt effort",
        Some("search_selection") => "Search strategy",
        Some("plan_prioritization") => "Plan first step",
        Some("verify_followup") => "Verify follow-up",
        Some("review_grading") => "Review grading",
        Some("handoff_check") => "Handoff (shadow)",
        _ => "Legacy / unknown",
    }
}

#[derive(Default)]
struct Group {
    count: usize,
    comparable: usize,
    agreed: usize,
    labeled: usize,
    correct: usize,
    outcomes: BTreeMap<String, usize>,
    recommendations: BTreeMap<String, usize>,
    pairs: BTreeMap<String, usize>,
    confidence: Vec<f64>,
    latency: Vec<f64>,
    tokens: i64,
    token_samples: usize,
    token_overflowed: bool,
    token_overflowed_samples: usize,
    usage: Usage,
}

/// Each measurement has its own coverage; absent values never become measured zero.
#[derive(Default)]
struct Usage {
    input: Vec<i64>,
    output: Vec<i64>,
    costs: Vec<f64>,
    estimated: usize,
    reported: usize,
}

impl Usage {
    fn record(&mut self, p: &Map<String, Value>) {
        for (key, values) in [
            ("jev_input_tokens", &mut self.input),
            ("jev_output_tokens", &mut self.output),
        ] {
            if let Some(n) = p.get(key).and_then(Value::as_i64).filter(|n| *n >= 0) {
                values.push(n);
            }
        }
        if let Some(n) = p
            .get("jev_cost_usd")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
        {
            self.costs.push(n);
            match p.get("jev_cost_source").and_then(Value::as_str) {
                Some("list_price_estimate") => self.estimated += 1,
                Some("provider") => self.reported += 1,
                _ => {}
            }
        }
    }

    fn render(&self) -> Value {
        let tokens = |values: &[i64]| {
            json!({
                "total": if values.is_empty() { None } else { values.iter().try_fold(0_i64, |a, b| a.checked_add(*b)) },
                "samples": values.len(),
            })
        };
        let cost = self.costs.iter().sum::<f64>();
        json!({"input_tokens": tokens(&self.input), "output_tokens": tokens(&self.output),
            "cost_usd": {"total": if self.costs.is_empty() || !cost.is_finite() { None } else { Some(cost) },
                "samples": self.costs.len(), "estimated_samples": self.estimated, "provider_samples": self.reported}})
    }
}

/// Combine only call reports, never orchestration mirrors or outcome events.
pub fn usage_totals(reports: &[&Value]) -> Value {
    let groups: Vec<&Value> = reports
        .iter()
        .flat_map(|r| r["groups"].as_array().into_iter().flatten())
        .collect();
    let population: u64 = groups.iter().filter_map(|g| g["count"].as_u64()).sum();
    let mut result = json!({"calls": population});
    for key in ["tokens", "input_tokens", "output_tokens", "cost_usd"] {
        let samples: u64 = groups
            .iter()
            .filter_map(|g| g[key]["samples"].as_u64())
            .sum();
        let mut value = json!({"samples": samples, "total": null});
        if key == "cost_usd" {
            let total = groups
                .iter()
                .filter(|g| g[key]["samples"].as_u64().unwrap_or(0) > 0)
                .try_fold(0.0, |a, g| g[key]["total"].as_f64().map(|b| a + b));
            if samples > 0 {
                value["total"] = json!(total.filter(|n| n.is_finite()));
            }
            for source in ["estimated_samples", "provider_samples"] {
                value[source] = json!(groups
                    .iter()
                    .filter_map(|g| g[key][source].as_u64())
                    .sum::<u64>());
            }
        } else if samples > 0 {
            value["total"] = json!(groups
                .iter()
                .filter(|g| g[key]["samples"].as_u64().unwrap_or(0) > 0)
                .try_fold(0_i64, |a, g| g[key]["total"]
                    .as_i64()
                    .and_then(|b| a.checked_add(b))));
        }
        result[key] = value;
    }
    result
}

#[derive(Clone, Copy)]
enum Kind {
    Checkpoint,
    Route,
}

impl Kind {
    fn event_type(self) -> &'static str {
        match self {
            Kind::Checkpoint => "jev.checkpoint",
            Kind::Route => "jev.route",
        }
    }

    fn outcome_event_type(self) -> &'static str {
        match self {
            Kind::Checkpoint => "jev.checkpoint_outcome",
            Kind::Route => "jev.outcome",
        }
    }

    fn key_fields(self) -> &'static [&'static str] {
        match self {
            Kind::Checkpoint => &[
                "family",
                "question_version",
                "policy_version",
                "model_requested",
                "model_returned",
                "interaction_origin",
                "context_version",
            ],
            Kind::Route => &[
                "source",
                "routing_mode",
                "model_requested",
                "model_returned",
                "interaction_origin",
                "context_version",
            ],
        }
    }

    /// The payload field holding the recommendation to distribute and compare.
    fn recommendation_field(self) -> &'static str {
        match self {
            Kind::Checkpoint => "recommendation",
            Kind::Route => "observed_recommendation",
        }
    }
}

fn payload(row: &Value) -> Option<&Map<String, Value>> {
    row.get("payload").and_then(Value::as_object)
}

fn text(payload: &Map<String, Value>, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_string)
}

fn is_fixture(payload: &Map<String, Value>) -> bool {
    ["model_requested", "model_returned"].iter().any(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|model| FIXTURE_MODELS.contains(&model))
    })
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    })
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

/// Missing measurements remain distinct from measured, unrepresentable totals.
fn total_tokens(payload: &Map<String, Value>) -> Result<Option<i64>, ()> {
    if let Some(total) = payload
        .get("jev_total_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
    {
        return Ok(Some(total));
    }
    let Some(input) = payload
        .get("jev_input_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
    else {
        return Ok(None);
    };
    let Some(output) = payload
        .get("jev_output_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
    else {
        return Ok(None);
    };
    input.checked_add(output).map(Some).ok_or(())
}

/// Latest outcome per `call_id`; a later label replaces an earlier one.
fn latest_outcomes(rows: &[Value], kind: Kind) -> BTreeMap<String, String> {
    let mut latest = BTreeMap::<String, (i64, String)>::new();
    for row in rows {
        if row.get("event_type").and_then(Value::as_str) != Some(kind.outcome_event_type()) {
            continue;
        }
        let Some(payload) = payload(row) else {
            continue;
        };
        let (Some(call_id), Some(outcome)) = (text(payload, "call_id"), text(payload, "outcome"))
        else {
            continue;
        };
        let at = row
            .get("recorded_at_us")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        match latest.get(&call_id) {
            Some((existing, _)) if *existing > at => {}
            _ => {
                latest.insert(call_id, (at, outcome));
            }
        }
    }
    latest
        .into_iter()
        .map(|(call, (_, outcome))| (call, outcome))
        .collect()
}

fn top_pairs(pairs: &BTreeMap<String, usize>) -> Vec<Value> {
    let mut ranked: Vec<(&String, &usize)> = pairs.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    ranked
        .into_iter()
        .take(TOP_PAIRS)
        .map(|(pair, count)| json!({"pair": pair, "count": count}))
        .collect()
}

fn summarize(rows: &[Value], kind: Kind, include_fixtures: bool) -> Value {
    let outcomes = latest_outcomes(rows, kind);
    let mut groups = BTreeMap::<GroupKey, Group>::new();
    let mut excluded_fixtures = 0_usize;
    for row in rows {
        if row.get("event_type").and_then(Value::as_str) != Some(kind.event_type()) {
            continue;
        }
        let Some(payload) = payload(row) else {
            continue;
        };
        if !include_fixtures && is_fixture(payload) {
            excluded_fixtures += 1;
            continue;
        }
        let key: GroupKey = kind
            .key_fields()
            .iter()
            .map(|field| text(payload, field))
            .collect();
        let group = groups.entry(key).or_default();
        group.count += 1;
        group.usage.record(payload);

        let recommendation = text(payload, kind.recommendation_field());
        if let Some(recommendation) = &recommendation {
            *group
                .recommendations
                .entry(recommendation.clone())
                .or_default() += 1;
        }
        if let (Kind::Checkpoint, Some(recommendation), Some(baseline)) =
            (kind, &recommendation, text(payload, "baseline_action"))
        {
            group.comparable += 1;
            group.agreed += usize::from(*recommendation == baseline);
            *group
                .pairs
                .entry(format!("{baseline}->{recommendation}"))
                .or_default() += 1;
        }
        if let Some(confidence) = payload
            .get("recommendation_confidence")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        {
            group.confidence.push(confidence);
        }
        if let Some(latency) = payload.get("jev_latency_ms").and_then(Value::as_f64) {
            group.latency.push(latency);
        }
        match total_tokens(payload) {
            Ok(Some(tokens)) => {
                group.token_samples += 1;
                if let Some(total) = group.tokens.checked_add(tokens) {
                    group.tokens = total;
                } else {
                    group.token_overflowed = true;
                }
            }
            Err(()) => {
                group.token_samples += 1;
                group.token_overflowed = true;
                group.token_overflowed_samples += 1;
            }
            Ok(None) => {}
        }
        if let Some(outcome) = text(payload, "call_id").and_then(|call| outcomes.get(&call)) {
            *group.outcomes.entry(outcome.clone()).or_default() += 1;
            if outcome != "unknown" {
                group.labeled += 1;
                group.correct += usize::from(outcome == "correct");
            }
        }
    }

    let mut rendered = Vec::with_capacity(groups.len());
    for (key, mut group) in groups {
        let mut entry = Map::new();
        for (field, value) in kind.key_fields().iter().zip(key) {
            entry.insert((*field).to_string(), json!(value));
        }
        entry.insert("count".into(), json!(group.count));
        entry.insert(
            "interaction_label".into(),
            json!(origin_label(
                entry.get("interaction_origin").and_then(Value::as_str)
            )),
        );
        entry.insert("labeled".into(), json!(group.labeled));
        entry.insert("unlabeled".into(), json!(group.count - group.labeled));
        entry.insert("outcomes".into(), json!(group.outcomes));
        entry.insert("correct".into(), json!(group.correct));
        entry.insert(
            "accuracy".into(),
            json!(ratio(group.correct, group.labeled)),
        );
        entry.insert("recommendations".into(), json!(group.recommendations));
        entry.insert("confidence_samples".into(), json!(group.confidence.len()));
        entry.insert(
            "mean_confidence".into(),
            json!(if group.confidence.is_empty() {
                None
            } else {
                Some(group.confidence.iter().sum::<f64>() / group.confidence.len() as f64)
            }),
        );
        if matches!(kind, Kind::Checkpoint) {
            entry.insert("comparable".into(), json!(group.comparable));
            entry.insert(
                "agreement_rate".into(),
                json!(ratio(group.agreed, group.comparable)),
            );
            entry.insert("top_pairs".into(), json!(top_pairs(&group.pairs)));
            entry.insert(
                "median_confidence".into(),
                json!(median(&mut group.confidence)),
            );
        }
        entry.insert(
            "median_latency_ms".into(),
            json!(median(&mut group.latency)),
        );
        let mut tokens = json!({"total": group.tokens, "samples": group.token_samples});
        if group.token_overflowed {
            tokens["total"] = Value::Null;
            tokens["overflowed"] = json!(true);
            tokens["overflowed_samples"] = json!(group.token_overflowed_samples);
        }
        entry.insert("tokens".into(), tokens);
        for (key, value) in group.usage.render().as_object().unwrap() {
            entry.insert(key.clone(), value.clone());
        }
        rendered.push(Value::Object(entry));
    }
    json!({
        "include_fixtures": include_fixtures,
        "excluded_fixture_events": excluded_fixtures,
        "groups": rendered,
    })
}

pub fn checkpoint_groups(rows: &[Value], include_fixtures: bool) -> Value {
    summarize(rows, Kind::Checkpoint, include_fixtures)
}

pub fn route_groups(rows: &[Value], include_fixtures: bool) -> Value {
    summarize(rows, Kind::Route, include_fixtures)
}

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Jev's own confidence samples on one calendar day. Labels and baselines are ignored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyConfidence {
    pub day: i64,
    pub samples: usize,
    pub mean: f64,
}

/// Only numeric, finite probabilities are confidence; zero is a valid sample.
pub fn recommendation_confidence(payload: &Value) -> Option<f64> {
    payload
        .get("recommendation_confidence")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
}

/// Daily mean of genuine recorded confidence, oldest first, without outcome joins.
/// Missing timestamps or confidence contribute no sample, leaving calendar gaps.
pub fn daily_checkpoint_confidence(rows: &[Value], include_fixtures: bool) -> Vec<DailyConfidence> {
    let mut days = BTreeMap::<i64, (usize, f64)>::new();
    for row in rows {
        if row["event_type"] != "jev.checkpoint" {
            continue;
        }
        let Some(payload) = payload(row) else {
            continue;
        };
        if !include_fixtures && is_fixture(payload) {
            continue;
        }
        let Some(confidence) = recommendation_confidence(&row["payload"]) else {
            continue;
        };
        let Some(at) = row["recorded_at_us"].as_i64() else {
            continue;
        };
        let entry = days.entry(at.div_euclid(MICROS_PER_DAY)).or_default();
        entry.0 += 1;
        // Incremental mean avoids an unbounded accumulating sum.
        entry.1 += (confidence - entry.1) / entry.0 as f64;
    }
    days.into_iter()
        .map(|(day, (samples, mean))| DailyConfidence { day, samples, mean })
        .collect()
}

/// Labeled checkpoint decisions on one UTC day of the checkpoint itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyAccuracy {
    /// Days since the Unix epoch.
    pub day: i64,
    pub labeled: usize,
    pub correct: usize,
}

/// Labeled checkpoint decisions per UTC day, oldest first, using the same
/// latest-label join, fixture rule and `unknown` exclusion as
/// `checkpoint_groups`, so the days sum to its `labeled` and `correct`.
pub fn daily_checkpoint_accuracy(rows: &[Value], include_fixtures: bool) -> Vec<DailyAccuracy> {
    let outcomes = latest_outcomes(rows, Kind::Checkpoint);
    let mut days = BTreeMap::<i64, (usize, usize)>::new();
    for row in rows {
        if row.get("event_type").and_then(Value::as_str) != Some(Kind::Checkpoint.event_type()) {
            continue;
        }
        let Some(payload) = payload(row) else {
            continue;
        };
        if !include_fixtures && is_fixture(payload) {
            continue;
        }
        let Some(outcome) = text(payload, "call_id").and_then(|call| outcomes.get(&call)) else {
            continue;
        };
        if outcome == "unknown" {
            continue;
        }
        let at = row
            .get("recorded_at_us")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let day = days.entry(at.div_euclid(MICROS_PER_DAY)).or_default();
        day.0 += 1;
        day.1 += usize::from(outcome == "correct");
    }
    days.into_iter()
        .map(|(day, (labeled, correct))| DailyAccuracy {
            day,
            labeled,
            correct,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn categories_separate_origins_and_context_versions_without_inventing_history() {
        let row = |origin, version, confidence| {
            json!({"event_type":"jev.checkpoint","payload":{
            "family":"tool_selection", "interaction_origin":origin,"context_version":version,
            "recommendation_confidence":confidence,"jev_input_tokens":10,"jev_output_tokens":2,"jev_cost_usd":0.01}})
        };
        let rows = vec![
            row(json!("automatic_tool"), json!("tool-context-1"), 0.4),
            row(json!("automatic_tool"), json!("tool-context-1"), 0.8),
            row(json!("explicit_decision"), json!("decision-context-1"), 0.9),
            row(json!("automatic_tool"), json!("tool-context-2"), 0.7),
            row(Value::Null, Value::Null, 0.3),
        ];
        let report = checkpoint_groups(&rows, false);
        let groups = report["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 4);
        let automatic = groups
            .iter()
            .find(|g| g["context_version"] == "tool-context-1")
            .unwrap();
        assert_eq!(automatic["interaction_label"], "Automatic tool check");
        assert_eq!(automatic["confidence_samples"], 2);
        assert!((automatic["mean_confidence"].as_f64().unwrap() - 0.6).abs() < 1e-12);
        assert_eq!(automatic["tokens"]["total"], 24);
        assert_eq!(automatic["cost_usd"]["total"], 0.02);
        let historical = groups
            .iter()
            .find(|g| g["interaction_origin"].is_null())
            .unwrap();
        assert_eq!(historical["interaction_label"], "Legacy / unknown");
        assert!(historical["context_version"].is_null());
    }
    #[test]
    fn usage_counts_calls_once_and_keeps_partial_zero_and_overflow_measurements() {
        let rows = vec![
            json!({"event_type":"jev.checkpoint","payload":{"family":"a","jev_input_tokens":10,"jev_output_tokens":2,"jev_cost_usd":0.00000042,"jev_cost_source":"list_price_estimate"}}),
            json!({"event_type":"jev.route","payload":{"jev_total_tokens":0,"jev_input_tokens":0,"jev_output_tokens":0,"jev_cost_usd":0,"jev_cost_source":"provider"}}),
            json!({"event_type":"jev.route","payload":{}}),
            json!({"event_type":"jev.checkpoint_outcome","payload":{"jev_total_tokens":999}}),
            json!({"event_type":"orchestration.decision_evaluated","payload":{"total_tokens":999,"cost_usd":99}}),
            json!({"event_type":"jev.checkpoint","payload":{"model_requested":"fixture","jev_total_tokens":999}}),
            json!({"event_type":"jev.route","payload":{"model_returned":"jev-9.9.9","jev_total_tokens":999,"jev_cost_usd":0.0005}}),
        ];
        let checkpoints = super::checkpoint_groups(&rows, false);
        let routes = super::route_groups(&rows, false);
        let totals = super::usage_totals(&[&checkpoints, &routes]);
        assert_eq!(totals["calls"], 3);
        assert_eq!(totals["tokens"]["total"], 12);
        assert_eq!(totals["tokens"]["samples"], 2);
        assert_eq!(totals["input_tokens"]["total"], 10);
        assert_eq!(totals["output_tokens"]["total"], 2);
        assert_eq!(totals["cost_usd"]["samples"], 2);
        assert_eq!(totals["cost_usd"]["estimated_samples"], 1);
        assert_eq!(totals["cost_usd"]["provider_samples"], 1);
        assert_eq!(totals["cost_usd"]["total"], 0.00000042);
        let missing = super::usage_totals(&[&super::route_groups(&[rows[2].clone()], false)]);
        assert!(missing["tokens"]["total"].is_null());
        assert!(missing["cost_usd"]["total"].is_null());
        let overflow = super::route_groups(
            &[
                json!({"event_type":"jev.route","payload":{"jev_total_tokens":i64::MAX}}),
                json!({"event_type":"jev.route","payload":{"jev_total_tokens":1}}),
            ],
            false,
        );
        assert!(super::usage_totals(&[&overflow])["tokens"]["total"].is_null());
        let invalid = super::route_groups(
            &[
                json!({"event_type":"jev.route","payload":{"jev_total_tokens":-1,"jev_input_tokens":true,"jev_cost_usd":-1}}),
            ],
            false,
        );
        assert_eq!(invalid["groups"][0]["tokens"]["samples"], 0);
        assert!(invalid["groups"][0]["cost_usd"]["total"].is_null());
    }
    use super::*;

    fn event(event_type: &str, at: i64, payload: Value) -> Value {
        json!({"id": format!("{event_type}-{at}"), "event_type": event_type, "payload": payload, "recorded_at_us": at})
    }

    fn checkpoint(at: i64, call: &str, baseline: &str, recommendation: &str, model: &str) -> Value {
        event(
            "jev.checkpoint",
            at,
            json!({
                "call_id": call, "family": "plan", "question_version": "q1", "policy_version": "p1",
                "model_requested": model, "model_returned": model,
                "baseline_action": baseline, "recommendation": recommendation,
                "recommendation_confidence": 0.5 + at as f64 / 100.0,
                "jev_latency_ms": 100 * at, "jev_total_tokens": 10,
            }),
        )
    }

    fn label(at: i64, call: &str, outcome: &str) -> Value {
        event(
            "jev.checkpoint_outcome",
            at,
            json!({"call_id": call, "outcome": outcome}),
        )
    }

    fn only_group(summary: &Value) -> &Value {
        let groups = summary["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{summary}");
        &groups[0]
    }

    #[test]
    fn daily_confidence_uses_only_valid_recorded_samples_without_labels_or_baselines() {
        let make = |day: i64, value: Value| {
            let mut row = checkpoint(day * MICROS_PER_DAY, "same", "run", "hold", "m");
            row["payload"]["recommendation_confidence"] = value;
            row
        };
        let mut rows = vec![
            make(0, json!(0)),
            make(0, json!(1)),
            make(1, Value::Null),
            make(1, json!(-0.1)),
            make(1, json!(1.1)),
            make(1, json!("0.8")),
            make(2, json!(0.75)),
            label(100, "same", "correct"),
        ];
        let mut fixture = make(0, json!(1));
        fixture["payload"]["model_returned"] = json!("fixture");
        rows.push(fixture);
        let mut missing_time = make(3, json!(0.8));
        missing_time
            .as_object_mut()
            .unwrap()
            .remove("recorded_at_us");
        rows.push(missing_time);
        let expected = vec![
            DailyConfidence {
                day: 0,
                samples: 2,
                mean: 0.5,
            },
            DailyConfidence {
                day: 2,
                samples: 1,
                mean: 0.75,
            },
        ];
        assert_eq!(daily_checkpoint_confidence(&rows, false), expected);
        assert_eq!(daily_checkpoint_confidence(&rows, true)[0].samples, 3);
        for row in &mut rows {
            row["payload"]["baseline_action"] = json!("hold");
            row["payload"]["outcome"] = json!("under_escalated");
        }
        assert_eq!(daily_checkpoint_confidence(&rows, false), expected);
        assert_eq!(
            recommendation_confidence(&json!({"recommendation_confidence": 0})),
            Some(0.0)
        );
        assert_eq!(recommendation_confidence(&json!({})), None);
        assert_eq!(
            recommendation_confidence(&json!({"recommendation_confidence": f64::INFINITY})),
            None
        );
    }

    #[test]
    fn groups_agreement_accuracy_and_medians() {
        let rows = vec![
            checkpoint(1, "a", "proceed", "proceed", "m"),
            checkpoint(2, "b", "proceed", "refine_plan", "m"),
            checkpoint(3, "c", "proceed", "refine_plan", "m"),
            checkpoint(4, "d", "proceed", "refine_plan", "m"),
            label(10, "a", "correct"),
            label(11, "b", "over_escalated"),
            label(12, "c", "unknown"),
        ];
        let summary = checkpoint_groups(&rows, false);
        let group = only_group(&summary);
        assert_eq!(group["family"], "plan");
        assert_eq!(group["count"], 4);
        assert_eq!(group["comparable"], 4);
        assert_eq!(group["agreement_rate"], 0.25);
        assert_eq!(group["labeled"], 2);
        assert_eq!(group["correct"], 1);
        assert_eq!(group["accuracy"], 0.5);
        assert_eq!(group["unlabeled"], 2);
        assert_eq!(
            group["outcomes"],
            json!({"correct": 1, "over_escalated": 1, "unknown": 1})
        );
        assert_eq!(group["median_confidence"], 0.525);
        assert_eq!(group["median_latency_ms"], 250.0);
        assert_eq!(group["tokens"], json!({"total": 40, "samples": 4}));
        assert_eq!(
            group["top_pairs"][0],
            json!({"pair": "proceed->refine_plan", "count": 3})
        );
        assert_eq!(
            group["top_pairs"][1],
            json!({"pair": "proceed->proceed", "count": 1})
        );
    }

    #[test]
    fn later_label_replaces_earlier_and_orphans_are_ignored() {
        let rows = vec![
            checkpoint(1, "a", "proceed", "proceed", "m"),
            label(5, "a", "incorrect"),
            label(9, "a", "correct"),
            label(7, "a", "unknown"),
            label(8, "missing", "correct"),
        ];
        let summary = checkpoint_groups(&rows, false);
        let group = only_group(&summary);
        assert_eq!(group["outcomes"], json!({"correct": 1}));
        assert_eq!(group["labeled"], 1);
        assert_eq!(group["accuracy"], 1.0);
    }

    #[test]
    fn fixture_models_are_excluded_unless_requested() {
        let rows = vec![
            checkpoint(1, "a", "proceed", "proceed", "fixture"),
            checkpoint(2, "b", "proceed", "proceed", "real-model"),
        ];
        let default = checkpoint_groups(&rows, false);
        assert_eq!(default["excluded_fixture_events"], 1);
        assert_eq!(only_group(&default)["model_requested"], "real-model");

        let included = checkpoint_groups(&rows, true);
        assert_eq!(included["excluded_fixture_events"], 0);
        assert_eq!(included["groups"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn groups_split_by_version_and_missing_facts_stay_null() {
        let mut other = checkpoint(2, "b", "proceed", "proceed", "m");
        other["payload"]["policy_version"] = json!("p2");
        let bare = event(
            "jev.checkpoint",
            3,
            json!({"call_id": "c", "family": "plan"}),
        );
        let summary = checkpoint_groups(
            &[checkpoint(1, "a", "proceed", "proceed", "m"), other, bare],
            false,
        );
        let groups = summary["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 3);
        let bare_group = groups
            .iter()
            .find(|g| g["policy_version"].is_null())
            .unwrap();
        assert_eq!(bare_group["agreement_rate"], Value::Null);
        assert_eq!(bare_group["accuracy"], Value::Null);
        assert_eq!(bare_group["median_latency_ms"], Value::Null);
        assert_eq!(bare_group["tokens"], json!({"total": 0, "samples": 0}));
    }

    #[test]
    fn empty_input_yields_no_groups() {
        assert_eq!(checkpoint_groups(&[], false)["groups"], json!([]));
        assert_eq!(route_groups(&[], false)["groups"], json!([]));
    }

    #[test]
    fn route_groups_distribute_observed_recommendation_and_join_outcomes() {
        let route = |at: i64, call: &str, observed: &str, model: &str| {
            event(
                "jev.route",
                at,
                json!({
                    "call_id": call, "source": "typesafe", "routing_mode": "shadow",
                    "observed_recommendation": observed, "model_requested": model,
                    "jev_input_tokens": 3, "jev_output_tokens": 2, "jev_latency_ms": 40,
                }),
            )
        };
        let rows = vec![
            route(1, "a", "proceed", "m"),
            route(2, "b", "reasoning_model", "m"),
            route(3, "c", "proceed", "fixture"),
            event(
                "jev.outcome",
                9,
                json!({"call_id": "a", "outcome": "correct"}),
            ),
            checkpoint(4, "z", "proceed", "proceed", "m"),
        ];
        let summary = route_groups(&rows, false);
        let group = only_group(&summary);
        assert_eq!(group["source"], "typesafe");
        assert_eq!(group["count"], 2);
        assert_eq!(
            group["recommendations"],
            json!({"proceed": 1, "reasoning_model": 1})
        );
        assert_eq!(group["labeled"], 1);
        assert_eq!(group["accuracy"], 1.0);
        assert_eq!(group["tokens"], json!({"total": 10, "samples": 2}));
        assert!(group.get("agreement_rate").is_none());
        assert_eq!(summary["excluded_fixture_events"], 1);
    }

    #[test]
    fn daily_accuracy_buckets_by_checkpoint_day_and_matches_groups() {
        let day = MICROS_PER_DAY;
        let rows = vec![
            checkpoint(1, "a", "proceed", "proceed", "m"),
            checkpoint(2, "b", "proceed", "hold", "m"),
            checkpoint(day + 1, "c", "proceed", "proceed", "m"),
            checkpoint(day + 2, "d", "proceed", "proceed", "m"),
            checkpoint(3 * day, "e", "proceed", "proceed", "fixture"),
            label(5, "a", "correct"),
            label(6, "b", "correct"),
            // A later label replaces the earlier one, and is bucketed by the
            // checkpoint's day rather than the label's.
            label(4 * day, "b", "over_escalated"),
            label(day + 5, "c", "correct"),
            label(day + 6, "d", "unknown"),
            label(3 * day + 1, "e", "correct"),
        ];
        let daily = daily_checkpoint_accuracy(&rows, false);
        assert_eq!(
            daily,
            vec![
                DailyAccuracy {
                    day: 0,
                    labeled: 2,
                    correct: 1
                },
                DailyAccuracy {
                    day: 1,
                    labeled: 1,
                    correct: 1
                },
            ]
        );
        let group = only_group(&checkpoint_groups(&rows, false)).clone();
        assert_eq!(
            daily.iter().map(|d| d.labeled).sum::<usize>(),
            group["labeled"].as_u64().unwrap() as usize
        );
        assert_eq!(
            daily.iter().map(|d| d.correct).sum::<usize>(),
            group["correct"].as_u64().unwrap() as usize
        );
        assert_eq!(daily_checkpoint_accuracy(&rows, true).len(), 3);
    }
    #[test]
    fn overflowing_component_and_group_totals_are_unknown_for_both_report_kinds() {
        for kind in ["jev.checkpoint", "jev.route"] {
            let report = |rows: &[Value]| {
                if kind == "jev.checkpoint" {
                    checkpoint_groups(rows, false)
                } else {
                    route_groups(rows, false)
                }
            };
            let component_rows = vec![
                event(
                    kind,
                    1,
                    json!({"jev_input_tokens": i64::MAX, "jev_output_tokens": 1}),
                ),
                event(kind, 2, json!({"jev_total_tokens": 5})),
                event(kind, 3, json!({"jev_input_tokens": 3})),
            ];
            let component = report(&component_rows);
            assert_eq!(
                only_group(&component)["tokens"],
                json!({"total":null,"samples":2,"overflowed":true,"overflowed_samples":1})
            );
            assert_eq!(only_group(&component)["count"], 3);
            let group_rows = vec![
                event(kind, 1, json!({"jev_total_tokens": i64::MAX})),
                event(kind, 2, json!({"jev_total_tokens": 1})),
                // A later sample cannot turn an overflowed total into a known one.
                event(kind, 3, json!({"jev_total_tokens": 0})),
            ];
            assert_eq!(
                only_group(&report(&group_rows))["tokens"],
                json!({"total":null,"samples":3,"overflowed":true,"overflowed_samples":0})
            );
        }
    }

    #[test]
    fn valid_zero_and_missing_token_measurements_keep_sample_semantics() {
        for kind in ["jev.checkpoint", "jev.route"] {
            let rows = vec![
                event(
                    kind,
                    1,
                    json!({"jev_input_tokens": 2, "jev_output_tokens": 3}),
                ),
                event(
                    kind,
                    2,
                    json!({"jev_total_tokens": 0, "jev_input_tokens": i64::MAX, "jev_output_tokens": 1}),
                ),
                event(kind, 3, json!({"jev_input_tokens": 9})),
                event(kind, 4, json!({})),
            ];
            let report = if kind == "jev.checkpoint" {
                checkpoint_groups(&rows, false)
            } else {
                route_groups(&rows, false)
            };
            assert_eq!(
                only_group(&report)["tokens"],
                json!({"total":5,"samples":2})
            );
            assert_eq!(only_group(&report)["count"], 4);
        }
    }
}
