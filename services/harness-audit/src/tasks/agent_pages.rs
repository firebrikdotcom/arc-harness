//! Attachable agents: the panel on an item page, the agents page, and the
//! start, stop, hand back and done actions. The agent's first turn is
//! read-only; the owner attaches from a terminal with the code shown here.

use std::path::PathBuf;
use std::sync::Arc;

use actix_session::Session;
use actix_web::http::StatusCode;
use actix_web::{get, web, HttpRequest, HttpResponse};
use arc_web::ui::Breadcrumb;
use arc_web::UiRegistry;
use serde::Deserialize;
use serde_json::{json, Value};
use tera::Context;

use super::edit::{directory, listed_repo};
use super::state::{self, blocking, on_runtime, task_refused};
use super::{
    act, agent_label, crumbs_root, http_url, item_target, not_found, plain, see_other, Check,
    NumId, Page, ProviderId,
};
use crate::domain::task_agent::events::{AgentKind, AgentState};
use crate::task_managers::agents::{self, AgentView, StartRequest};
use crate::task_managers::{valid_code, valid_id, TaskError, PROVIDER};

pub(super) const VIEW_AGENTS: &str = "capabilities/harness-audit-tasks/agents.html";

/// A badge label and tone for an agent state.
fn agent_badge(state: AgentState) -> (&'static str, &'static str) {
    match state {
        AgentState::Running => ("First turn running", "neutral"),
        AgentState::Waiting => ("Waiting for you", "warning"),
        AgentState::TakeoverRequested => ("Attaching", "neutral"),
        AgentState::Attached => ("Attached", "success"),
        AgentState::Stopped => ("Stopped", "ghost"),
        AgentState::Done => ("Done", "success"),
        AgentState::Failed => ("Failed", "error"),
    }
}

fn timestamp(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|at| at.to_rfc3339())
        .unwrap_or_default()
}

/// One agent for the templates. Only the checked code and item id go into
/// attributes; nothing here is task-manager text.
fn shape_agent(view: &AgentView) -> Option<Value> {
    let record = &view.record;
    if !valid_code(&record.code) || !valid_id(&record.item_id) {
        return None;
    }
    let (label, tone) = agent_badge(record.state);
    let live = record.state.is_live();
    let turn_running = matches!(
        record.state,
        AgentState::Running | AgentState::TakeoverRequested
    ) && view.process_alive;
    Some(json!({
        "code": record.code,
        "attach": format!("harness tasks attach {}", record.code),
        "item_id": record.item_id,
        "item_href": format!("/tasks/{PROVIDER}/items/{}", record.item_id),
        "state": record.state.as_str(),
        "label": label,
        "tone": tone,
        "live": live,
        "agent_label": agent_label(record.agent_kind.as_str()),
        "repo": record.repo,
        "worktree": record.worktree,
        "branch": record.branch,
        "created_worktree": record.created_worktree,
        "process_alive": view.process_alive,
        "pr_url": http_url(&json!(record.pr_url)),
        "reason": record.reason,
        "can_stop": live,
        "can_handback": record.state == AgentState::Attached,
        "can_done": live && !turn_running,
        "started_at": timestamp(record.started_at),
        "updated_at": timestamp(record.updated_at),
    }))
}

/// The agent panel of an item page: its live agent, or the start form's
/// defaults from the list's association. `None` when the state is unavailable.
pub(super) async fn panel(
    request: &HttpRequest,
    check: &Arc<Check>,
    provider: &ProviderId,
    item: &NumId,
    list: Option<String>,
) -> Option<Value> {
    if provider.as_str() != PROVIDER {
        return None;
    }
    let (tm, rt) = state::facade(request)?;
    let check = check.clone();
    let provider = provider.clone();
    let item = item.as_str().to_owned();
    let found = blocking(move || {
        tm.refresh()?;
        let internal = |error: anyhow::Error| TaskError::Internal(error.to_string());
        let live = tm
            .store()
            .live_agent_for_item(&item)
            .map_err(internal)?
            .map(|record| agents::view(record, rt.ssh.as_deref()));
        let association = match list.as_deref().filter(|list| valid_id(list)) {
            Some(list) => tm
                .store()
                .resolve_association(list, check.mapped_repo(&provider, list))
                .map_err(internal)?,
            None => None,
        };
        Ok((live, association))
    })
    .await;
    Some(match found {
        Ok((live, association)) => json!({
            "agent": live.as_ref().and_then(shape_agent),
            "repo": association.as_ref().map(|a| a.repo.clone()),
            "dir": association.as_ref().and_then(|a| a.dir.clone()),
            "source": association.as_ref().map(|a| a.source),
            "error": null,
        }),
        Err(error) => {
            tracing::warn!(error = %error, "task agent panel could not be read");
            json!({"agent": null, "repo": null, "dir": null, "source": null, "error": super::clip(&error.to_string(), 400)})
        }
    })
}

