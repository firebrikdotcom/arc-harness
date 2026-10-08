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
use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use tera::Context;

const STYLESHEET: &str = include_str!("../public/styles.css");

const HOST_TEMPLATES: &[TemplateDef] = &[
    TemplateDef {
        name: TemplateName("components/client-time.html"),
        source: include_str!("../resources/views/components/client-time.html"),
    },
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
        name: TemplateName("audit/sessions.html"),
        source: include_str!("../resources/views/audit/sessions.html"),
    },
    TemplateDef {
        name: TemplateName("audit/workflow.html"),
        source: include_str!("../resources/views/audit/workflow.html"),
    },
    TemplateDef {
        name: TemplateName("audit/events.html"),
        source: include_str!("../resources/views/audit/events.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/prompt-items.html"),
        source: include_str!("../resources/views/audit/partials/prompt-items.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/task-items.html"),
        source: include_str!("../resources/views/audit/partials/task-items.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/task-detail.html"),
        source: include_str!("../resources/views/audit/partials/task-detail.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/todo-item.html"),
        source: include_str!("../resources/views/audit/partials/todo-item.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/timeline-items.html"),
        source: include_str!("../resources/views/audit/partials/timeline-items.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/signal-items.html"),
        source: include_str!("../resources/views/audit/partials/signal-items.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/flow.html"),
        source: include_str!("../resources/views/audit/partials/flow.html"),
    },
    TemplateDef {
        name: TemplateName("audit/partials/more.html"),
        source: include_str!("../resources/views/audit/partials/more.html"),
    },
];

/// Session lists open with a few rows and grow on request instead of rendering
/// every prompt, task and event up front.
const FIRST_ROWS: usize = 2;
const MORE_ROWS: usize = 5;
const FIRST_EVENTS: usize = 10;
/// Signal rows are one line each, so the curated timeline opens with more of them.
const FIRST_SIGNAL: usize = 25;
const MORE_EVENTS: usize = 20;
const MAX_PAGE: usize = 1000;

/// The flow diagram draws every block at once, so very long sessions keep the latest ones.
const FLOW_MAX: usize = 400;

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
        label: "Workflow",
        href: "/workflow",
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
    context.insert("client_timezone", &client_timezone(request).name());
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

fn client_timezone(request: &HttpRequest) -> Tz {
    request
        .cookie("audit_timezone")
        .and_then(|cookie| cookie.value().parse().ok())
        .unwrap_or(chrono_tz::UTC)
}

