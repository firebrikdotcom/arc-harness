//! Grouped summaries over projected audit rows.
//!
//! Pure functions over `audit_events_view` rows so the projection can be tested
//! without a running service. Only compact facts already present in event
//! payloads are read; nothing here reaches outside the local database.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const FIXTURE_MODEL: &str = "fixture";
const TOP_PAIRS: usize = 5;

type GroupKey = Vec<Option<String>>;

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
            ],
            Kind::Route => &[
                "source",
                "routing_mode",
                "model_requested",
                "model_returned",
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
    ["model_requested", "model_returned"]
        .iter()
        .any(|key| payload.get(*key).and_then(Value::as_str) == Some(FIXTURE_MODEL))
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

fn total_tokens(payload: &Map<String, Value>) -> Option<i64> {
    if let Some(total) = payload.get("jev_total_tokens").and_then(Value::as_i64) {
        return Some(total);
    }
    let input = payload.get("jev_input_tokens").and_then(Value::as_i64)?;
    let output = payload.get("jev_output_tokens").and_then(Value::as_i64)?;
    Some(input + output)
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
        {
            group.confidence.push(confidence);
        }
        if let Some(latency) = payload.get("jev_latency_ms").and_then(Value::as_f64) {
            group.latency.push(latency);
        }
        if let Some(tokens) = total_tokens(payload) {
            group.tokens += tokens;
            group.token_samples += 1;
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
        entry.insert("labeled".into(), json!(group.labeled));
        entry.insert("unlabeled".into(), json!(group.count - group.labeled));
        entry.insert("outcomes".into(), json!(group.outcomes));
        entry.insert("correct".into(), json!(group.correct));
        entry.insert(
            "accuracy".into(),
            json!(ratio(group.correct, group.labeled)),
        );
        entry.insert("recommendations".into(), json!(group.recommendations));
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
        entry.insert(
            "tokens".into(),
            json!({"total": group.tokens, "samples": group.token_samples}),
        );
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
}
