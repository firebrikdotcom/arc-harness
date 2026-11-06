use crate::domain::audit_event::aggregate::AuditEventAggregate;
use crate::domain::audit_event::commands::AuditEventCommand;
use crate::reports::{EventFilter, ReportStore};
use actix_web::{get, post, put, web, HttpRequest, HttpResponse, Responder};
use arc_core::command_bus::{CommandBus, CommandContext};
use serde::Deserialize;
use serde_json::{json, Value};

/// Read the last atomically committed projection without waiting for catch-up.
pub async fn read_report<T, F>(store: web::Data<ReportStore>, read: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&ReportStore) -> anyhow::Result<T> + Send + 'static,
{
    web::block(move || read(&store)).await?
}

/// Ingestion acknowledges only after its committed source events are projected.
async fn refresh_report<T, F>(store: web::Data<ReportStore>, read: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&ReportStore) -> anyhow::Result<T> + Send + 'static,
{
    web::block(move || {
        while store.refresh()? == crate::reports::CATCH_UP_BATCH {}
        read(&store)
    })
    .await?
}

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
    offset: Option<usize>,
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
    store: web::Data<ReportStore>,
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
    let id = body.id.clone();
    match refresh_report(store.clone(), move |store| store.get_event(&id)).await {
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
        Ok(_) => match refresh_report(store, |_| Ok(())).await {
            Ok(()) => HttpResponse::Created().json(json!({"id": body.id})),
            Err(error) => {
                HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()}))
            }
        },
        Err(error) => {
            // Another request may have committed this ID after our initial read,
            // or dispatch may have persisted before reporting a publish failure.
            let id = body.id.clone();
            match refresh_report(store, move |store| store.get_event(&id)).await {
                Ok(Some(row))
                    if row["event_type"] == body.event_type && row["payload"] == body.payload =>
                {
                    HttpResponse::Ok().json(json!({"id": body.id, "duplicate": true}))
                }
                Ok(Some(_)) => HttpResponse::Conflict()
                    .json(json!({"error": "event ID reused with different data"})),
                Ok(None) => HttpResponse::BadRequest().json(json!({"error": error.to_string()})),
                Err(error) => {
                    HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()}))
                }
            }
        }
    }
}

#[get("/audit/events")]
async fn list_events(
    query: web::Query<ListQuery>,
    store: web::Data<ReportStore>,
) -> impl Responder {
    let limit = query.limit.unwrap_or(1000).clamp(1, 10_000);
    match read_report(store, move |store| {
        store.events(&EventFilter::default(), limit)
    })
    .await
    {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(error) => HttpResponse::InternalServerError().json(json!({"error": error.to_string()})),
    }
}

#[get("/audit/summary")]
async fn summary(store: web::Data<ReportStore>) -> impl Responder {
    report_response(read_report(store, |store| store.raw_summary()).await)
}

#[get("/audit/summary/checkpoints")]
async fn checkpoint_summary(
    query: web::Query<GroupedQuery>,
    store: web::Data<ReportStore>,
) -> impl Responder {
    report_response(
        read_report(store, move |store| {
            store.groups_page(
                "checkpoint",
                query.include_fixtures,
                query.offset.unwrap_or(0),
            )
        })
        .await,
    )
}

#[get("/audit/summary/routes")]
async fn route_summary(
    query: web::Query<GroupedQuery>,
    store: web::Data<ReportStore>,
) -> impl Responder {
    report_response(
        read_report(store, move |store| {
            store.groups_page("route", query.include_fixtures, query.offset.unwrap_or(0))
        })
        .await,
    )
}

#[get("/audit/orchestration")]
async fn orchestration_summary(
    query: web::Query<GroupedQuery>,
    store: web::Data<ReportStore>,
) -> impl Responder {
    report_response(
        read_report(store, move |store| {
            store.orchestration_page(query.offset.unwrap_or(0))
        })
        .await,
    )
}

