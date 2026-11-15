//! Glue between the pages and the Task Managers state facade and agent
//! runtime, both registered as app data. Their work blocks (sync diesel, git,
//! gh, subprocesses), so it always runs on the blocking pool, never on the
//! request's executor.

use std::future::Future;
use std::sync::Arc;

use actix_web::http::StatusCode;
use actix_web::{web, HttpRequest, HttpResponse};

use super::{not_found, plain};
use crate::task_managers::agents::Runtime;
use crate::task_managers::{TaskError, TaskManagers};

/// The facade and the runtime, when the service registered them.
pub(super) fn facade(request: &HttpRequest) -> Option<(Arc<TaskManagers>, Arc<Runtime>)> {
    let state = request
        .app_data::<web::Data<TaskManagers>>()?
        .clone()
        .into_inner();
    let runtime = request
        .app_data::<web::Data<Runtime>>()?
        .clone()
        .into_inner();
    Some((state, runtime))
}

pub(super) fn unavailable() -> HttpResponse {
    plain(
        StatusCode::SERVICE_UNAVAILABLE,
        "Task Managers state is not available in this service.",
    )
}

/// Synchronous facade work (index reads) on the blocking pool.
pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TaskError> + Send + 'static,
) -> Result<T, TaskError> {
    web::block(work)
        .await
        .unwrap_or_else(|error| Err(TaskError::Internal(error.to_string())))
}

/// Asynchronous facade work (a command bus dispatch, an agent start or
/// finish) on the blocking pool, driven by a private runtime there.
pub(super) async fn on_runtime<T, F, Fut>(work: F) -> Result<T, TaskError>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, TaskError>>,
{
    blocking(move || {
        let runtime = actix_web::rt::Runtime::new()
            .map_err(|error| TaskError::Internal(format!("could not start a runtime: {error}")))?;
        runtime.block_on(work())
    })
    .await
}

/// A facade or runtime refusal as plain text, in the same shape as a CLI one.
pub(super) fn task_refused(error: &TaskError) -> HttpResponse {
    tracing::warn!(kind = error.kind(), error = %error, "task manager state action failed");
    let (status, prefix) = match error {
        TaskError::NotFound(_) => return not_found(),
        TaskError::Usage(_) => (StatusCode::BAD_REQUEST, "Refused"),
        TaskError::Conflict(_) => (StatusCode::CONFLICT, "Not now"),
        TaskError::Internal(_) => (StatusCode::BAD_GATEWAY, "It failed"),
    };
    plain(
        status,
        format!("{prefix}: {}", super::clip(&error.to_string(), 600)),
    )
}
