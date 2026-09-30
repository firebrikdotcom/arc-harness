//! Browser workbench for the audit database, rendered through the Arc UI host.
//!
//! The pages read the same projected rows as the JSON API and reuse the grouped
//! summaries from `summary.rs`, so what the browser shows and what `curl` shows
//! can never drift apart. Templates and the stylesheet are embedded at compile
//! time, so the service needs no working directory or asset build to serve them.

use crate::domain::audit_event::projector::AUDIT_EVENTS_VIEW;
use crate::summary as grouped;
use actix_session::Session;
use actix_web::http::StatusCode;
use actix_web::{get, web, HttpRequest, HttpResponse, Responder};
use arc_core::read_model_store::ReadModelStore;
use arc_web::ui::{
    Breadcrumb, TemplateBundle, TemplateDef, TemplateName, UiContribution, UiHost, UiPage,
};
use arc_web::UiRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use tera::Context;

const STYLESHEET: &str = include_str!("../public/styles.css");

const HOST_TEMPLATES: &[TemplateDef] = &[
    TemplateDef {
        name: TemplateName("layouts/admin.html"),
        source: include_str!("../resources/views/layouts/admin.html"),
    },
    TemplateDef {
        name: TemplateName("layouts/public.html"),
        source: include_str!("../resources/views/layouts/public.html"),
    },
    TemplateDef {
        name: TemplateName("components/ui.html"),
        source: include_str!("../resources/views/components/ui.html"),
    },
    TemplateDef {
        name: TemplateName("audit/workbench.html"),
        source: include_str!("../resources/views/audit/workbench.html"),
    },
    TemplateDef {
        name: TemplateName("audit/events.html"),
        source: include_str!("../resources/views/audit/events.html"),
    },
];

const RECENT_EVENTS: usize = 25;
const MAX_EVENTS: usize = 500;

pub fn host() -> UiHost {
    UiHost {
        owner: env!("CARGO_PKG_NAME"),
        templates: TemplateBundle {
            templates: HOST_TEMPLATES,
        },
        admin_layout: TemplateName("layouts/admin.html"),
        public_layout: TemplateName("layouts/public.html"),
    }
}

pub fn contribution() -> UiContribution {
    UiContribution {
        owner: env!("CARGO_PKG_NAME"),
        ..UiContribution::default()
    }
}

/// Navigation rendered by the layout. The framework's `admin_navigation` is
/// only shown to an authenticated identity, and this service has no sign-in,
/// so the host supplies its own unauthenticated links.
#[derive(Serialize)]
struct NavItem {
    label: &'static str,
    href: &'static str,
}

const NAVIGATION: &[NavItem] = &[
    NavItem {
        label: "Workbench",
        href: "/",
    },
    NavItem {
        label: "Events",
        href: "/events",
    },
];

#[derive(Debug, Default, Deserialize)]
pub struct WorkbenchQuery {
    #[serde(default)]
    include_fixtures: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    #[serde(rename = "type")]
    event_type: Option<String>,
    limit: Option<usize>,
}

fn render(
    registry: &UiRegistry,
    request: &HttpRequest,
    session: &Session,
    template: &'static str,
    title: &str,
    mut context: Context,
    breadcrumbs: Vec<Breadcrumb>,
) -> HttpResponse {
    decorate(&mut context, title);
    registry.render(
        UiPage {
            template: TemplateName(template),
            title: title.into(),
            context,
            status: StatusCode::OK,
            breadcrumbs,
        },
        request,
        session,
    )
}

/// Everything the host layout needs beyond what `UiRegistry::render` injects.
/// `UiPage::new` would insert `title`, but the page is built directly so the
/// status and breadcrumbs can be set in one place; keep the two in step here.
fn decorate(context: &mut Context, title: &str) {
    context.insert("title", title);
    // `app_name` is reserved by the registry and falls back to the framework
    // crate's name when APP_NAME is unset, so the layout shows this instead.
    context.insert("service_name", "Harness audit");
    context.insert("local_navigation", NAVIGATION);
    context.insert("app_version", env!("CARGO_PKG_VERSION"));
}