fn report_response(result: anyhow::Result<Value>) -> HttpResponse {
    match result {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => HttpResponse::ServiceUnavailable().json(json!({"error": error.to_string()})),
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
async fn workflow_events(
    query: web::Query<GroupedQuery>,
    store: web::Data<ReportStore>,
) -> impl Responder {
    let offset = query.offset.unwrap_or(0);
    match read_report(store, move |store| {
        Ok((store.sessions_page(offset, 100)?, store.session_count()?))
    })
    .await
    {
        Ok((sessions, total)) => {
            let mut response = HttpResponse::Ok();
            response.insert_header(("X-Total-Count", total.to_string()));
            let next = offset.saturating_add(sessions.len());
            if next < total {
                response.insert_header((
                    "Link",
                    format!("</api/audit/workflow?offset={next}>; rel=next"),
                ));
            }
            response.json(sessions)
        }
        Err(error) => HttpResponse::ServiceUnavailable().json(json!({"error":error.to_string()})),
    }
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.configure(crate::ui::config).service(health).service(
        web::scope("/api")
            .service(collection_settings)
            .service(update_settings)
            .service(workflow_events)
            .service(orchestration_summary)
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

    #[actix_web::test]
    async fn page_reads_use_committed_reports_while_pending_updates_are_write_locked() {
        use diesel::{connection::SimpleConnection, Connection, SqliteConnection};
        let path = std::env::temp_dir().join(format!(
            "harness-read-snapshot-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = ReportStore::open(path.to_str().unwrap()).unwrap();
        let mut conn = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        conn.batch_execute(r#"CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,aggregate_id TEXT NOT NULL,aggregate_type TEXT NOT NULL,event_type TEXT NOT NULL,payload TEXT NOT NULL,sequence BIGINT NOT NULL,timestamp_utc_us BIGINT NOT NULL);
            INSERT INTO events VALUES(1,'first','AuditEvent','AuditEventRecorded','{"event_type":"jev.agent","payload":{}}',1,1);"#).unwrap();
        store.refresh().unwrap();
        conn.batch_execute(r#"INSERT INTO events VALUES(2,'pending','AuditEvent','AuditEventRecorded','{"event_type":"jev.agent","payload":{}}',1,2); BEGIN IMMEDIATE;"#).unwrap();
        let result = actix_web::rt::time::timeout(
            std::time::Duration::from_secs(1),
            read_report(web::Data::new(store.clone()), |store| store.raw_summary()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["events"], 1);
        conn.batch_execute("ROLLBACK;").unwrap();
        assert_eq!(store.refresh().unwrap(), 1);
        assert_eq!(store.raw_summary().unwrap()["events"], 2);
        drop(conn);
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[actix_web::test]
    async fn workflow_next_link_stops_at_last_page() {
        use actix_web::{test, App};
        use diesel::{connection::SimpleConnection, Connection, SqliteConnection};
        let path = std::env::temp_dir().join(format!(
            "harness-workflow-pages-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = ReportStore::open(path.to_str().unwrap()).unwrap();
        let app =
            test::init_service(App::new().app_data(web::Data::new(store)).configure(config)).await;
        let empty = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/audit/workflow")
                .to_request(),
        )
        .await;
        assert!(empty.status().is_success());
        assert_eq!(empty.headers().get("X-Total-Count").unwrap(), "0");
        assert!(!empty.headers().contains_key("Link"));
        let mut conn = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        conn.batch_execute("WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i<100) INSERT INTO dashboard_reports(key,document) SELECT 'directory:' || i, json_object('session_id',i,'last_at',i) FROM n;").unwrap();
        for (offset, length, next) in [
            (0, 100, true),
            (1, 100, false),
            (100, 1, false),
            (101, 0, false),
            (999, 0, false),
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(&format!("/api/audit/workflow?offset={offset}"))
                    .to_request(),
            )
            .await;
            assert!(response.status().is_success());
            assert_eq!(response.headers().get("X-Total-Count").unwrap(), "101");
            assert_eq!(response.headers().contains_key("Link"), next);
            if next {
                assert_eq!(
                    response.headers().get("Link").unwrap(),
                    "</api/audit/workflow?offset=100>; rel=next"
                );
            }
            let body: Vec<serde_json::Value> = test::read_body_json(response).await;
            assert_eq!(body.len(), length);
        }
        drop(app);
        drop(conn);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn list_limit_is_bounded() {
        let query = ListQuery {
            limit: Some(50_000),
        };
        assert_eq!(query.limit.unwrap_or(1000).clamp(1, 10_000), 10_000);
    }
}
