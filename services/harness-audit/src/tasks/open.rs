//! Open items: every not-done item across projects, filtered, with presets.
//! The CLI walks the provider (`open-walk`, `me`); the facade filters and
//! keeps the presets. Filters are checked here before either is asked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use actix_session::Session;
use actix_web::http::StatusCode;
use actix_web::{get, web, HttpRequest, HttpResponse};
use arc_web::ui::Breadcrumb;
use arc_web::UiRegistry;
use serde::Deserialize;
use serde_json::{json, Value};
use tera::Context;

use super::state::{self, blocking, on_runtime, task_refused};
use super::{
    act, clip, crumbs_root, http_url, item_target, named, not_found, num_id, plain, see_other,
    text, words, Check, Failure, NumId, Outcome, Page, ProviderId, Tasks, READ_TIMEOUT,
};
use crate::domain::task_preset::events::{AgentFilter, TaskFilters};
use crate::task_managers::open::effective_filters;
use crate::task_managers::{
    valid_date, valid_id, valid_label, valid_preset_name, TaskError, TaskManagers, PROVIDER,
};

/// The open page's view template.
pub(super) const VIEW_OPEN: &str = "capabilities/harness-audit-tasks/open.html";
/// Walking every list of every project can take a while on a cold cache.
const WALK_TIMEOUT: Duration = Duration::from_secs(60);
/// Most not-done items one walk reads before filtering.
pub(super) const WALK_LIMIT: &str = "1000";
/// Rows on the open page, and in the section on the source page.
const PAGE_ROWS: usize = 200;
const SECTION_ROWS: usize = 25;

impl Tasks {
    pub(super) async fn open_walk(
        self: Arc<Self>,
        provider: ProviderId,
        project: Option<NumId>,
        list: Option<NumId>,
    ) -> Outcome {
        let mut args = words(&["open-walk", "--provider", provider.as_str()]);
        if let Some(project) = &project {
            args.extend(words(&["--project", project.as_str()]));
        }
        if let Some(list) = &list {
            args.extend(words(&["--list", list.as_str()]));
        }
        args.extend(words(&["--limit", WALK_LIMIT, "--json"]));
        self.call(args, WALK_TIMEOUT).await
    }

    pub(super) async fn me(self: Arc<Self>, provider: ProviderId) -> Outcome {
        let args = words(&["me", "--provider", provider.as_str(), "--json"]);
        self.call(args, READ_TIMEOUT).await
    }
}

/// The filter fields, as the GET form and the preset form send them. Empty means unset.
#[derive(Debug, Default, Deserialize)]
pub(super) struct FilterFields {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    project: String,
    #[serde(default)]
    list: String,
    #[serde(default)]
    assignee: String,
    #[serde(default)]
    due_before: String,
    #[serde(default)]
    overdue: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    agent: String,
    /// A preset to start from (GET), or the name to save under (POST).
    #[serde(default)]
    preset: String,
}

/// Checked filters, or why not. Nothing here reaches the CLI or the facade unchecked.
pub(super) fn parse_filters(fields: &FilterFields, check: &Check) -> Result<TaskFilters, String> {
    let id = |raw: &str, what: &str| match raw.trim() {
        "" => Ok(None),
        value if valid_id(value) => Ok(Some(value.to_owned())),
        _ => Err(format!("{what} must be 1-20 digits")),
    };
    let assignee = match fields.assignee.trim() {
        "" => None,
        value if valid_label(value) => Some(value.to_owned()),
        _ => return Err("assignee must be 1-100 printable characters, or me".into()),
    };
    let due_before = match fields.due_before.trim() {
        "" => None,
        value if valid_date(value) => Some(value.to_owned()),
        _ => return Err("due before must be a YYYY-MM-DD date".into()),
    };
    let overdue = match fields.overdue.trim() {
        "" => false,
        "on" | "1" | "true" => true,
        _ => return Err("overdue is a checkbox".into()),
    };
    let repo = match fields.repo.trim() {
        "" => None,
        value if check.repos.iter().any(|name| name == value) => Some(value.to_owned()),
        _ => return Err("repository must be one of the configured repos".into()),
    };
    let agent = match fields.agent.trim() {
        "" | "any" => None,
        "none" => Some(AgentFilter::NoAgent),
        "active" => Some(AgentFilter::Active),
        _ => return Err("agent must be none or active".into()),
    };
    Ok(TaskFilters {
        project: id(&fields.project, "project")?,
        list: id(&fields.list, "list")?,
        assignee,
        due_before,
        overdue,
        repo,
        agent,
    })
}