fn local_activity_date(value: &str, timezone: Tz) -> String {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&timezone).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn local_accuracy_days(
    rows: &[Value],
    include_fixtures: bool,
    timezone: Tz,
) -> Vec<grouped::DailyAccuracy> {
    let mut calendar_rows = rows.to_vec();
    // Shift checkpoint calendar coordinates only. Outcome ordering retains its original instants.
    for row in &mut calendar_rows {
        if row["event_type"] != "jev.checkpoint" {
            continue;
        }
        if let Some(date) = row["recorded_at_us"]
            .as_i64()
            .and_then(DateTime::<Utc>::from_timestamp_micros)
        {
            row["recorded_at_us"] = json!(date
                .with_timezone(&timezone)
                .naive_local()
                .and_utc()
                .timestamp_micros());
        }
    }
    grouped::daily_checkpoint_accuracy(&calendar_rows, include_fixtures)
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
        "recorded_at_us": recorded_at_us,
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

// Accuracy chart geometry, in SVG user units; the SVG scales to its panel.
const CHART_WIDTH: f64 = 720.0;
const CHART_HEIGHT: f64 = 220.0;
const CHART_LEFT: f64 = 48.0;
const CHART_RIGHT: f64 = 56.0;
const CHART_TOP: f64 = 16.0;
const CHART_BOTTOM: f64 = 32.0;
const MAX_DAY_TICKS: i64 = 10;

fn day_label(day: i64) -> String {
    format_timestamp(day * 86_400_000_000)[..10].to_owned()
}

/// Template-ready geometry for the daily accuracy line: one point per day with
/// labeled decisions, a path that breaks across days without labels, y
/// gridlines at 0/50/100%, and up to ten evenly spaced day ticks. Coordinates
/// are pre-formatted so the template does no arithmetic.
pub fn accuracy_chart(days: &[grouped::DailyAccuracy]) -> Value {
    let (Some(first), Some(last)) = (days.first(), days.last()) else {
        return json!({"points": [], "days": []});
    };
    let plot_width = CHART_WIDTH - CHART_LEFT - CHART_RIGHT;
    let plot_height = CHART_HEIGHT - CHART_TOP - CHART_BOTTOM;
    let span = last.day - first.day;
    let x = |day: i64| -> f64 {
        if span == 0 {
            CHART_LEFT + plot_width / 2.0
        } else {
            CHART_LEFT + (day - first.day) as f64 / span as f64 * plot_width
        }
    };
    let y = |fraction: f64| CHART_TOP + (1.0 - fraction) * plot_height;
    let mut path = String::new();
    let mut previous_day: Option<i64> = None;
    let points: Vec<Value> = days
        .iter()
        .map(|point| {
            let (px, py) = (x(point.day), y(point.correct as f64 / point.labeled as f64));
            let command = if previous_day == Some(point.day - 1) {
                'L'
            } else {
                'M'
            };
            path.push_str(&format!("{command}{px:.1},{py:.1}"));
            previous_day = Some(point.day);
            json!({
                "x": format!("{px:.1}"),
                "y": format!("{py:.1}"),
                "date": day_label(point.day),
                "labeled": point.labeled,
                "correct": point.correct,
                "percent": (point.correct * 100 + point.labeled / 2) / point.labeled,
            })
        })
        .collect();
    let gridlines: Vec<Value> = [(1.0, "100%"), (0.5, "50%"), (0.0, "0%")]
        .iter()
        .map(|(fraction, label)| json!({"y": format!("{:.1}", y(*fraction)), "label": label}))
        .collect();
    // A whole number of days between ticks keeps them evenly spaced; the
    // latest day is already named in the panel header when it falls between.
    let step = (span + MAX_DAY_TICKS - 2) / (MAX_DAY_TICKS - 1);
    let ticks: Vec<Value> = (0..=span)
        .step_by(step.max(1) as usize)
        .map(|offset| first.day + offset)
        .map(|day| json!({"x": format!("{:.1}", x(day)), "label": day_label(day)[5..].to_owned()}))
        .collect();
    json!({
        "width": CHART_WIDTH,
        "height": CHART_HEIGHT,
        "plot_left": CHART_LEFT,
        "plot_right": CHART_WIDTH - CHART_RIGHT,
        "axis_y": format!("{:.1}", CHART_HEIGHT - CHART_BOTTOM + 20.0),
        "path": path,
        "points": points,
        "gridlines": gridlines,
        "ticks": ticks,
        "latest": points.last(),
        "days": points.iter().rev().collect::<Vec<_>>(),
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
    let chart = accuracy_chart(&local_accuracy_days(
        &rows,
        query.include_fixtures,
        client_timezone(&request),
    ));
    let mut context = Context::new();
    context.insert("totals", &totals(rows.len(), &checkpoints));
    context.insert("accuracy_chart", &chart);
    context.insert("checkpoints", &checkpoints);
    context.insert("routes", &routes);
    context.insert("recent", &recent);
    context.insert("include_fixtures", &query.include_fixtures);
    let settings = match crate::settings::load() {
        Ok(settings) => settings,
        Err(error) => return unavailable(error),
    };
    context.insert("collection_settings", &settings);
    context.insert("workflow_sessions", &crate::workflow::sessions(&rows).len());
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
    let shown: Vec<Value> = matching
        .iter()
        .take(limit)
        .map(|row| event_row(row))
        .collect();
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

#[derive(Default, Deserialize, Serialize, Clone)]
struct WorkflowQuery {
    session_id: Option<String>,
    task_id: Option<String>,
    todo_id: Option<String>,
    return_to: Option<String>,
    q: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    status: Option<String>,
    path: Option<String>,
    phase: Option<String>,
    prompts: Option<String>,
    since: Option<String>,
    until: Option<String>,
    sort: Option<String>,
    page: Option<usize>,
    per_page: Option<usize>,
    show_prompts: Option<usize>,
    show_tasks: Option<usize>,
    show_events: Option<usize>,
    event_q: Option<String>,
    event_category: Option<String>,
    event_status: Option<String>,
    /// `all` shows every event with its evidence, `flow` draws the signal as a
    /// diagram; otherwise the signal list.
    view: Option<String>,
    /// `oldest` lists the timeline in source order; otherwise newest first.
    order: Option<String>,
    /// Directory quick filters: `needs`, `unfinished` or `verified`.
    attention: Option<String>,
    /// `show` lists sessions with no prompt, task, plan, check or decision.
    empty: Option<String>,
    /// `/workflow/items` only: which list to page and the slice to return.
    list: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    #[serde(skip)]
    timezone: Option<String>,
}

fn url_component(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn directory_url(query: &WorkflowQuery, page: usize) -> String {
    let mut fields = Vec::new();
    for key in [
        "q",
        "agent",
        "model",
        "status",
        "path",
        "phase",
        "prompts",
        "since",
        "until",
        "sort",
        "attention",
        "empty",
    ] {
        if let Some(value) = serde_json::to_value(query).unwrap()[key]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            fields.push(format!("{key}={}", url_component(value)));
        }
    }
    fields.push(format!(
        "per_page={}",
        query.per_page.unwrap_or(25).clamp(1, 100)
    ));
    fields.push(format!("page={page}"));
    format!("/workflow?{}", fields.join("&"))
}

fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = date[..4].parse().unwrap();
    let month: u32 = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    let max = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    year > 0 && day > 0 && day <= max
}

/// `Today · Oct 6`, `Yesterday · Oct 5`, or `Sat · Oct 3` for a local `YYYY-MM-DD`.
fn day_heading(day: &str, today: chrono::NaiveDate) -> String {
    let Ok(date) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
        return "Date unknown".into();
    };
    let prefix = match (today - date).num_days() {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        _ => date.format("%a").to_string(),
    };
    let year = if date.year() == today.year() {
        String::new()
    } else {
        date.format(", %Y").to_string()
    };
    format!("{prefix} · {}{year}", date.format("%b %-d"))
}

/// Share of todos done; sessions without todos rank below every plan.
fn progress_rank(session: &Value) -> (f64, u64) {
    let done = session["verdict"]["todos_done"].as_u64().unwrap_or(0);
    let total = session["verdict"]["todos_total"].as_u64().unwrap_or(0);
    if total == 0 {
        (-1.0, 0)
    } else {
        (done as f64 / total as f64, total)
    }
}

/// Failed checks first, then no checks, then partly and fully passed.
fn checks_rank(session: &Value) -> u8 {
    let checks = &session["verdict"]["checks"];
    let last = |key: &str| checks[key]["last"].as_str().unwrap_or("");
    let results = [last("verify"), last("review")];
    if results.contains(&"failed") {
        0
    } else if results == ["", ""] {
        1
    } else if results == ["passed", "passed"] {
        3
    } else {
        2
    }
}

fn session_directory(sessions: &[Value], query: &WorkflowQuery) -> Value {
    let clean = |value: &Option<String>| value.as_deref().unwrap_or("").trim().to_owned();
    let terms: Vec<String> = clean(&query.q)
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let path = clean(&query.path).to_lowercase();
    let since = clean(&query.since);
    let until = clean(&query.until);
    let timezone: Tz = query
        .timezone
        .as_deref()
        .and_then(|zone| zone.parse().ok())
        .unwrap_or(chrono_tz::UTC);
    let today = Utc::now().with_timezone(&timezone).date_naive();
    let mut errors = Vec::new();
    if (!since.is_empty() && !valid_date(&since)) || (!until.is_empty() && !valid_date(&until)) {
        errors.push("Use valid YYYY-MM-DD dates for the activity range.");
    }
    if !since.is_empty() && !until.is_empty() && since > until {
        errors.push("Activity from must be on or before activity through.");
    }
    let mut facets = serde_json::Map::new();
    for key in ["agent", "model", "status"] {
        let values: BTreeSet<&str> = sessions.iter().filter_map(|s| s[key].as_str()).collect();
        facets.insert(key.into(), json!(values));
    }
    let phases: BTreeSet<&str> = sessions
        .iter()
        .flat_map(|s| s["tasks"].as_object().into_iter().flat_map(|t| t.values()))
        .filter_map(|t| t["phase"].as_str().filter(|s| !s.is_empty()))
        .collect();
    facets.insert("phase".into(), json!(phases));
    let mut matching: Vec<Value> = sessions
        .iter()
        .filter(|s| {
            if !errors.is_empty() {
                return false;
            }
            if [
                (&query.agent, "agent"),
                (&query.model, "model"),
                (&query.status, "status"),
            ]
            .iter()
            .any(|(wanted, key)| {
                !clean(wanted).is_empty() && s[*key].as_str().unwrap_or("") != clean(wanted)
            }) {
                return false;
            }
            if !s["cwd"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(&path)
            {
                return false;
            }
            if !clean(&query.phase).is_empty()
                && !s["tasks"].as_object().is_some_and(|tasks| {
                    tasks
                        .values()
                        .any(|t| t["phase"].as_str().unwrap_or("") == clean(&query.phase))
                })
            {
                return false;
            }
            let verdict = &s["verdict"];
            match clean(&query.attention).as_str() {
                "needs" if verdict["attention"].as_array().is_none_or(Vec::is_empty) => {
                    return false
                }
                "unfinished" if verdict["unfinished"] != true => return false,
                "verified" if verdict["verified"] != true => return false,
                _ => {}
            }
            let prompt_count = s["prompts"].as_array().map_or(0, Vec::len);
            if (clean(&query.prompts) == "recorded" && prompt_count == 0)
                || (clean(&query.prompts) == "none" && prompt_count > 0)
            {
                return false;
            }
            let date = local_activity_date(s["last_at"].as_str().unwrap_or(""), timezone);
            if (!since.is_empty() && date.as_str() < since.as_str())
                || (!until.is_empty() && date.as_str() > until.as_str())
            {
                return false;
            }
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
                if let Some(text) = s[key].as_str() {
                    words.push(text);
                }
            }
            for task in s["tasks"]
                .as_object()
                .into_iter()
                .flat_map(|tasks| tasks.values())
            {
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
            for prompt in s["prompts"].as_array().into_iter().flatten() {
                if let Some(text) = prompt["text"].as_str() {
                    words.push(text);
                }
            }
            for event in s["events"].as_array().into_iter().flatten() {
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
            let searchable = words.join(" ").to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        })
        .cloned()
        .collect();
    // Sessions with nothing to review stay out of the list unless asked for.
    let show_empty = clean(&query.empty) == "show";
    let hidden_empty = if show_empty {
        0
    } else {
        let before = matching.len();
        matching.retain(|s| s["empty"] != true);
        before - matching.len()
    };
    for s in &mut matching {
        let tasks = s["tasks"].as_object().cloned().unwrap_or_default();
        s["task_count"] = json!(tasks.len());
        s["completed_tasks"] = json!(tasks
            .values()
            .filter(|t| t["status"] == "completed")
            .count());
        s["prompt_count"] = json!(s["prompts"].as_array().map_or(0, Vec::len));
        let description = s["events"]
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
            .unwrap_or_else(|| s["initial_goal"].as_str().unwrap_or(""));
        let preview: String = description.chars().take(180).collect();
        s["preview"] = json!(if description.chars().count() > 180 {
            format!("{preview}…")
        } else {
            preview
        });
        for key in ["first_at", "last_at"] {
            let value = s[key].as_str().unwrap_or("");
            s[format!("{key}_display")] = json!(value.get(..19).unwrap_or(value).replace('T', " "));
        }
        // One square per todo, capped so a long plan still fits the row.
        let (done, total) = (
            s["verdict"]["todos_done"].as_u64().unwrap_or(0),
            s["verdict"]["todos_total"].as_u64().unwrap_or(0),
        );
        let squares = total.min(8);
        let filled = (done * squares).checked_div(total).unwrap_or(0);
        s["todo_squares"] = json!((0..squares).map(|i| i < filled).collect::<Vec<_>>());
        s["local_day"] = json!(local_activity_date(
            s["last_at"].as_str().unwrap_or(""),
            timezone
        ));
    }
    let sort = clean(&query.sort);
    // Column sorts take a `_desc` suffix for the reverse order; `latest` is `oldest` reversed.
    let (key, descending) = match sort.as_str() {
        "" | "latest" => ("oldest", true),
        other => other
            .strip_suffix("_desc")
            .map_or((other, false), |key| (key, true)),
    };
    matching.sort_by(|a, b| {
        let comparison = match key {
            "oldest" => a["last_at"].as_str().cmp(&b["last_at"].as_str()),
            "name" => a["label"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b["label"].as_str().unwrap_or("").to_lowercase()),
            "agent" | "model" | "status" => a[key].as_str().cmp(&b[key].as_str()),
            "path" => a["cwd"].as_str().cmp(&b["cwd"].as_str()),
            "project" => {
                let project = |s: &Value| s["project"].as_str().unwrap_or("").to_lowercase();
                project(a)
                    .cmp(&project(b))
                    .then(a["cwd"].as_str().cmp(&b["cwd"].as_str()))
            }
            "progress" => progress_rank(a)
                .partial_cmp(&progress_rank(b))
                .unwrap_or(std::cmp::Ordering::Equal),
            "checks" => checks_rank(a).cmp(&checks_rank(b)),
            "tasks" => b["task_count"].as_u64().cmp(&a["task_count"].as_u64()),
            "prompts" => b["prompt_count"].as_u64().cmp(&a["prompt_count"].as_u64()),
            _ => b["last_at"].as_str().cmp(&a["last_at"].as_str()),
        };
        let comparison = if descending {
            comparison.reverse()
        } else {
            comparison
        };
        comparison.then(a["session_id"].as_str().cmp(&b["session_id"].as_str()))
    });
    let total = matching.len();
    let per_page = query.per_page.unwrap_or(25).clamp(1, 100);
    let pages = total.div_ceil(per_page).max(1);
    let page = query.page.unwrap_or(1).clamp(1, pages);
    let offset = (page - 1) * per_page;
    let mut rows: Vec<Value> = matching.into_iter().skip(offset).take(per_page).collect();
    // Time-ordered lists read by day; other sorts would scatter the headings.
    if matches!(sort.as_str(), "" | "latest" | "oldest") {
        let mut previous = String::new();
        for row in &mut rows {
            let day = row["local_day"].as_str().unwrap_or("").to_owned();
            if day != previous {
                row["day_heading"] = json!(day_heading(&day, today));
                previous = day;
            }
        }
    }
    let reviewable: Vec<&Value> = sessions.iter().filter(|s| s["empty"] != true).collect();
    let count = |test: &dyn Fn(&Value) -> bool| reviewable.iter().filter(|s| test(s)).count();
    let today_text = today.format("%Y-%m-%d").to_string();
    let pulse = json!({
        "running": count(&|s| s["status"] == "running"),
        "needs": count(&|s| s["verdict"]["attention"].as_array().is_some_and(|a| !a.is_empty())),
        "unfinished": count(&|s| s["verdict"]["unfinished"] == true),
        "verified_today": count(&|s| s["verdict"]["verified"] == true
            && local_activity_date(s["last_at"].as_str().unwrap_or(""), timezone) == today_text),
    });
    let advanced = [
        "agent", "model", "status", "path", "phase", "prompts", "since", "until",
    ]
    .iter()
    .any(|key| {
        !clean(
            &serde_json::to_value(query).unwrap()[*key]
                .as_str()
                .map(str::to_owned),
        )
        .is_empty()
    }) || query.per_page.is_some_and(|n| n != 25);
    let quick = |field: &str, value: &str| {
        let mut next = query.clone();
        next.attention = None;
        next.status = None;
        match field {
            "attention" => next.attention = Some(value.into()),
            "status" => next.status = Some(value.into()),
            _ => {}
        }
        directory_url(&next, 1)
    };
    // Column titles sort on click; a second click on the active column reverses it.
    let columns: Vec<Value> = [
        ("Session", "name", "name_desc", false),
        ("Project", "project", "project_desc", false),
        ("Progress", "progress", "progress_desc", true),
        ("Checks", "checks", "checks_desc", false),
        ("Last active", "oldest", "latest", true),
    ]
    .iter()
    .map(|&(label, ascending, reversed, first_descending)| {
        let current = if sort.is_empty() { "latest" } else { sort.as_str() };
        let active = current == ascending || current == reversed;
        let next = if active {
            if current == ascending { reversed } else { ascending }
        } else if first_descending {
            reversed
        } else {
            ascending
        };
        let mut target = query.clone();
        target.sort = Some(next.into());
        json!({"label": label, "href": directory_url(&target, 1), "active": active,
            "direction": if !active { "" } else if current == reversed { "descending" } else { "ascending" }})
    })
    .collect();
    let mut with_empty = query.clone();
    with_empty.empty = Some("show".into());
    let filters = json!({"q":clean(&query.q),"agent":clean(&query.agent),"model":clean(&query.model),"status":clean(&query.status),"path":clean(&query.path),"phase":clean(&query.phase),"prompts":clean(&query.prompts),"since":since,"until":until,"sort":if sort.is_empty(){"latest"}else{&sort},"per_page":per_page,"attention":clean(&query.attention)});
    let chips = json!([
        {"label": "Needs attention", "count": pulse["needs"], "href": quick("attention", "needs"), "active": clean(&query.attention) == "needs"},
        {"label": "Running", "count": pulse["running"], "href": quick("status", "running"), "active": clean(&query.status) == "running" && clean(&query.attention).is_empty()},
        {"label": "Ended unfinished", "count": pulse["unfinished"], "href": quick("attention", "unfinished"), "active": clean(&query.attention) == "unfinished"},
        {"label": "Verified", "count": Value::Null, "href": quick("attention", "verified"), "active": clean(&query.attention) == "verified"},
    ]);
    json!({"rows":rows,"columns":columns,"total":total,"all_count":sessions.len(),"pulse":pulse,"chips":chips,"advanced":advanced,"hidden_empty":hidden_empty,"show_empty_url":directory_url(&with_empty,1),"show_empty":show_empty,"facets":facets,"filters":filters,"errors":errors,"page":page,"pages":pages,"start":if total==0{0}else{offset+1},"end":(offset+per_page).min(total),"url":directory_url(query,page),"previous":if page>1{directory_url(query,page-1)}else{String::new()},"next":if page<pages{directory_url(query,page+1)}else{String::new()}})
}

#[get("/workflow")]
async fn workflow_page(
    request: HttpRequest,
    session: Session,
    query: web::Query<WorkflowQuery>,
    store: web::Data<dyn ReadModelStore>,
    registry: web::Data<UiRegistry>,
) -> impl Responder {
    let rows = match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => rows,
        Err(error) => return unavailable(error),
    };
    let mut query = query.into_inner();
    query.timezone = Some(client_timezone(&request).name().to_owned());
    let sessions = crate::workflow::sessions(&rows);
    if query.session_id.is_none() {
        let mut context = Context::new();
        context.insert("directory", &session_directory(&sessions, &query));
        return render(
            &registry,
            &request,
            &session,
            "audit/sessions.html",
            "Workflow sessions",
            context,
            vec![
                Breadcrumb::link("Workbench", "/"),
                Breadcrumb::current("Workflow"),
            ],
        );
    }
    let selected = query
        .session_id
        .as_deref()
        .and_then(|id| sessions.iter().find(|s| s["session_id"] == id));
    let mut context = Context::new();
    context.insert("sessions", &!sessions.is_empty());
    context.insert("selected", &selected);
    if let Some(session) = selected {
        session_view(session, &query, &mut context);
    }
    render(
        &registry,
        &request,
        &session,
        "audit/workflow.html",
        "Workflow session",
        context,
        vec![
            Breadcrumb::link("Workbench", "/"),
            Breadcrumb::link("Workflow sessions", "/workflow"),
            Breadcrumb::current("Session details"),
        ],
    )
}

fn trimmed(value: &Option<String>) -> String {
    value.as_deref().unwrap_or("").trim().to_owned()
}

fn return_to(query: &WorkflowQuery) -> &str {
    query
        .return_to
        .as_deref()
        .filter(|s| *s == "/workflow" || s.starts_with("/workflow?"))
        .unwrap_or("/workflow")
}

/// A session-page URL that keeps the selected session, task scope and timeline
/// filters, plus `extra` fields such as list sizes or a fragment slice.
fn session_url(query: &WorkflowQuery, base: &str, extra: &[(&str, String)]) -> String {
    let scope = [
        ("session_id", &query.session_id),
        ("task_id", &query.task_id),
        ("todo_id", &query.todo_id),
        ("event_q", &query.event_q),
        ("event_category", &query.event_category),
        ("event_status", &query.event_status),
        ("view", &query.view),
        ("order", &query.order),
    ];
    let fields: Vec<String> = scope
        .iter()
        .map(|(key, value)| (*key, trimmed(value)))
        .chain(extra.iter().cloned())
        .chain([("return_to", return_to(query).to_owned())])
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key}={}", url_component(&value)))
        .collect();
    format!("{base}?{}", fields.join("&"))
}

/// Timeline events for the selected task or todo that match the search words,
/// category and result. Filtering here keeps counts and paging true for the
/// whole session rather than only the rows already on the page.
fn timeline<'a>(session: &'a Value, query: &WorkflowQuery) -> Vec<&'a Value> {
    let words: Vec<String> = trimmed(&query.event_q)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let category = trimmed(&query.event_category);
    let status = trimmed(&query.event_status);
    session["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| {
            let p = &e["payload"];
            // A long prompt arrives in parts; the timeline shows it once.
            (e["event_type"] != "workflow.prompt_recorded"
                || p["part_index"].as_u64().unwrap_or(0) == 0)
                && query.task_id.as_deref().is_none_or(|id| p["task_id"] == id)
                && query.todo_id.as_deref().is_none_or(|id| p["todo_id"] == id)
                && (category.is_empty() || e["category"] == category.as_str())
                && (status.is_empty() || e["result_status"] == status.as_str())
        })
        .filter(|e| {
            if words.is_empty() {
                return true;
            }
            let mut text = String::new();
            for key in [
                "title",
                "task_name",
                "explanation",
                "result_status",
                "event_type",
            ] {
                text.push_str(e[key].as_str().unwrap_or(""));
                text.push(' ');
            }
            for (key, value) in e["payload"].as_object().into_iter().flatten() {
                match value {
                    Value::String(s) if key != "text" => text.push_str(s),
                    Value::Number(n) => text.push_str(&n.to_string()),
                    Value::Array(items) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .for_each(|s| text.push_str(s)),
                    _ => {}
                }
                text.push(' ');
            }
            let text = text.to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .collect()
}

/// The list-size field that keeps a list's length across a full page load.
fn show_key(list: &str) -> (&'static str, &'static str, usize) {
    match list {
        "prompts" => ("show_prompts", "prompts", MORE_ROWS),
        "tasks" => ("show_tasks", "tasks", MORE_ROWS),
        _ => ("show_events", "events", MORE_EVENTS),
    }
}

/// The "Showing n of m · Show more" control under a list. `url` reloads the page
/// with a longer list when scripts are off; `src` returns just the next rows.
fn more(query: &WorkflowQuery, list: &str, shown: usize, total: usize) -> Value {
    let (key, noun, step) = show_key(list);
    let shown = shown.min(total);
    let next = (shown + step).min(total);
    let mut sizes: Vec<(&str, String)> = [
        ("show_prompts", query.show_prompts),
        ("show_tasks", query.show_tasks),
        ("show_events", query.show_events),
    ]
    .into_iter()
    .filter(|(field, _)| *field != key)
    .filter_map(|(field, size)| size.map(|size| (field, size.to_string())))
    .collect();
    sizes.push((key, next.to_string()));
    let slice = [
        ("list", list.to_owned()),
        ("offset", shown.to_string()),
        ("limit", step.to_string()),
    ];
    json!({
        "list": list, "noun": noun, "shown": shown, "total": total, "step": next - shown,
        "url": format!("{}#{list}", session_url(query, "/workflow", &sizes)),
        "src": session_url(query, "/workflow/items", &slice),
    })
}

/// The signal view folds tool calls and drops bookkeeping; `view=all` keeps every event.
fn signal_view(query: &WorkflowQuery) -> bool {
    trimmed(&query.view) != "all"
}

/// Timeline rows for the current view, after the task, todo and search filters.
fn timeline_rows(session: &Value, query: &WorkflowQuery) -> Vec<Value> {
    let events: Vec<Value> = timeline(session, query).into_iter().cloned().collect();
    let rows = if signal_view(query) {
        crate::workflow::signal_items(&events)
    } else {
        events
    };
    match (newest_first(query), signal_view(query)) {
        (false, _) => rows,
        (true, true) => crate::workflow::newest_first(rows),
        (true, false) => rows.into_iter().rev().collect(),
    }
}

/// `view=flow` draws the signal timeline as a diagram of blocks per prompt.
fn flow_view(query: &WorkflowQuery) -> bool {
    trimmed(&query.view) == "flow"
}

/// The icon and kind label that tell a flow block's event type at a glance.
fn flow_icon(item: &Value) -> (&'static str, &'static str) {
    let kind = item["event_type"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches("workflow.");
    match (
        item["category"].as_str().unwrap_or(""),
        item["tone"].as_str(),
    ) {
        ("burst", _) => ("tools", "Tool calls"),
        ("prompt", _) => ("prompt", "Prompt"),
        ("tool", _) => ("tool-fail", "Tool failure"),
        ("check", Some("ok")) => ("check-ok", "Check"),
        ("check", Some("fail")) => ("check-fail", "Check"),
        ("check", _) => ("check", "Check"),
        ("decision", _) => ("decision", "Decision"),
        ("todo", _) => ("todo", "Todo"),
        ("plan", _) => ("plan", "Plan"),
        ("phase", _) => ("phase", "Phase"),
        ("task", _) if kind == "task_completed" => ("task-done", "Task"),
        ("task", _) if kind == "task_blocked" => ("task-blocked", "Task"),
        ("task", _) => ("task", "Task"),
        ("session", _) => ("session", "Session"),
        _ => ("event", "Event"),
    }
}

/// MCP tools read as `mcp__server__tool`; the diagram shows the tool part.
fn short_tool(name: &str) -> &str {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.rsplit("__").next().unwrap_or(rest),
        None => name,
    }
}

/// A flow block's recorded fields for the inspector, without identifiers every
/// event of the session shares and without empty values.
fn flow_fields(payload: &Value) -> Vec<Value> {
    payload
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !matches!(key.as_str(), "session_id" | "schema_version"))
        .filter_map(|(key, value)| {
            let text = match value {
                Value::Null | Value::Object(_) => return None,
                Value::String(text) if text.trim().is_empty() => return None,
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            Some(json!({"key": key.replace('_', " "), "value": text}))
        })
        .collect()
}

/// The signal timeline as a diagram: one lane per prompt, its blocks in the
/// order they happened. Newest first puts the latest lane on top; blocks inside
/// a lane always read left to right. Events before the first prompt form a
/// lane without a prompt.
fn flow_lanes(session: &Value, query: &WorkflowQuery) -> Value {
    let events: Vec<Value> = timeline(session, query).into_iter().cloned().collect();
    let mut items = crate::workflow::signal_items(&events);
    let total = items.len();
    let hidden = total.saturating_sub(FLOW_MAX);
    items.drain(..hidden);
    let mut lanes: Vec<Value> = Vec::new();
    for (index, mut item) in items.into_iter().enumerate() {
        let (icon, kind) = flow_icon(&item);
        item["flow_id"] = json!(format!("flow-{index}"));
        item["icon"] = json!(icon);
        item["kind"] = json!(kind);
        item["fields"] = json!(flow_fields(&item["payload"]));
        for list in ["tools", "call_list"] {
            let key = if list == "tools" { "name" } else { "tool" };
            for entry in item[list].as_array_mut().into_iter().flatten() {
                let short = short_tool(entry[key].as_str().unwrap_or("")).to_owned();
                entry["short"] = json!(short);
            }
        }
        if item["category"] == "prompt" || lanes.is_empty() {
            lanes.push(json!({"prompt": Value::Null, "blocks": []}));
        }
        let lane = lanes.last_mut().unwrap();
        if item["category"] == "prompt" {
            lane["prompt"] = item.clone();
        }
        lane["blocks"].as_array_mut().unwrap().push(item);
    }
    if newest_first(query) {
        lanes.reverse();
    }
    json!({"lanes": lanes, "total": total, "hidden": hidden})
}

/// The latest events lead unless `order=oldest` asks for source order.
fn newest_first(query: &WorkflowQuery) -> bool {
    trimmed(&query.order) != "oldest"
}

/// The link that flips the timeline order, keeping the view and filters.
fn order_toggle(query: &WorkflowQuery) -> Value {
    let next = WorkflowQuery {
        order: newest_first(query).then(|| "oldest".to_owned()),
        show_events: None,
        ..query.clone()
    };
    json!({
        "label": if newest_first(query) { "Oldest first" } else { "Newest first" },
        "current": if newest_first(query) { "newest first" } else { "oldest first" },
        "href": format!("{}#timeline", session_url(&next, "/workflow", &[])),
    })
}

fn timeline_more(query: &WorkflowQuery, shown: usize, total: usize) -> Value {
    let mut more = more(query, "timeline", shown, total);
    if signal_view(query) {
        more["noun"] = json!("entries");
    }
    more
}

/// Quick timeline views: the curated signal, every raw event, and one-click
/// filters for failures, decisions and checks.
fn timeline_chips(session: &Value, query: &WorkflowQuery) -> Value {
    let category = trimmed(&query.event_category);
    let status = trimmed(&query.event_status);
    let unfiltered = category.is_empty() && status.is_empty() && trimmed(&query.event_q).is_empty();
    let verdict = &session["verdict"];
    let checks: u64 = verdict["checks"]
        .as_object()
        .into_iter()
        .flat_map(|c| c.values())
        .filter_map(|c| c["total"].as_u64())
        .sum();
    let failures = session["analysis"]["failed"].as_u64().unwrap_or(0)
        + session["analysis"]["check_failures"].as_u64().unwrap_or(0);
    // Failure, decision and check filters keep the view they were chosen from.
    let current = match trimmed(&query.view).as_str() {
        "all" => Some("all"),
        "flow" => Some("flow"),
        _ => None,
    };
    let link = |view: Option<&str>, category: Option<&str>, status: Option<&str>| {
        let next = WorkflowQuery {
            view: view.map(Into::into),
            event_category: category.map(Into::into),
            event_status: status.map(Into::into),
            event_q: None,
            show_events: None,
            ..query.clone()
        };
        format!("{}#timeline", session_url(&next, "/workflow", &[]))
    };
    json!([
        {"label": "Signal", "href": link(None, None, None), "active": signal_view(query) && !flow_view(query) && unfiltered, "count": Value::Null},
        {"label": "Flow", "href": link(Some("flow"), None, None), "active": flow_view(query) && unfiltered, "count": Value::Null},
        {"label": "Raw events", "href": link(Some("all"), None, None), "active": !signal_view(query) && unfiltered, "count": session["events"].as_array().map_or(0, Vec::len)},
        {"label": "Failures", "href": link(current, None, Some("failed")), "active": status == "failed", "count": failures},
        {"label": "Decisions", "href": link(current, Some("decision"), None), "active": category == "decision", "count": verdict["decisions"]},
        {"label": "Checks", "href": link(current, Some("check"), None), "active": category == "check", "count": checks},
    ])
}

fn page(items: &[Value], offset: usize, limit: usize) -> &[Value] {
    let start = offset.min(items.len());
    &items[start..(start + limit).min(items.len())]
}

fn session_view(session: &Value, query: &WorkflowQuery, context: &mut Context) {
    let size =
        |requested: Option<usize>, first: usize| requested.unwrap_or(first).clamp(1, MAX_PAGE);
    let prompts = session["prompts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let tasks = session["task_list"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let expanded = query
        .task_id
        .as_deref()
        .and_then(|id| tasks.iter().find(|t| t["task_id"] == id));
    // A task opened from a link stays visible even when it is past the first rows.
    let task_rows = size(query.show_tasks, FIRST_ROWS)
        .max(expanded.and_then(|t| t["sequence"].as_u64()).unwrap_or(0) as usize);
    let prompt_rows = size(query.show_prompts, FIRST_ROWS);
    let first_events = if signal_view(query) {
        FIRST_SIGNAL
    } else {
        FIRST_EVENTS
    };
    let event_rows = size(query.show_events, first_events);
    let events = timeline_rows(session, query);
    context.insert("prompt_page", page(prompts, 0, prompt_rows));
    context.insert(
        "prompts_more",
        &more(query, "prompts", prompt_rows, prompts.len()),
    );
    context.insert("task_page", page(tasks, 0, task_rows));
    context.insert("tasks_more", &more(query, "tasks", task_rows, tasks.len()));
    context.insert("expanded_task", &expanded);
    context.insert("timeline_page", page(&events, 0, event_rows));
    context.insert(
        "timeline_more",
        &timeline_more(query, event_rows, events.len()),
    );
    context.insert("signal_view", &signal_view(query));
    context.insert("flow_view", &flow_view(query));
    if flow_view(query) {
        context.insert("flow", &flow_lanes(session, query));
    }
    context.insert("timeline_chips", &timeline_chips(session, query));
    context.insert("order_toggle", &order_toggle(query));
    // Every planned todo of the session, newest task last, for the side column.
    let todo_rows: Vec<Value> = tasks
        .iter()
        .flat_map(|task| {
            task["todo_list"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|todo| todo["status"] != "removed")
                .map(|todo| {
                    let mut row = todo.clone();
                    row["task_id"] = task["task_id"].clone();
                    row
                })
        })
        .collect();
    context.insert("todo_rows", &todo_rows);
    context.insert(
        "named_tasks",
        &tasks.iter().filter(|t| t["name"] != "Session work").count(),
    );
    context.insert("task_filter", &query.task_id);
    context.insert("todo_filter", &query.todo_id);
    context.insert(
        "event_filters",
        &json!({"q": trimmed(&query.event_q), "category": trimmed(&query.event_category), "status": trimmed(&query.event_status)}),
    );
    context.insert("return_to", return_to(query));
}

/// Embedded templates for list fragments, which render without the page layout.
fn fragments() -> &'static tera::Tera {
    static TERA: std::sync::OnceLock<tera::Tera> = std::sync::OnceLock::new();
    TERA.get_or_init(|| {
        let mut tera = tera::Tera::default();
        tera.add_raw_templates(HOST_TEMPLATES.iter().map(|def| (def.name.0, def.source)))
            .expect("embedded templates parse");
        tera
    })
}

/// Rows for one session list, returned as `{html, more_html}` so the page can
/// append them in place. `list=task` returns one task's details instead.
fn workflow_fragment(session: &Value, query: &WorkflowQuery) -> Result<Value, String> {
    let list = query.list.as_deref().unwrap_or("");
    let offset = query.offset.unwrap_or(0);
    let mut context = Context::new();
    context.insert("selected", session);
    context.insert("return_to", return_to(query));
    context.insert("expanded_task", &Value::Null);
    context.insert(
        "named_tasks",
        &session["task_list"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["name"] != "Session work")
            .count(),
    );
    let render = |name: &str, context: &Context| {
        fragments()
            .render(name, context)
            .map_err(|error| format!("{error:?}"))
    };
    if list == "task" {
        let task = session["task_list"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| {
                query
                    .task_id
                    .as_deref()
                    .is_some_and(|id| t["task_id"] == id)
            })
            .ok_or("task not found")?;
        context.insert("expanded_task", task);
        return Ok(
            json!({"html": render("audit/partials/task-detail.html", &context)?, "more_html": ""}),
        );
    }
    let (template, field, items): (&str, &str, Vec<Value>) = match list {
        "prompts" => (
            "prompt-items",
            "prompt_page",
            session["prompts"].as_array().cloned().unwrap_or_default(),
        ),
        "tasks" => (
            "task-items",
            "task_page",
            session["task_list"].as_array().cloned().unwrap_or_default(),
        ),
        "timeline" => (
            if signal_view(query) {
                "signal-items"
            } else {
                "timeline-items"
            },
            "timeline_page",
            timeline_rows(session, query),
        ),
        _ => return Err("unknown list".to_owned()),
    };
    // A list reloaded from the start (a new timeline filter) opens at its first size.
    let (_, _, step) = show_key(list);
    let first = match list {
        "timeline" if signal_view(query) => FIRST_SIGNAL,
        "timeline" => FIRST_EVENTS,
        _ => FIRST_ROWS,
    };
    let rows = page(
        &items,
        offset,
        query
            .limit
            .unwrap_or(if offset == 0 { first } else { step })
            .clamp(1, MAX_PAGE),
    );
    context.insert(field, rows);
    let shown = offset + rows.len();
    context.insert(
        "more",
        &if list == "timeline" {
            timeline_more(query, shown, items.len())
        } else {
            more(query, list, shown, items.len())
        },
    );
    Ok(json!({
        "html": render(&format!("audit/partials/{template}.html"), &context)?,
        "more_html": render("audit/partials/more.html", &context)?,
    }))
}

#[get("/workflow/items")]
async fn workflow_items(
    query: web::Query<WorkflowQuery>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    let rows = match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => rows,
        Err(error) => return unavailable(error),
    };
    let sessions = crate::workflow::sessions(&rows);
    let Some(session) = query
        .session_id
        .as_deref()
        .and_then(|id| sessions.iter().find(|s| s["session_id"] == id))
    else {
        return HttpResponse::NotFound().json(json!({"error": "session not found"}));
    };
    match workflow_fragment(session, &query) {
        Ok(body) => HttpResponse::Ok().json(body),
        Err(error) => HttpResponse::BadRequest().json(json!({"error": error})),
    }
}

