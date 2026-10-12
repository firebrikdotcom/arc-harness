use crate::domain::audit_event::aggregate::AuditEventAggregate;
use crate::domain::audit_event::commands::AuditEventCommand;
use crate::domain::audit_event::projector::AUDIT_EVENTS_VIEW;
use crate::summary as grouped;
use actix_web::{get, post, put, web, HttpRequest, HttpResponse, Responder};
use arc_core::command_bus::{CommandBus, CommandContext};
use arc_core::read_model_store::ReadModelStore;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
struct RecordAuditEvent {
    id: String,
    event_type: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct GroupedQuery {
    #[serde(default)]
    include_fixtures: bool,
}

#[get("/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(json!({
        "status": "healthy",
        "application": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[post("/audit/events")]
async fn record_event(
    body: web::Json<RecordAuditEvent>,
    bus: web::Data<CommandBus<AuditEventAggregate>>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    let config = match crate::settings::load() {
        Ok(config) => config,
        Err(error) => {
            return HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()}))
        }
    };
    if !config.permits(&body.event_type) {
        return HttpResponse::Ok().json(json!({"id": body.id, "collected": false}));
    }
    if body.event_type.starts_with("workflow.") {
        if let Err(error) = crate::workflow::validate(&body.event_type, &body.payload) {
            return HttpResponse::BadRequest().json(json!({"error": error}));
        }
    }
    // Stable queued IDs make a retry after an ambiguous response safe.
    match store.get(AUDIT_EVENTS_VIEW, &body.id).await {
        Ok(Some(row)) => {
            return if row["event_type"] == body.event_type && row["payload"] == body.payload {
                HttpResponse::Ok().json(json!({"id": body.id, "duplicate": true}))
            } else {
                HttpResponse::Conflict()
                    .json(json!({"error": "event ID reused with different data"}))
            };
        }
        Ok(None) => {}
        Err(error) => {
            return HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()}))
        }
    }
    let command = AuditEventCommand::Record {
        id: body.id.clone(),
        event_type: body.event_type.clone(),
        payload: body.payload.clone(),
    };
    match bus.dispatch(command, CommandContext::system()).await {
        Ok(_) => HttpResponse::Created().json(json!({"id": body.id})),
        Err(error) => HttpResponse::BadRequest().json(json!({"error": error.to_string()})),
    }
}

#[get("/audit/events")]
async fn list_events(
    query: web::Query<ListQuery>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    let limit = query.limit.unwrap_or(1000).clamp(1, 10_000);
    match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(mut rows) => {
            rows.sort_by_key(|row| row.get("recorded_at_us").and_then(Value::as_i64));
            rows.reverse();
            rows.truncate(limit);
            HttpResponse::Ok().json(rows)
        }
        Err(error) => HttpResponse::InternalServerError().json(json!({"error": error.to_string()})),
    }
}

#[get("/audit/summary")]
async fn summary(store: web::Data<dyn ReadModelStore>) -> impl Responder {
    let rows = match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => rows,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({"error": error.to_string()}));
        }
    };
    let mut event_types = BTreeMap::<String, usize>::new();
    let mut totals = BTreeMap::<&str, i64>::new();
    let mut outcomes = BTreeMap::<String, usize>::new();
    for row in &rows {
        if let Some(event_type) = row.get("event_type").and_then(Value::as_str) {
            *event_types.entry(event_type.to_string()).or_default() += 1;
        }
        let payload = row.get("payload").and_then(Value::as_object);
        if let Some(payload) = payload {
            if let Some(outcome) = payload.get("outcome").and_then(Value::as_str) {
                *outcomes.entry(outcome.to_string()).or_default() += 1;
            }
            for key in [
                "jev_input_tokens",
                "jev_output_tokens",
                "agent_input_tokens",
                "agent_output_tokens",
                "agent_total_tokens",
                "agent_total_tokens_delta",
                "total_decision_ms",
                "baseline_ms",
                "rework_ms",
            ] {
                if let Some(value) = payload.get(key).and_then(Value::as_i64) {
                    *totals.entry(key).or_default() += value;
                }
            }
        }
    }
    HttpResponse::Ok().json(json!({
        "events": rows.len(),
        "event_types": event_types,
        "outcomes": outcomes,
        "totals": totals,
    }))
}

#[get("/audit/summary/checkpoints")]
async fn checkpoint_summary(
    query: web::Query<GroupedQuery>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => {
            HttpResponse::Ok().json(grouped::checkpoint_groups(&rows, query.include_fixtures))
        }
        Err(error) => HttpResponse::InternalServerError().json(json!({"error": error.to_string()})),
    }
}

#[get("/audit/summary/routes")]
async fn route_summary(
    query: web::Query<GroupedQuery>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => HttpResponse::Ok().json(grouped::route_groups(&rows, query.include_fixtures)),
        Err(error) => HttpResponse::InternalServerError().json(json!({"error": error.to_string()})),
    }
}

#[get("/audit/settings")]
async fn collection_settings() -> impl Responder {
    match crate::settings::load() {
        Ok(settings) => HttpResponse::Ok().json(settings),
        Err(error) => HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()})),
    }
}

#[put("/audit/settings")]
async fn update_settings(
    request: HttpRequest,
    body: web::Json<crate::settings::CollectionSettings>,
) -> impl Responder {
    if let Some(origin) = request.headers().get("Origin") {
        let expected = format!(
            "{}://{}",
            request.connection_info().scheme(),
            request.connection_info().host()
        );
        if origin.to_str().ok() != Some(expected.as_str()) {
            return HttpResponse::Forbidden()
                .json(json!({"error": "same-origin request required"}));
        }
    }
    match crate::settings::save(body.into_inner()) {
        Ok(()) => HttpResponse::Ok().json(json!({"saved": true})),
        Err(error) => HttpResponse::InternalServerError().json(json!({"error": error.to_string()})),
    }
}

#[get("/audit/workflow")]
async fn workflow_events(store: web::Data<dyn ReadModelStore>) -> impl Responder {
    match store.list(AUDIT_EVENTS_VIEW).await {
        Ok(rows) => HttpResponse::Ok().json(crate::workflow::sessions(&rows)),
        Err(error) => HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()})),
    }
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.configure(crate::ui::config).service(health).service(
        web::scope("/api")
            .service(collection_settings)
            .service(update_settings)
            .service(workflow_events)
            .service(record_event)
            .service(list_events)
            .service(summary)
            .service(checkpoint_summary)
            .service(route_summary),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_limit_is_bounded() {
        let query = ListQuery {
            limit: Some(50_000),
        };
        assert_eq!(query.limit.unwrap_or(1000).clamp(1, 10_000), 10_000);
    }
}