/// A preset's filters with every field the request set laid over them.
fn over(base: &TaskFilters, given: TaskFilters) -> TaskFilters {
    TaskFilters {
        project: given.project.or_else(|| base.project.clone()),
        list: given.list.or_else(|| base.list.clone()),
        assignee: given.assignee.or_else(|| base.assignee.clone()),
        due_before: given.due_before.or_else(|| base.due_before.clone()),
        overdue: given.overdue || base.overdue,
        repo: given.repo.or_else(|| base.repo.clone()),
        agent: given.agent.or(base.agent),
    }
}

fn agent_word(filter: Option<AgentFilter>) -> &'static str {
    match filter {
        None => "",
        Some(AgentFilter::NoAgent) => "none",
        Some(AgentFilter::Active) => "active",
    }
}

/// Filters as the form fields show them.
fn filter_fields(filters: &TaskFilters) -> Value {
    json!({
        "project": filters.project,
        "list": filters.list,
        "assignee": filters.assignee,
        "due_before": filters.due_before,
        "overdue": filters.overdue,
        "repo": filters.repo,
        "agent": agent_word(filters.agent),
    })
}

/// Why an open view could not be built.
pub(super) enum OpenFailure {
    Cli(Failure),
    State(TaskError),
}

/// What the open page and the source section show.
pub(super) struct OpenView {
    pub rows: Vec<Value>,
    pub filters: TaskFilters,
    /// The default preset's name when it supplied the filters.
    pub preset: Option<String>,
    pub truncated: bool,
    pub presets: Vec<Value>,
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// One open row for the templates; ids are checked before they reach a link.
fn shape_row(
    item: &Value,
    repos: &BTreeMap<String, String>,
    agents: &BTreeMap<String, String>,
    today: &str,
    check: &Check,
) -> Option<Value> {
    let id = num_id(&item["id"])?;
    let list = named(&item["list"]);
    let assignees: Vec<String> = item["assignees"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|person| person["name"].as_str().or_else(|| person.as_str()))
                .map(|name| clip(name, 100))
                .collect()
        })
        .unwrap_or_default();
    let due_on = text(item, "due_on");
    let overdue = due_on
        .get(..10)
        .is_some_and(|due| valid_date(due) && due < today);
    let repo = list["id"]
        .as_str()
        .and_then(|list| repos.get(list))
        .cloned();
    Some(json!({
        "agent_code": agents.get(&id),
        "id": id,
        "title": text(item, "title"),
        "project": named(&item["project"]),
        "list": list,
        "assignees": assignees,
        "due_on": due_on,
        "overdue": overdue,
        "url": http_url(&item["url"]),
        "repo_label": repo.as_deref().map(|repo| check.label(repo)),
        "repo": repo,
    }))
}

/// Walk, filter and shape. `given` empty means the default preset applies.
pub(super) async fn open_view(
    tasks: Arc<Tasks>,
    check: Arc<Check>,
    tm: Arc<TaskManagers>,
    provider: ProviderId,
    given: TaskFilters,
    limit: usize,
) -> Result<OpenView, OpenFailure> {
    let presets = {
        let tm = tm.clone();
        blocking(move || {
            tm.refresh()?;
            tm.store()
                .presets()
                .map_err(|error| TaskError::Internal(error.to_string()))
        })
        .await
        .map_err(OpenFailure::State)?
    };
    let default = presets.iter().find(|preset| preset.is_default);
    let (effective, _) = effective_filters(&given, default);
    let me = if effective.assignee.as_deref() == Some("me") {
        let person = tasks
            .clone()
            .me(provider.clone())
            .await
            .map_err(OpenFailure::Cli)?;
        num_id(&person["person"]["id"])
    } else {
        None
    };
    let walk = tasks
        .open_walk(
            provider.clone(),
            effective.project.as_deref().and_then(NumId::parse),
            effective.list.as_deref().and_then(NumId::parse),
        )
        .await
        .map_err(OpenFailure::Cli)?;
    let today = today();
    let day = today.clone();
    let labels = check.clone();
    let (page, repos, agents) = blocking(move || {
        let default_repo = |list: &str| check.mapped_repo(&provider, list).map(str::to_owned);
        let page = tm.open_items(&walk, &given, me.as_deref(), &day, &default_repo, limit)?;
        let internal = |error: anyhow::Error| TaskError::Internal(error.to_string());
        let mut repos = BTreeMap::new();
        for item in &page.items {
            let Some(list) = num_id(&item["list"]["id"]) else {
                continue;
            };
            if repos.contains_key(&list) {
                continue;
            }
            let association = tm
                .store()
                .resolve_association(&list, default_repo(&list).as_deref())
                .map_err(internal)?;
            if let Some(association) = association {
                repos.insert(list, association.repo);
            }
        }
        let agents: BTreeMap<String, String> = tm
            .store()
            .agents()
            .map_err(internal)?
            .into_iter()
            .filter(|agent| agent.state.is_live())
            .map(|agent| (agent.item_id, agent.code))
            .collect();
        Ok((page, repos, agents))
    })
    .await
    .map_err(OpenFailure::State)?;
    let rows = page
        .items
        .iter()
        .filter_map(|item| shape_row(item, &repos, &agents, &today, &labels))
        .collect();
    let presets = presets
        .iter()
        .map(|preset| {
            json!({
                "name": preset.name,
                "is_default": preset.is_default,
                "filters": filter_fields(&preset.filters),
            })
        })
        .collect();
    Ok(OpenView {
        rows,
        filters: page.filters,
        preset: page.preset,
        truncated: page.truncated,
        presets,
    })
}