fn unavailable(error: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .content_type("text/plain; charset=utf-8")
        .body(format!("The audit database could not be read: {error}"))
}

fn text(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Compact, template-friendly view of one projected event row.
pub fn event_row(row: &Value) -> Value {
    let payload = row.get("payload").cloned().unwrap_or(Value::Null);
    let recorded_at_us = row.get("recorded_at_us").and_then(Value::as_i64);
    // Routes carry both the shadow `recommendation` (always the default path)
    // and what Jev actually answered; outcomes carry the action taken instead.
    let recommendation = if row.get("event_type").and_then(Value::as_str) == Some("jev.route") {
        text(&payload, "observed_recommendation").or_else(|| text(&payload, "recommendation"))
    } else {
        text(&payload, "recommendation")
            .or_else(|| text(&payload, "observed_recommendation"))
            .or_else(|| text(&payload, "action_taken"))
    };
    json!({
        "id": row.get("id"),
        "event_type": row.get("event_type"),
        "recorded_at": recorded_at_us.map(format_timestamp),
        "call_id": text(&payload, "call_id"),
        "family": text(&payload, "family"),
        "question_version": text(&payload, "question_version"),
        "baseline_action": text(&payload, "baseline_action"),
        "recommendation": recommendation,
        "outcome": text(&payload, "outcome"),
        "confidence": payload.get("recommendation_confidence").and_then(Value::as_f64),
        "latency_ms": payload.get("jev_latency_ms").and_then(Value::as_i64),
    })
}

/// `2026-09-29 21:06:25Z` from microseconds since the Unix epoch, without a
/// calendar dependency.
pub fn format_timestamp(micros: i64) -> String {
    let secs = micros.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// Howard Hinnant's days-to-civil algorithm; exact for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn sorted_newest_first(mut rows: Vec<Value>) -> Vec<Value> {
    rows.sort_by_key(|row| row.get("recorded_at_us").and_then(Value::as_i64));
    rows.reverse();
    rows
}

/// Headline numbers for the metric grid, derived from the grouped summary so
/// they agree with the table beneath them.
pub fn totals(event_count: usize, checkpoints: &Value) -> Value {
    let groups = checkpoints
        .get("groups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sum = |key: &str| -> u64 {
        groups
            .iter()
            .filter_map(|group| group.get(key).and_then(Value::as_u64))
            .sum()
    };
    let (count, labeled, correct) = (sum("count"), sum("labeled"), sum("correct"));
    json!({
        "events": event_count,
        "checkpoints": count,
        "labeled": labeled,
        "unlabeled": count.saturating_sub(labeled),
        "correct": correct,
        "accuracy_percent": (correct * 100 + labeled / 2).checked_div(labeled).unwrap_or(0),
        "excluded_fixtures": checkpoints.get("excluded_fixture_events").and_then(Value::as_u64).unwrap_or(0),
    })
}

#[get("/")]
async fn workbench(
    request: HttpRequest,
    session: Session,
    query: web::Query<WorkbenchQuery>,
    store: web::Data<dyn ReadModelStore>,
    registry: web::Data<UiRegistry>,
) -> impl Responder {
    let rows = match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => rows,
        Err(error) => return unavailable(error),
    };
    let checkpoints = grouped::checkpoint_groups(&rows, query.include_fixtures);
    let routes = grouped::route_groups(&rows, query.include_fixtures);
    let recent: Vec<Value> = sorted_newest_first(rows.clone())
        .iter()
        .take(RECENT_EVENTS)
        .map(event_row)
        .collect();
    let mut context = Context::new();
    context.insert("totals", &totals(rows.len(), &checkpoints));
    context.insert("checkpoints", &checkpoints);
    context.insert("routes", &routes);
    context.insert("recent", &recent);
    context.insert("include_fixtures", &query.include_fixtures);
    render(
        &registry,
        &request,
        &session,
        "audit/workbench.html",
        "Audit workbench",
        context,
        vec![Breadcrumb::current("Workbench")],
    )
}