#[get("/public/styles.css")]
async fn stylesheet() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/css; charset=utf-8")
        .insert_header(("Cache-Control", "public, max-age=300"))
        .body(STYLESHEET)
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(workbench)
        .service(event_log)
        .service(workflow_page)
        .service(workflow_items)
        .service(stylesheet);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(
        call: &str,
        family: &str,
        baseline: &str,
        recommendation: &str,
        at: i64,
    ) -> Value {
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
    fn client_calendar_handles_midnight_dst_and_keeps_stored_instants() {
        let timezone = chrono_tz::America::New_York;
        assert_eq!(
            local_activity_date("2026-03-08T04:30:00Z", timezone),
            "2026-03-07"
        );
        assert_eq!(
            local_activity_date("2026-03-08T07:30:00Z", timezone),
            "2026-03-08"
        );
        let micros = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .timestamp_micros()
        };
        let rows = vec![
            checkpoint("a", "test", "run", "run", micros("2026-03-08T04:30:00Z")),
            checkpoint("b", "test", "run", "run", micros("2026-03-08T07:30:00Z")),
            outcome("a", "correct", micros("2026-03-08T08:00:00Z")),
            outcome("b", "correct", micros("2026-03-08T08:01:00Z")),
        ];
        let original = rows.clone();
        let chart = accuracy_chart(&local_accuracy_days(&rows, false, timezone));
        assert_eq!(chart["points"][0]["date"], "2026-03-07");
        assert_eq!(chart["points"][1]["date"], "2026-03-08");
        assert_eq!(rows, original);
        let sessions = vec![directory_fixture("s", "2026-10-02", "Codex", false)];
        let query = WorkflowQuery {
            since: Some("2026-10-01".into()),
            until: Some("2026-10-01".into()),
            timezone: Some("America/New_York".into()),
            ..Default::default()
        };
        assert_eq!(session_directory(&sessions, &query)["total"], 1);
        assert_eq!(
            session_directory(
                &sessions,
                &WorkflowQuery {
                    timezone: Some("Asia/Kolkata".into()),
                    ..query
                }
            )["total"],
            0
        );
        let before = DateTime::parse_from_rfc3339("2026-03-08T06:59:00Z")
            .unwrap()
            .with_timezone(&timezone);
        let after = DateTime::parse_from_rfc3339("2026-03-08T07:00:00Z")
            .unwrap()
            .with_timezone(&timezone);
        assert_eq!(before.format("%H:%M %Z").to_string(), "01:59 EST");
        assert_eq!(after.format("%H:%M %Z").to_string(), "03:00 EDT");
    }

    #[test]
    fn timezone_cookie_is_validated() {
        use actix_web::{cookie::Cookie, test::TestRequest};
        let request = TestRequest::default()
            .cookie(Cookie::new("audit_timezone", "Asia/Kolkata"))
            .to_http_request();
        assert_eq!(client_timezone(&request), chrono_tz::Asia::Kolkata);
        let invalid = TestRequest::default()
            .cookie(Cookie::new("audit_timezone", "invalid"))
            .to_http_request();
        assert_eq!(client_timezone(&invalid), chrono_tz::UTC);
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
        let row = event_row(&checkpoint(
            "abc",
            "evidence_assessment",
            "run",
            "fix",
            1_790_629_585_000_000,
        ));
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
        assert_eq!(
            format_timestamp(951_782_400_000_000),
            "2000-02-29 00:00:00Z"
        );
        assert_eq!(format_timestamp(-1_000_000), "1969-12-31 23:59:59Z");
    }

    #[test]
    fn workbench_renders_summary_tables_with_the_arc_layout() {
        let rows = vec![
            checkpoint(
                "a",
                "evidence_assessment",
                "run_full_verification",
                "run_full_verification",
                1,
            ),
            outcome("a", "correct", 2),
            json!({
                "id": "r1", "event_type": "jev.route", "recorded_at_us": 3,
                "payload": {"call_id": "r1", "source": "typesafe", "routing_mode": "shadow",
                            "model_requested": "jev-1.13.0", "model_returned": "jev-1.13.0",
                            "observed_recommendation": "reasoning_model", "jev_latency_ms": 190}
            }),
        ];
        let html = render_workbench(&rows);
        assert!(html.contains("Audit workbench"));
        assert!(html.contains("evidence_assessment"));
        assert!(html.contains("run_full_verification-&gt;run_full_verification"));
        assert!(html.contains("reasoning_model"));
        assert!(html.contains("class=\"workbench\""));
        assert!(html.contains("/public/styles.css"));
        assert!(html.contains("Harness audit"));
        assert!(
            html.contains(">0.33<"),
            "median confidence is rounded for display"
        );
        assert!(!html.contains("0.3299999"));
        let chart = html
            .find("Accuracy over time")
            .expect("chart panel renders");
        assert!(
            chart < html.find("metric-grid").unwrap(),
            "chart sits above the metric grid"
        );
        assert!(html.contains("<path class=\"chart__line\" d=\"M356.0,16.0\"/>"));
        assert!(html.contains("1970-01-01: 100% (1 correct of 1 labeled)"));
    }

    #[test]
    fn workbench_shows_an_empty_chart_state_without_labels() {
        let html = render_workbench(&[checkpoint("a", "evidence_assessment", "run", "run", 1)]);
        assert!(html.contains("No labeled decisions yet"));
        assert!(!html.contains("<svg class=\"chart\""));
    }

    #[test]
    fn accuracy_chart_breaks_the_line_on_days_without_labels() {
        let day = |day, labeled, correct| grouped::DailyAccuracy {
            day,
            labeled,
            correct,
        };
        let chart = accuracy_chart(&[day(100, 4, 2), day(101, 2, 2), day(110, 3, 0)]);
        // 0% sits on the plot floor (y=188), 50% midway, 100% on the top line.
        assert_eq!(chart["path"], "M48.0,102.0L109.6,16.0M664.0,188.0");
        assert_eq!(chart["points"][0]["percent"], 50);
        assert_eq!(chart["latest"]["date"], "1970-04-21");
        assert_eq!(
            chart["days"][0]["date"], "1970-04-21",
            "table lists newest first"
        );
        let ticks: Vec<&str> = chart["ticks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            ticks,
            ["04-11", "04-13", "04-15", "04-17", "04-19", "04-21"]
        );
        let nine_days = accuracy_chart(&[day(0, 1, 1), day(8, 1, 1)]);
        assert_eq!(
            nine_days["ticks"].as_array().unwrap().len(),
            9,
            "short spans tick every day"
        );
        let single = accuracy_chart(&[day(5, 3, 1)]);
        assert_eq!(single["path"], "M356.0,130.7");
        assert_eq!(single["ticks"].as_array().unwrap().len(), 1);
        assert_eq!(accuracy_chart(&[])["points"], json!([]));
    }

    fn render_workbench(rows: &[Value]) -> String {
        let registry = UiRegistry::build(Some(&host()), &[contribution()])
            .unwrap()
            .unwrap();
        let checkpoints = grouped::checkpoint_groups(rows, false);
        let mut context = Context::new();
        context.insert("totals", &totals(rows.len(), &checkpoints));
        context.insert(
            "accuracy_chart",
            &accuracy_chart(&grouped::daily_checkpoint_accuracy(rows, false)),
        );
        context.insert("checkpoints", &checkpoints);
        context.insert("routes", &grouped::route_groups(rows, false));
        context.insert("recent", &rows.iter().map(event_row).collect::<Vec<_>>());
        context.insert("include_fixtures", &false);
        context.insert(
            "collection_settings",
            &crate::settings::CollectionSettings::default(),
        );
        context.insert("workflow_sessions", &0);
        decorate(&mut context, "Audit workbench");
        // Keys that `UiRegistry::render` injects at request time.
        for key in ["app_name", "environment", "csrf_token", "request_path"] {
            context.insert(key, "x");
        }
        context.insert("breadcrumbs", &Vec::<Breadcrumb>::new());
        context.insert("admin_navigation", &Vec::<Value>::new());
        context.insert("admin_actions", &Vec::<Value>::new());
        registry_tera(&registry)
            .render("audit/workbench.html", &context)
            .expect("renders")
    }

    fn directory_fixture(id: &str, date: &str, agent: &str, prompt: bool) -> Value {
        json!({"session_id":id,"label":format!("Repair {id}"),"agent":agent,"model":"model-v1","cwd":"/Projects/Working Tree","status":"running","first_at":format!("{date}T01:00:00Z"),"last_at":format!("{date}T02:00:00Z"),"tasks":{"t":{"name":"Repair parser","description":"Curated summary","status":"completed","outcome":"Regression passed","phase":"build"}},"prompts":if prompt{vec![json!({"text":"Fix Unicode imports"})]}else{vec![]},"initial_goal":"","events":[],"project":"Working Tree","worktree":false,"empty":false,"verdict":{"outcome":{"status":"completed","text":"Regression passed"},"checks":{},"todos_done":0,"todos_total":0,"attention":[],"unfinished":false,"verified":false,"duration":"1h 0m"}})
    }

    #[test]
    fn directory_filters_search_across_metadata_tasks_outcomes_and_prompts() {
        let data = vec![
            directory_fixture("a", "2026-10-02", "Codex", true),
            directory_fixture("b", "2026-10-01", "Claude Code", false),
        ];
        let query = WorkflowQuery {
            q: Some("unicode regression".into()),
            agent: Some("Codex".into()),
            model: Some("model-v1".into()),
            path: Some("WORKING tree".into()),
            status: Some("running".into()),
            phase: Some("build".into()),
            prompts: Some("recorded".into()),
            since: Some("2026-10-02".into()),
            until: Some("2026-10-02".into()),
            ..Default::default()
        };
        let directory = session_directory(&data, &query);
        assert_eq!(directory["total"], 1);
        assert_eq!(directory["rows"][0]["session_id"], "a");
        assert_eq!(directory["rows"][0]["completed_tasks"], 1);
        assert_eq!(directory["facets"]["agent"].as_array().unwrap().len(), 2);
        let none = session_directory(
            &data,
            &WorkflowQuery {
                q: Some("missing term".into()),
                ..Default::default()
            },
        );
        assert_eq!(none["total"], 0);
        let without = session_directory(
            &data,
            &WorkflowQuery {
                prompts: Some("none".into()),
                ..Default::default()
            },
        );
        assert_eq!(without["rows"][0]["session_id"], "b");
    }

    #[test]
    fn directory_pagination_sorting_dates_and_urls_are_bounded() {
        let data = vec![
            directory_fixture("a", "2026-10-02", "Codex", true),
            directory_fixture("b", "2026-10-01", "Claude Code", false),
        ];
        let page = session_directory(
            &data,
            &WorkflowQuery {
                sort: Some("oldest".into()),
                per_page: Some(1),
                page: Some(usize::MAX),
                ..Default::default()
            },
        );
        assert_eq!(page["page"], 2);
        assert_eq!(page["rows"][0]["session_id"], "a");
        assert!(page["previous"].as_str().unwrap().contains("sort=oldest"));
        assert!(page["next"].as_str().unwrap().is_empty());
        let bad = session_directory(
            &data,
            &WorkflowQuery {
                since: Some("2026-02-30".into()),
                ..Default::default()
            },
        );
        assert_eq!(bad["total"], 0);
        assert!(!bad["errors"].as_array().unwrap().is_empty());
        assert!(valid_date("2024-02-29"));
        assert!(!valid_date("2026-02-29"));
        assert!(!valid_date("9999-99-99"));
        let url = directory_url(
            &WorkflowQuery {
                q: Some("work & café".into()),
                path: Some("/Working Tree".into()),
                ..Default::default()
            },
            2,
        );
        assert!(url.contains("q=work%20%26%20caf%C3%A9"));
        assert!(url.contains("path=%2FWorking%20Tree"));
    }

    fn render_directory(sessions: &[Value], query: &WorkflowQuery) -> String {
        let mut context = Context::new();
        context.insert("directory", &session_directory(sessions, query));
        decorate(&mut context, "Workflow sessions");
        for key in ["app_name", "environment", "csrf_token", "request_path"] {
            context.insert(key, "x");
        }
        context.insert("breadcrumbs", &Vec::<Breadcrumb>::new());
        context.insert("admin_navigation", &Vec::<Value>::new());
        context.insert("admin_actions", &Vec::<Value>::new());
        registry_tera(
            &UiRegistry::build(Some(&host()), &[contribution()])
                .unwrap()
                .unwrap(),
        )
        .render("audit/sessions.html", &context)
        .unwrap()
    }

    #[test]
    fn directory_triage_counts_quick_filters_day_headings_and_hides_empty_sessions() {
        let now = Utc::now();
        let at = |hours_ago: i64| {
            (now - chrono::Duration::hours(hours_ago))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        let ev = |session: &str, kind: &str, hours_ago: i64, extra: Value| {
            let mut payload = json!({"session_id": session, "task_id": format!("{session}-t"), "occurred_at": at(hours_ago)});
            payload
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!({"event_type": format!("workflow.{kind}"), "payload": payload})
        };
        let rows = vec![
            // Blocked and failing: needs attention.
            ev("blocked", "session_started", 1, json!({"cwd": "/Code/app"})),
            ev(
                "blocked",
                "session_updated",
                1,
                json!({"herdr_tab": "PR 9 review"}),
            ),
            ev("blocked", "task_started", 1, json!({"name": "review-pr-9"})),
            ev(
                "blocked",
                "review",
                1,
                json!({"check": "review", "exit_code": 1}),
            ),
            ev(
                "blocked",
                "task_blocked",
                1,
                json!({"outcome": "Cleanup failed"}),
            ),
            // Verified and completed today.
            ev("done", "task_started", 2, json!({"name": "Ship docs"})),
            ev(
                "done",
                "verification",
                2,
                json!({"check": "verify", "exit_code": 0}),
            ),
            ev(
                "done",
                "review",
                2,
                json!({"check": "review", "exit_code": 0}),
            ),
            ev("done", "task_completed", 2, json!({"outcome": "Docs live"})),
            // Ended with open todos: unfinished, several days ago.
            ev("open", "session_started", 80, json!({})),
            ev("open", "task_started", 80, json!({"name": "Half done"})),
            ev(
                "open",
                "todo_updated",
                80,
                json!({"todo_id": "a", "description": "a", "status": "pending"}),
            ),
            ev("open", "session_ended", 80, json!({})),
            // A probe with nothing to review.
            ev(
                "probe",
                "session_started",
                3,
                json!({"cwd": "/Library/Probe"}),
            ),
        ];
        let sessions = crate::workflow::sessions(&rows);
        let directory = session_directory(&sessions, &WorkflowQuery::default());
        assert_eq!(directory["total"], 3);
        assert_eq!(directory["hidden_empty"], 1);
        assert_eq!(
            directory["pulse"],
            json!({"running": 1, "needs": 1, "unfinished": 1, "verified_today": 1})
        );
        let headings: Vec<&str> = directory["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["day_heading"].as_str())
            .collect();
        assert_eq!(headings.len(), 2, "two calendar days: {headings:?}");
        assert!(headings[0].starts_with("Today · ") || headings[0].starts_with("Yesterday · "));
        // Quick filters.
        let only = |attention: &str| {
            let d = session_directory(
                &sessions,
                &WorkflowQuery {
                    attention: Some(attention.into()),
                    ..Default::default()
                },
            );
            d["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["session_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(only("needs"), ["blocked"]);
        let needs = render_directory(
            &sessions,
            &WorkflowQuery {
                attention: Some("needs".into()),
                ..Default::default()
            },
        );
        assert!(needs
            .contains(r#"<span class="badge badge--tab" title="Herdr tab">PR 9 review</span>"#));
        let by_tab = session_directory(
            &sessions,
            &WorkflowQuery {
                q: Some("PR 9 review".into()),
                ..Default::default()
            },
        );
        assert_eq!(by_tab["total"], 1);
        assert_eq!(only("unfinished"), ["open"]);
        assert_eq!(only("verified"), ["done"]);
        let shown = session_directory(
            &sessions,
            &WorkflowQuery {
                empty: Some("show".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            (shown["total"].as_u64(), shown["hidden_empty"].as_u64()),
            (Some(4), Some(0))
        );
        // Other sorts do not split rows by day.
        let by_name = session_directory(
            &sessions,
            &WorkflowQuery {
                sort: Some("name".into()),
                ..Default::default()
            },
        );
        assert!(by_name["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["day_heading"].is_null()));
        // Sorting from a column title does not open More filters.
        assert_eq!(by_name["advanced"], false);
        let order = |sort: &str| {
            session_directory(
                &sessions,
                &WorkflowQuery {
                    sort: Some(sort.into()),
                    ..Default::default()
                },
            )["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["session_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(order("checks"), ["blocked", "open", "done"]);
        assert_eq!(order("checks_desc"), ["done", "open", "blocked"]);
        assert_eq!(order("latest"), ["blocked", "done", "open"]);
        assert_eq!(order("oldest"), ["open", "done", "blocked"]);
        let reversed = order("name_desc");
        let mut forward = order("name");
        forward.reverse();
        assert_eq!(reversed, forward);
        assert_eq!(
            order("progress_desc").last().map(String::as_str),
            Some("done")
        );
        // Titles link to their first sort; the active title reverses it.
        let columns = |sort: Option<&str>| {
            session_directory(
                &sessions,
                &WorkflowQuery {
                    sort: sort.map(Into::into),
                    ..Default::default()
                },
            )["columns"]
                .clone()
        };
        let default = columns(None);
        assert_eq!(default[0]["label"], "Session");
        assert!(default[0]["href"].as_str().unwrap().contains("sort=name&"));
        assert_eq!(default[4]["active"], true);
        assert_eq!(default[4]["direction"], "descending");
        assert!(default[4]["href"].as_str().unwrap().contains("sort=oldest"));
        let by_project = columns(Some("project"));
        assert_eq!(by_project[1]["direction"], "ascending");
        assert!(by_project[1]["href"]
            .as_str()
            .unwrap()
            .contains("sort=project_desc"));
        assert_eq!(by_project[4]["active"], false);
        let html = render_directory(
            &sessions,
            &WorkflowQuery {
                sort: Some("project_desc".into()),
                attention: Some("needs".into()),
                ..Default::default()
            },
        );
        assert!(html.contains(r#"aria-sort="descending""#));
        // Sort links keep the active filters.
        assert!(html.contains("sort=project&amp;attention=needs"));

        let html = render_directory(&sessions, &WorkflowQuery::default());
        assert!(html.contains("Needs attention"));
        assert!(html.contains("Review PR 9"));
        assert!(html.contains("task blocked, check failed"));
        assert!(html.contains("Completed:</span> Docs live"));
        assert!(html.contains("1 empty session hidden"));
        assert!(html.contains("empty=show"));
        assert!(html.contains("class=\"badge badge--error\">review<"));
        assert!(html.contains("0/1 todos"));
        assert!(html.contains("data-client-relative="));
        assert!(
            !html.contains("<details class=\"more-filters\" open"),
            "advanced filters start closed"
        );
        let filtered = render_directory(
            &sessions,
            &WorkflowQuery {
                attention: Some("needs".into()),
                ..Default::default()
            },
        );
        assert!(filtered.contains("class=\"chip is-active\""));
        assert!(
            filtered.contains("name=\"attention\" value=\"needs\""),
            "search keeps the quick filter"
        );
        assert!(day_heading(
            "2025-12-31",
            chrono::NaiveDate::from_ymd_opt(2026, 1, 9).unwrap()
        )
        .ends_with("Dec 31, 2025"));
        assert_eq!(
            day_heading("", chrono::NaiveDate::from_ymd_opt(2026, 1, 9).unwrap()),
            "Date unknown"
        );
    }

    #[test]
    fn session_directory_template_renders_rows_filters_and_empty_results() {
        let mut data = vec![directory_fixture("id-hash", "2026-10-02", "Codex", true)];
        data[0]["label"] = json!("Repair <script>alert</script>");
        for query in [
            WorkflowQuery::default(),
            WorkflowQuery {
                q: Some("no-match".into()),
                ..Default::default()
            },
        ] {
            let mut context = Context::new();
            context.insert("directory", &session_directory(&data, &query));
            decorate(&mut context, "Workflow sessions");
            for key in ["app_name", "environment", "csrf_token", "request_path"] {
                context.insert(key, "x");
            }
            context.insert("breadcrumbs", &Vec::<Breadcrumb>::new());
            context.insert("admin_navigation", &Vec::<Value>::new());
            context.insert("admin_actions", &Vec::<Value>::new());
            let registry = UiRegistry::build(Some(&host()), &[contribution()])
                .unwrap()
                .unwrap();
            let html = registry_tera(&registry)
                .render("audit/sessions.html", &context)
                .unwrap();
            assert!(html.contains("Apply filters"));
            assert!(!html.contains("session-selector"));
            assert!(!html.contains("<script>alert</script>"));
            if query.q.is_none() {
                assert!(html.contains("Repair &lt;script&gt;"));
                assert!(html.contains("return_to="));
                assert!(!html.contains(">id-hash<"));
            } else {
                assert!(html.contains("No matching sessions"));
            }
        }
    }

    #[test]
    fn workflow_renders_tasks_with_phase_history() {
        let rows = vec![
            json!({"id":"evidence-id","recorded_at_us":123,"event_type":"workflow.tool_completed","payload":{"session_id":"session","task_id":"task","tool_name":"Tool <script>bad</script>","tool_call_id":"call-id","duration_ms":42,"exit_code":0,"outcome":"succeeded","occurred_at":"2026-10-02T00:03:00Z"}}),
            json!({"event_type":"workflow.session_updated","payload":{"session_id":"session","agent":"Codex","model":"example-model","cwd":"/example/Working Tree","occurred_at":"2026-10-02T00:00:00Z"}}),
            json!({"event_type":"workflow.prompt_recorded","payload":{"session_id":"session","task_id":"task","prompt_id":"p","part_index":0,"part_count":1,"text":"Example goal <script>bad</script>","occurred_at":"2026-10-02T00:00:00Z"}}),
            json!({"event_type":"workflow.task_started","payload":{"session_id":"session","task_id":"task","name":"Example task","status":"running","occurred_at":"2026-10-02T00:00:00Z"}}),
            json!({"event_type":"workflow.phase_changed","payload":{"session_id":"session","task_id":"task","phase":"build","phase_status":"active","occurred_at":"2026-10-02T00:01:00Z"}}),
            json!({"event_type":"workflow.review","payload":{"session_id":"session","task_id":"task","check":"review","exit_code":0,"outcome":"passed","occurred_at":"2026-10-02T00:02:00Z"}}),
            json!({"event_type":"workflow.todo_created","payload":{"session_id":"session","task_id":"task","todo_id":"item-a","description":"Repair <script>bad</script>","criterion":"Navigation regression passes","reason":"Initial scope","revision":1,"status":"pending","occurred_at":"2026-10-02T00:00:00Z"}}),
            json!({"event_type":"workflow.plan_registered","payload":{"session_id":"session","task_id":"task","description":"Complete initial scope","revision":1,"occurred_at":"2026-10-02T00:00:00Z"}}),
            json!({"event_type":"workflow.todo_updated","payload":{"session_id":"session","task_id":"task","todo_id":"item-a","description":"Repair <script>bad</script>","criterion":"Navigation regression passes","reason":"Regression confirmed","revision":2,"status":"completed","previous_status":"pending","evidence":"Navigation regression succeeded","occurred_at":"2026-10-02T00:02:00Z"}}),
        ];
        let sessions = crate::workflow::sessions(&rows);
        // Opening the task from a link renders its details inline.
        let query = WorkflowQuery {
            session_id: Some("session".into()),
            task_id: Some("task".into()),
            ..WorkflowQuery::default()
        };
        for selected in &sessions {
            let html = render_session(selected, &query);
            assert!(html.contains("Navigation regression passes"));
            assert!(html.contains("Navigation regression succeeded"));
            assert!(html.contains("Revision history · 2"));
            assert!(html.contains("Plan history · 1"));
            assert!(html.contains("todo_id=item-a"));
            assert!(html.contains("Repair &lt;script&gt;"));
            assert!(!html.contains("<script>bad</script>"));
            assert!(html.contains("Example task"));
            assert!(html.contains("Back to sessions"));
            assert!(html.contains("Session metadata"));
            assert!(!html.contains("Session analysis"));
            assert!(html.contains("<dd>Codex</dd>"));
            assert!(html.contains("<dd>example-model</dd>"));
            assert!(html.contains("Working directory"));
            assert!(html.contains("build: active"));
            assert!(html.contains("1/1 todos"));
            assert!(html.contains("Started <time data-client-time=\"2026-10-02T00:00:00Z\">"));
            assert!(html.contains("Added <time data-client-time=\"2026-10-02T00:00:00Z\">"));
            assert!(html.contains("Updated <time data-client-time=\"2026-10-02T00:02:00Z\">"));
            // Verdict strip and the signal view.
            assert!(html.contains("Session verdict"));
            assert!(html.contains("review 1/1"));
            assert!(html.contains("no verify"));
            assert!(html.contains("1 / 1 done"));
            assert!(html.contains("Phase: build"));
            assert!(html.contains("review passed"));
            assert!(html.contains("Todo done: Repair &lt;script&gt;"));
            assert!(html.contains("Example goal &lt;script&gt;bad&lt;"));
            assert!(html.contains("1 tool call · Tool &lt;script&gt;"));
            assert!(!html.contains("Event evidence"));
            // Raw events keep every card with its evidence.
            let raw = render_session(
                selected,
                &WorkflowQuery {
                    view: Some("all".into()),
                    ..query.clone()
                },
            );
            assert!(raw.contains("Tool succeeded"));
            assert!(raw.contains("42 ms"));
            assert!(raw.contains("call-id"));
            assert!(raw.contains("evidence-id"));
            assert!(raw.contains("Tool &lt;script&gt;"));
            assert!(!raw.contains("<script>bad</script>"));
            assert!(raw.contains("review · exit 0"));
            assert!(raw.contains("Event evidence"));
        }
    }

    fn render_session(session: &Value, query: &WorkflowQuery) -> String {
        let registry = UiRegistry::build(Some(&host()), &[contribution()])
            .unwrap()
            .unwrap();
        let mut context = Context::new();
        decorate(&mut context, "Workflow audit");
        for key in ["app_name", "environment", "csrf_token", "request_path"] {
            context.insert(key, "x");
        }
        context.insert("breadcrumbs", &Vec::<Breadcrumb>::new());
        context.insert("admin_navigation", &Vec::<Value>::new());
        context.insert("admin_actions", &Vec::<Value>::new());
        context.insert("sessions", &true);
        context.insert("selected", session);
        session_view(session, query, &mut context);
        registry_tera(&registry)
            .render("audit/workflow.html", &context)
            .expect("Workflow session renders")
    }

    #[test]
    fn flow_view_draws_one_lane_per_prompt_with_icons_and_inspectable_blocks() {
        let at = |second: usize| format!("2026-10-02T00:00:{second:02}Z");
        let rows = vec![
            json!({"event_type":"workflow.task_started","payload":{"session_id":"s","task_id":"t","name":"Repair parser","status":"running","occurred_at":at(1)}}),
            json!({"event_type":"workflow.prompt_recorded","payload":{"session_id":"s","task_id":"t","prompt_id":"p1","part_index":0,"part_count":1,"text":"Fix the <b>parser</b>","occurred_at":at(2)}}),
            json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t","tool_name":"mcp__browser__navigate","tool_label":"Open <the> page","outcome":"succeeded","exit_code":0,"duration_ms":12,"occurred_at":at(3)}}),
            json!({"event_type":"workflow.tool_failed","payload":{"session_id":"s","task_id":"t","tool_name":"Bash","tool_label":"Run the test suite","tool_command":"make <test>","outcome":"failed","exit_code":2,"occurred_at":at(4)}}),
            json!({"event_type":"workflow.decision","payload":{"session_id":"s","task_id":"t","description":"Pick the fix","options":["patch","rewrite"],"selected":"patch","occurred_at":at(5)}}),
            json!({"event_type":"workflow.prompt_recorded","payload":{"session_id":"s","task_id":"t","prompt_id":"p2","part_index":0,"part_count":1,"text":"Now verify","occurred_at":at(6)}}),
            json!({"event_type":"workflow.verification","payload":{"session_id":"s","task_id":"t","check":"verify","exit_code":0,"ran":3,"occurred_at":at(7)}}),
        ];
        let session = crate::workflow::sessions(&rows).remove(0);
        let mut query = WorkflowQuery {
            session_id: Some("s".into()),
            view: Some("flow".into()),
            ..WorkflowQuery::default()
        };
        // Newest first: the latest prompt's lane leads; blocks read left to right.
        let flow = flow_lanes(&session, &query);
        let lanes = flow["lanes"].as_array().unwrap();
        assert_eq!(lanes.len(), 3);
        assert_eq!(lanes[0]["prompt"]["prompt_number"], 2);
        assert_eq!(lanes[2]["prompt"], Value::Null);
        let icons = |lane: &Value| -> Vec<String> {
            lane["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["icon"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(icons(&lanes[0]), ["prompt", "check-ok"]);
        assert_eq!(
            icons(&lanes[1]),
            ["prompt", "tools", "tool-fail", "decision"]
        );
        assert_eq!(icons(&lanes[2]), ["task"]);
        let burst = &lanes[1]["blocks"][1];
        assert_eq!(burst["tools"][0]["short"], "navigate");
        assert_eq!(burst["call_list"][0]["short"], "navigate");
        // Inspector fields drop shared identifiers and empty values.
        let fields = lanes[1]["blocks"][2]["fields"].as_array().unwrap();
        assert!(fields
            .iter()
            .any(|f| f["key"] == "tool label" && f["value"] == "Run the test suite"));
        assert!(fields
            .iter()
            .all(|f| f["key"] != "session id" && f["key"] != "schema version"));
        query.order = Some("oldest".into());
        assert_eq!(
            flow_lanes(&session, &query)["lanes"][0]["prompt"],
            Value::Null
        );
        query.order = None;

        let html = render_session(&session, &query);
        assert!(html.contains("class=\"flow-inspector\""));
        assert_eq!(count(&html, "class=\"flow-lane\""), 3);
        assert_eq!(count(&html, "data-flow-open=\"flow-"), 7);
        for icon in [
            "prompt",
            "tools",
            "tool-fail",
            "decision",
            "check-ok",
            "task",
        ] {
            assert!(
                html.contains(&format!("<use href=\"#fi-{icon}\"/>")),
                "{icon} icon"
            );
            assert!(
                html.contains(&format!("<symbol id=\"fi-{icon}\"")),
                "{icon} symbol"
            );
        }
        assert!(html.contains("<template id=\"flow-2\">"));
        assert!(html.contains("Run the test suite"));
        assert!(html.contains("make &lt;test&gt;"));
        assert!(!html.contains("make <test>"));
        assert!(html.contains("Open &lt;the&gt; page"));
        assert!(html.contains("Fix the &lt;b&gt;parser"));
        assert!(!html.contains("<b>parser</b>"));
        assert!(html.contains("name=\"view\" value=\"flow\""));
        assert!(html.contains("data-flow=\"true\""));
        assert!(html.contains("aria-current=\"true\">Flow"));
        // The diagram is drawn whole, so it has no show-more control.
        assert!(!html.contains("data-more=\"timeline\""));
        // The signal list names the failed command under its headline.
        query.view = None;
        let signal = render_session(&session, &query);
        assert!(signal.contains(
            "Bash failed (exit 2)</span><span class=\"signal-detail\">make &lt;test&gt;"
        ));
        assert!(!signal.contains("class=\"flow-inspector\""));
    }

    fn busy_session() -> Value {
        let at = |minute: usize| format!("2026-10-02T00:{minute:02}:00Z");
        let mut rows = Vec::new();
        for n in 1..=5 {
            rows.push(json!({"event_type":"workflow.prompt_recorded","payload":{"session_id":"s","task_id":"t1","prompt_id":format!("p{n}"),"part_index":0,"part_count":1,"text":format!("Prompt number {n}"),"occurred_at":at(n)}}));
        }
        for n in 1..=4 {
            rows.push(json!({"event_type":"workflow.task_started","payload":{"session_id":"s","task_id":format!("t{n}"),"name":format!("Task {n}"),"status":"running","occurred_at":at(10 + n)}}));
            rows.push(json!({"event_type":"workflow.todo_created","payload":{"session_id":"s","task_id":format!("t{n}"),"todo_id":format!("d{n}"),"description":format!("Todo of task {n}"),"criterion":"Done","reason":"Plan","revision":1,"status":"pending","occurred_at":at(10 + n)}}));
        }
        for n in 0..16 {
            rows.push(json!({"event_type":"workflow.tool_completed","payload":{"session_id":"s","task_id":"t2","tool_name":"Bash","outcome":"succeeded","exit_code":0,"occurred_at":at(20 + n)}}));
        }
        crate::workflow::sessions(&rows).remove(0)
    }

    fn count(html: &str, needle: &str) -> usize {
        html.matches(needle).count()
    }

    #[test]
    fn session_lists_open_with_two_rows_and_a_show_more_control() {
        let session = busy_session();
        let mut query = WorkflowQuery {
            session_id: Some("s".into()),
            ..WorkflowQuery::default()
        };
        let html = render_session(&session, &query);
        assert_eq!(count(&html, "class=\"row-item task-row"), 2);
        assert!(html.contains("Showing 2 of 4 tasks"));
        // Signal view: 5 prompt chapters, 4 task starts and one burst for the 16 tool
        // calls; todo creation is bookkeeping and stays out.
        assert_eq!(count(&html, "class=\"signal-chapter\""), 5);
        assert!(html.contains("16 tool calls · Bash 16"));
        assert!(html.contains("Task started: Task 3"));
        let timeline =
            &html[html.find("id=\"timeline\"").unwrap()..html.find("session-aside").unwrap()];
        assert!(!timeline.contains("Todo of task 1"));
        assert!(html.contains("Showing 10 of 10 entries"));
        // Newest first by default: the latest prompt's chapter leads, and the
        // order toggle restores source order without losing the view.
        assert!(timeline.find(">P5<").unwrap() < timeline.find(">P1<").unwrap());
        assert!(html.contains("Signal view, newest first"));
        let decoded = html.replace("&#x2F;", "/").replace("&amp;", "&");
        assert!(decoded.contains(
            "href=\"/workflow?session_id=s&order=oldest&return_to=%2Fworkflow#timeline\""
        ));
        let oldest = render_session(
            &session,
            &WorkflowQuery {
                order: Some("oldest".into()),
                ..query.clone()
            },
        );
        assert!(oldest.find(">P1<").unwrap() < oldest.find(">P5<").unwrap());
        assert!(
            oldest.contains("name=\"order\" value=\"oldest\""),
            "filters keep the order"
        );
        assert!(oldest.contains("⇅ Newest first"));
        // Prompts open their chapter instead of a separate list.
        assert!(!html.contains("class=\"row-item prompt-row\""));
        assert!(html.contains("Prompt number 4"));
        // Raw events keep the full list, ten at a time.
        query.view = Some("all".into());
        let html = render_session(&session, &query);
        assert_eq!(count(&html, "<li data-category="), 10);
        assert!(html.contains("Showing 10 of 29 events"));
        // Raw events also lead with the latest: the last tool return is at the top.
        let raw = &html[html.find("id=\"timeline-list\"").unwrap()..];
        assert!(
            raw.find("2026-10-02T00:35:00Z").unwrap() < raw.find("2026-10-02T00:34:00Z").unwrap()
        );
        // Attribute values are escaped (`/` as `&#x2F;`); compare the decoded URLs.
        let decoded = html.replace("&#x2F;", "/").replace("&amp;", "&");
        assert!(decoded.contains(
            "data-more-src=\"/workflow/items?session_id=s&view=all&list=timeline&offset=10&limit=20"
        ));
        // Without scripts the control reloads with a longer list and keeps the view.
        assert!(decoded.contains(
            "href=\"/workflow?session_id=s&view=all&show_events=29&return_to=%2Fworkflow#timeline\""
        ));
        // Tasks are numbered in the order they started, and details wait until opened.
        let tasks = &html[html.find("id=\"tasks\"").unwrap()..];
        assert!(tasks.find("Task 1").unwrap() < tasks.find("Task 2").unwrap());
        assert!(tasks.contains("Open task details"));
        assert!(!html.contains("Revision history"));
    }

    #[test]
    fn show_more_returns_the_next_rows_and_task_details_on_request() {
        let session = busy_session();
        let mut query = WorkflowQuery {
            session_id: Some("s".into()),
            list: Some("prompts".into()),
            offset: Some(2),
            ..WorkflowQuery::default()
        };
        let body = workflow_fragment(&session, &query).unwrap();
        let html = body["html"].as_str().unwrap();
        assert_eq!(count(html, "class=\"row-item prompt-row\""), 3);
        assert!(html.contains("Prompt number 3") && !html.contains("Prompt number 2"));
        let more = body["more_html"].as_str().unwrap();
        assert!(more.contains("Showing 5 of 5 prompts") && !more.contains("Show "));

        query.list = Some("timeline".into());
        query.offset = Some(10);
        query.view = Some("all".into());
        let body = workflow_fragment(&session, &query).unwrap();
        assert_eq!(
            count(body["html"].as_str().unwrap(), "<li data-category="),
            19
        );

        query.list = Some("task".into());
        query.task_id = Some("t3".into());
        let body = workflow_fragment(&session, &query).unwrap();
        let html = body["html"].as_str().unwrap();
        assert!(html.contains("Todo of task 3") && html.contains("Revision history · 1"));
        assert!(html.contains("Added <time"));

        query.list = Some("other".into());
        assert!(workflow_fragment(&session, &query).is_err());
        query.list = Some("task".into());
        query.task_id = Some("missing".into());
        assert!(workflow_fragment(&session, &query).is_err());
    }

    #[test]
    fn timeline_filters_page_the_whole_session_and_linked_tasks_stay_visible() {
        let session = busy_session();
        let mut query = WorkflowQuery {
            session_id: Some("s".into()),
            event_category: Some("todo".into()),
            view: Some("all".into()),
            ..WorkflowQuery::default()
        };
        assert_eq!(timeline(&session, &query).len(), 4);
        query.event_category = None;
        query.event_q = Some("task 4 todo".into());
        assert_eq!(timeline(&session, &query).len(), 1);
        query.event_q = Some("bash".into());
        query.event_status = Some("succeeded".into());
        assert_eq!(timeline(&session, &query).len(), 16);
        let html = render_session(&session, &query);
        assert!(html.contains("Showing 10 of 16 events"));
        assert!(html.contains("event_q=bash"), "paging keeps the filter");
        assert!(html.contains("value=\"bash\""));
        // A filter change reloads the timeline from the start at its first size.
        let mut reload = query.clone();
        reload.list = Some("timeline".into());
        let body = workflow_fragment(&session, &reload).unwrap();
        assert_eq!(
            count(body["html"].as_str().unwrap(), "<li data-category="),
            10
        );
        assert!(body["more_html"]
            .as_str()
            .unwrap()
            .contains("Showing 10 of 16 events"));

        // A task opened from a link past the first rows is listed and expanded.
        let query = WorkflowQuery {
            session_id: Some("s".into()),
            task_id: Some("t4".into()),
            ..WorkflowQuery::default()
        };
        let html = render_session(&session, &query);
        assert_eq!(count(&html, "class=\"row-item task-row"), 4);
        assert!(
            html.contains("class=\"row-item task-row is-selected\" id=\"task-t4\"><details open")
        );
        assert!(html.contains("Todo of task 4"));
        assert!(html.contains("Showing 4 of 4 tasks"));
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