/// The open-items section on a source page: the default preset, a few rows.
/// A failure is shown in the section and never fails the page.
pub(super) async fn section(page: &Page<'_>, provider: &ProviderId) -> Option<Value> {
    if provider.as_str() != PROVIDER {
        return None;
    }
    let (tm, _) = state::facade(page.request)?;
    let view = open_view(
        page.tasks.clone(),
        page.check.clone(),
        tm,
        provider.clone(),
        TaskFilters::default(),
        SECTION_ROWS,
    )
    .await;
    Some(match view {
        Ok(view) => json!({
            "rows": view.rows,
            "preset": view.preset,
            "truncated": view.truncated,
            "error": null,
        }),
        Err(failure) => {
            let message = match failure {
                OpenFailure::Cli(failure) => failure.message,
                OpenFailure::State(error) => error.to_string(),
            };
            tracing::warn!(error = %message, "open items could not be read");
            json!({"rows": [], "preset": null, "truncated": false, "error": clip(&message, 400)})
        }
    })
}

/// `GET /tasks/{provider}/open`: every not-done item, filtered.
#[get("/tasks/{provider}/open")]
pub(super) async fn open_page(
    request: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
    path: web::Path<String>,
    query: Option<web::Query<FilterFields>>,
) -> HttpResponse {
    let page = match Page::open(&request, &session, &registry).await {
        Ok(page) => page,
        Err(response) => return response,
    };
    let Some((provider, source)) = page
        .provider(&path)
        .filter(|(provider, source)| provider.as_str() == PROVIDER && source["connected"] == true)
    else {
        return not_found();
    };
    let Some(query) = query else {
        return plain(StatusCode::BAD_REQUEST, "The filters could not be read.");
    };
    let mut given = match parse_filters(&query, &page.check) {
        Ok(filters) => filters,
        Err(message) => return plain(StatusCode::BAD_REQUEST, format!("Filters: {message}.")),
    };
    let preset_name = query.preset.trim().to_owned();
    if !preset_name.is_empty() && !valid_preset_name(&preset_name) {
        return plain(
            StatusCode::BAD_REQUEST,
            "Preset names are 1-32 of a-z, 0-9, _ and -.",
        );
    }
    let Some((tm, _)) = state::facade(&request) else {
        return state::unavailable();
    };
    if !preset_name.is_empty() {
        let name = preset_name.clone();
        let reader = tm.clone();
        let preset = blocking(move || {
            reader.refresh()?;
            let presets = reader
                .store()
                .presets()
                .map_err(|error| TaskError::Internal(error.to_string()))?;
            presets
                .into_iter()
                .find(|preset| preset.name == name)
                .ok_or_else(|| TaskError::NotFound(format!("no preset named {name}")))
        })
        .await;
        match preset {
            Ok(preset) => given = over(&preset.filters, given),
            Err(error) => return task_refused(&error),
        }
    }
    let view = open_view(
        page.tasks.clone(),
        page.check.clone(),
        tm,
        provider.clone(),
        given,
        PAGE_ROWS,
    )
    .await;
    let view = match view {
        Ok(view) => view,
        Err(OpenFailure::Cli(failure)) => return page.failed(provider.as_str(), failure).await,
        Err(OpenFailure::State(TaskError::Usage(message))) => {
            return plain(StatusCode::BAD_REQUEST, format!("Filters: {message}."))
        }
        Err(OpenFailure::State(error)) => return task_refused(&error),
    };
    let label = text(source, "label");
    let shown_preset = view
        .preset
        .clone()
        .or_else(|| Some(preset_name.clone()).filter(|name| !name.is_empty()));
    let mut context = Context::new();
    context.insert("source", source);
    context.insert("rows", &view.rows);
    context.insert("filters", &filter_fields(&view.filters));
    context.insert("preset", &shown_preset);
    context.insert("default_applied", &view.preset.is_some());
    context.insert("truncated", &view.truncated);
    context.insert("presets", &view.presets);
    page.check.offer(&mut context);
    page.show(
        VIEW_OPEN,
        "Open items",
        provider.as_str(),
        context,
        vec![
            crumbs_root(),
            Breadcrumb::link(label, format!("/tasks/{}", provider.as_str())),
            Breadcrumb::current("Open items"),
        ],
    )
    .await
}