#[get("/events")]
async fn event_log(
    request: HttpRequest,
    session: Session,
    query: web::Query<EventsQuery>,
    store: web::Data<dyn ReadModelStore>,
    registry: web::Data<UiRegistry>,
) -> impl Responder {
    let rows = match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => sorted_newest_first(rows),
        Err(error) => return unavailable(error),
    };
    let event_types: BTreeSet<String> = rows
        .iter()
        .filter_map(|row| row.get("event_type").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let type_filter = query
        .event_type
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let limit = query.limit.unwrap_or(200).clamp(1, MAX_EVENTS);
    let matching: Vec<&Value> = rows
        .iter()
        .filter(|row| match &type_filter {
            Some(wanted) => row.get("event_type").and_then(Value::as_str) == Some(wanted),
            None => true,
        })
        .collect();
    let shown: Vec<Value> = matching.iter().take(limit).map(|row| event_row(row)).collect();
    let mut context = Context::new();
    context.insert("total", &matching.len());
    context.insert("shown", &shown.len());
    context.insert("events", &shown);
    context.insert("event_types", &event_types);
    context.insert("type_filter", &type_filter);
    render(
        &registry,
        &request,
        &session,
        "audit/events.html",
        "Audit events",
        context,
        vec![
            Breadcrumb::link("Workbench", "/"),
            Breadcrumb::current("Events"),
        ],
    )
}