/// `GET /tasks/agents`: every agent, live first.
#[get("/tasks/agents")]
pub(super) async fn agents_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    let page = match Page::open(&request, &session, &registry).await {
        Ok(page) => page,
        Err(response) => return response,
    };
    let Some((tm, rt)) = state::facade(&request) else {
        return state::unavailable();
    };
    let listed = blocking(move || agents::list(&tm, &rt)).await;
    let rows: Vec<Value> = match listed {
        Ok(views) => views.iter().filter_map(shape_agent).collect(),
        Err(error) => return task_refused(&error),
    };
    let live = rows.iter().filter(|row| row["live"] == true).count();
    let mut context = Context::new();
    context.insert("agents", &rows);
    context.insert("live", &live);
    page.show(
        VIEW_AGENTS,
        "Agents",
        "agents",
        context,
        vec![crumbs_root(), Breadcrumb::current("Agents")],
    )
    .await
}

#[derive(Debug, Deserialize)]
pub(super) struct StartForm {
    #[serde(default)]
    csrf_token: String,
    /// Empty: the list's association.
    #[serde(default)]
    repo: String,
    /// Empty: the association's directory, else a fresh worktree.
    #[serde(default)]
    dir: String,
    #[serde(default)]
    agent: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct AgentForm {
    #[serde(default)]
    csrf_token: String,
    /// `item` returns to the agent's item page; anything else to the agents page.
    #[serde(default)]
    back: String,
}

/// `POST /tasks/{provider}/items/{id}/agent`: start an agent on the item.
pub(super) async fn start(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<StartForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_item) = path.into_inner();
    let Some((provider, item)) = item_target(&check, &raw_provider, &raw_item)
        .filter(|(provider, _)| provider.as_str() == PROVIDER)
    else {
        return not_found();
    };
    let Some(form) = form else {
        return plain(StatusCode::BAD_REQUEST, "The form could not be read.");
    };
    let Some(agent_kind) = AgentKind::parse(form.agent.trim()) else {
        return plain(StatusCode::BAD_REQUEST, "Choose an agent.");
    };
    let repo = match listed_repo(&check, &form.repo) {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    let dir: Option<PathBuf> = match directory(&form.dir) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let Some((tm, rt)) = state::facade(&request) else {
        return state::unavailable();
    };
    let request = StartRequest {
        item_id: item.as_str().to_owned(),
        repo,
        dir,
        agent_kind,
    };
    match on_runtime(move || async move { agents::start(&tm, &rt, request).await }).await {
        Ok(_) => see_other(&format!(
            "/tasks/{}/items/{}",
            provider.as_str(),
            item.as_str()
        )),
        Err(error) => task_refused(&error),
    }
}

#[derive(Debug, Clone, Copy)]
enum Lifecycle {
    Stop,
    HandBack,
    Done,
}

async fn lifecycle(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<AgentForm>>,
    action: Lifecycle,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    if let Err(response) = act(&request, &session, token).await {
        return response;
    }
    let code = path.into_inner();
    if !valid_code(&code) {
        return not_found();
    }
    let Some((tm, rt)) = state::facade(&request) else {
        return state::unavailable();
    };
    let to_item = form.as_ref().is_some_and(|form| form.back == "item");
    let finished = on_runtime(move || async move {
        match action {
            Lifecycle::Stop => agents::stop(&tm, &rt, &code).await,
            Lifecycle::HandBack => agents::handback(&tm, &rt, &code).await,
            Lifecycle::Done => {
                let done = agents::done(&tm, &rt, &code).await?;
                if let Some(error) = &done.cleanup_error {
                    tracing::warn!(error = %error, "the agent's worktree could not be removed");
                }
                Ok(done.agent)
            }
        }
    })
    .await;
    match finished {
        Ok(view) if to_item && valid_id(&view.record.item_id) => {
            see_other(&format!("/tasks/{PROVIDER}/items/{}", view.record.item_id))
        }
        Ok(_) => see_other("/tasks/agents"),
        Err(error) => task_refused(&error),
    }
}

/// `POST /tasks/agents/{code}/stop`.
pub(super) async fn stop(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<AgentForm>>,
) -> HttpResponse {
    lifecycle(request, session, path, form, Lifecycle::Stop).await
}

/// `POST /tasks/agents/{code}/handback`.
pub(super) async fn handback(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<AgentForm>>,
) -> HttpResponse {
    lifecycle(request, session, path, form, Lifecycle::HandBack).await
}

/// `POST /tasks/agents/{code}/done`: commit, push, one draft pull request.
pub(super) async fn done(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<AgentForm>>,
) -> HttpResponse {
    lifecycle(request, session, path, form, Lifecycle::Done).await
}

/// `/tasks/agents` comes before `/tasks/{provider}`, so "agents" is never a provider.
pub(super) fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(agents_page)
        .service(web::resource("/tasks/agents/{code}/stop").route(web::post().to(stop)))
        .service(web::resource("/tasks/agents/{code}/handback").route(web::post().to(handback)))
        .service(web::resource("/tasks/agents/{code}/done").route(web::post().to(done)))
        .service(web::resource("/tasks/{provider}/items/{id}/agent").route(web::post().to(start)));
}