/// A provider the facade serves, from an action's path (`404` otherwise).
fn facade_provider(check: &Check, raw: &str) -> Option<ProviderId> {
    // Any digit id makes `item_target` check only the provider.
    item_target(check, raw, "0")
        .map(|(provider, _)| provider)
        .filter(|provider| provider.as_str() == PROVIDER)
}

fn open_location(provider: &ProviderId) -> String {
    format!("/tasks/{}/open", provider.as_str())
}

/// `POST /tasks/{provider}/open/presets`: save the filters under `preset`.
pub(super) async fn preset_save(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<FilterFields>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let Some(provider) = facade_provider(&check, &path) else {
        return not_found();
    };
    let Some(form) = form else {
        return plain(StatusCode::BAD_REQUEST, "The form could not be read.");
    };
    let name = form.preset.trim().to_owned();
    if !valid_preset_name(&name) {
        return plain(
            StatusCode::BAD_REQUEST,
            "Preset names are 1-32 of a-z, 0-9, _ and -.",
        );
    }
    let filters = match parse_filters(&form, &check) {
        Ok(filters) if filters.is_empty() => {
            return plain(
                StatusCode::BAD_REQUEST,
                "A preset needs at least one filter.",
            )
        }
        Ok(filters) => filters,
        Err(message) => return plain(StatusCode::BAD_REQUEST, format!("Filters: {message}.")),
    };
    let Some((tm, _)) = state::facade(&request) else {
        return state::unavailable();
    };
    let saved = name.clone();
    match on_runtime(move || async move { tm.save_preset(&saved, filters).await }).await {
        Ok(()) => see_other(&format!("{}?preset={name}", open_location(&provider))),
        Err(error) => task_refused(&error),
    }
}

/// The checked preset name of a preset action's path, or nothing (a 404).
fn preset_target(
    check: &Check,
    raw_provider: &str,
    raw_name: &str,
) -> Option<(ProviderId, String)> {
    let provider = facade_provider(check, raw_provider)?;
    valid_preset_name(raw_name).then(|| (provider, raw_name.to_owned()))
}

/// `POST /tasks/{provider}/open/presets/{name}/delete`.
pub(super) async fn preset_delete(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<super::CsrfForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_name) = path.into_inner();
    let Some((provider, name)) = preset_target(&check, &raw_provider, &raw_name) else {
        return not_found();
    };
    let Some((tm, _)) = state::facade(&request) else {
        return state::unavailable();
    };
    match on_runtime(move || async move { tm.delete_preset(&name).await }).await {
        Ok(()) => see_other(&open_location(&provider)),
        Err(error) => task_refused(&error),
    }
}

/// `POST /tasks/{provider}/open/presets/{name}/default`: apply it when no filter is given.
pub(super) async fn preset_default(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<super::CsrfForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_name) = path.into_inner();
    let Some((provider, name)) = preset_target(&check, &raw_provider, &raw_name) else {
        return not_found();
    };
    let Some((tm, _)) = state::facade(&request) else {
        return state::unavailable();
    };
    match on_runtime(move || async move { tm.set_default_preset(Some(&name)).await }).await {
        Ok(()) => see_other(&open_location(&provider)),
        Err(error) => task_refused(&error),
    }
}

/// `POST /tasks/{provider}/open/default/clear`: no preset applies on its own.
pub(super) async fn preset_clear_default(
    request: HttpRequest,
    session: Session,
    path: web::Path<String>,
    form: Option<web::Form<super::CsrfForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let Some(provider) = facade_provider(&check, &path) else {
        return not_found();
    };
    let Some((tm, _)) = state::facade(&request) else {
        return state::unavailable();
    };
    match on_runtime(move || async move { tm.set_default_preset(None).await }).await {
        Ok(()) => see_other(&open_location(&provider)),
        Err(error) => task_refused(&error),
    }
}

pub(super) fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(open_page)
        .service(web::resource("/tasks/{provider}/open/presets").route(web::post().to(preset_save)))
        .service(
            web::resource("/tasks/{provider}/open/presets/{name}/delete")
                .route(web::post().to(preset_delete)),
        )
        .service(
            web::resource("/tasks/{provider}/open/presets/{name}/default")
                .route(web::post().to(preset_default)),
        )
        .service(
            web::resource("/tasks/{provider}/open/default/clear")
                .route(web::post().to(preset_clear_default)),
        );
}