#[get("/public/styles.css")]
async fn stylesheet() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/css; charset=utf-8")
        .insert_header(("Cache-Control", "public, max-age=300"))
        .body(STYLESHEET)
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(workbench).service(event_log).service(stylesheet);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(call: &str, family: &str, baseline: &str, recommendation: &str, at: i64) -> Value {
        json!({
            "id": format!("cp-{call}"),
            "event_type": "jev.checkpoint",
            "recorded_at_us": at,
            "payload": {
                "call_id": call,
                "family": family,
                "question_version": "v1",
                "policy_version": "shadow-1",
                "model_requested": "jev-1.13.0",
                "model_returned": "jev-1.13.0",
                "baseline_action": baseline,
                "recommendation": recommendation,
                "recommendation_confidence": 0.3299999999999996,
                "jev_latency_ms": 210,
                "jev_input_tokens": 500,
                "jev_output_tokens": 60
            }
        })
    }

    fn outcome(call: &str, outcome: &str, at: i64) -> Value {
        json!({
            "id": format!("out-{call}"),
            "event_type": "jev.checkpoint_outcome",
            "recorded_at_us": at,
            "payload": {"call_id": call, "family": "evidence_assessment", "outcome": outcome}
        })
    }

    #[test]
    fn host_templates_form_a_valid_registry() {
        let registry = UiRegistry::build(Some(&host()), &[contribution()])
            .expect("templates parse")
            .expect("host registered");
        assert_eq!(registry.admin_layout().0, "layouts/admin.html");
    }

    #[test]
    fn totals_follow_the_grouped_summary() {
        let rows = vec![
            checkpoint("a", "evidence_assessment", "run", "run", 1),
            checkpoint("b", "evidence_assessment", "run", "fix", 2),
            checkpoint("c", "handoff_assessment", "go", "hold", 3),
            outcome("a", "correct", 4),
            outcome("b", "over_escalated", 5),
        ];
        let summary = grouped::checkpoint_groups(&rows, false);
        let totals = totals(rows.len(), &summary);
        assert_eq!(totals["events"], 5);
        assert_eq!(totals["checkpoints"], 3);
        assert_eq!(totals["labeled"], 2);
        assert_eq!(totals["unlabeled"], 1);
        assert_eq!(totals["correct"], 1);
        assert_eq!(totals["accuracy_percent"], 50);
    }

    #[test]
    fn event_rows_flatten_payload_fields_and_format_time() {
        let row = event_row(&checkpoint("abc", "evidence_assessment", "run", "fix", 1_790_629_585_000_000));
        assert_eq!(row["family"], "evidence_assessment");
        assert_eq!(row["baseline_action"], "run");
        assert_eq!(row["recommendation"], "fix");
        assert_eq!(row["recorded_at"], "2026-09-28 21:06:25Z");
        assert_eq!(row["latency_ms"], 210);
        let route = event_row(&json!({
            "event_type": "jev.route",
            "recorded_at_us": 0,
            "payload": {"recommendation": "default", "observed_recommendation": "reasoning_model"}
        }));
        assert_eq!(route["recommendation"], "reasoning_model");
        let labeled = event_row(&json!({
            "event_type": "jev.checkpoint_outcome",
            "recorded_at_us": 0,
            "payload": {"baseline_action": "retry_same_command", "action_taken": "change_approach", "outcome": "correct"}
        }));
        assert_eq!(labeled["recommendation"], "change_approach");
        assert_eq!(labeled["outcome"], "correct");
        assert_eq!(route["recorded_at"], "1970-01-01 00:00:00Z");
    }

    #[test]
    fn timestamps_handle_leap_years_and_negatives() {
        assert_eq!(format_timestamp(951_782_400_000_000), "2000-02-29 00:00:00Z");
        assert_eq!(format_timestamp(-1_000_000), "1969-12-31 23:59:59Z");
    }

    #[test]
    fn workbench_renders_summary_tables_with_the_arc_layout() {
        let rows = vec![
            checkpoint("a", "evidence_assessment", "run_full_verification", "run_full_verification", 1),
            outcome("a", "correct", 2),
            json!({
                "id": "r1", "event_type": "jev.route", "recorded_at_us": 3,
                "payload": {"call_id": "r1", "source": "typesafe", "routing_mode": "shadow",
                            "model_requested": "jev-1.13.0", "model_returned": "jev-1.13.0",
                            "observed_recommendation": "reasoning_model", "jev_latency_ms": 190}
            }),
        ];
        let registry = UiRegistry::build(Some(&host()), &[contribution()]).unwrap().unwrap();
        let checkpoints = grouped::checkpoint_groups(&rows, false);
        let mut context = Context::new();
        context.insert("totals", &totals(rows.len(), &checkpoints));
        context.insert("checkpoints", &checkpoints);
        context.insert("routes", &grouped::route_groups(&rows, false));
        context.insert("recent", &rows.iter().map(event_row).collect::<Vec<_>>());
        context.insert("include_fixtures", &false);
        decorate(&mut context, "Audit workbench");
        // Keys that `UiRegistry::render` injects at request time.
        for key in ["app_name", "environment", "csrf_token", "request_path"] {
            context.insert(key, "x");
        }
        context.insert("breadcrumbs", &Vec::<Breadcrumb>::new());
        context.insert("admin_navigation", &Vec::<Value>::new());
        context.insert("admin_actions", &Vec::<Value>::new());
        let html = registry_tera(&registry).render("audit/workbench.html", &context).expect("renders");
        assert!(html.contains("Audit workbench"));
        assert!(html.contains("evidence_assessment"));
        assert!(html.contains("run_full_verification-&gt;run_full_verification"));
        assert!(html.contains("reasoning_model"));
        assert!(html.contains("class=\"workbench\""));
        assert!(html.contains("/public/styles.css"));
        assert!(html.contains("Harness audit"));
        assert!(html.contains(">0.33<"), "median confidence is rounded for display");
        assert!(!html.contains("0.3299999"));
    }

    // `UiRegistry` keeps its Tera private; render the same template set directly
    // so the test checks the exact embedded sources.
    fn registry_tera(_registry: &UiRegistry) -> tera::Tera {
        let mut tera = tera::Tera::default();
        tera.add_raw_templates(HOST_TEMPLATES.iter().map(|def| (def.name.0, def.source)))
            .expect("embedded templates parse");
        tera
    }
}
