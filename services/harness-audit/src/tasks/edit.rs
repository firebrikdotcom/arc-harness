//! Create and edit: new lists and to-dos, item edits, the full description,
//! and list associations. Free text reaches the CLI only as private files.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use actix_session::Session;
use actix_web::http::StatusCode;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use super::state::{self, blocking, on_runtime, task_refused};
use super::{
    act, acted, item_target, not_found, plain, words, Check, Failure, NumId, Outcome, ProviderId,
    Tasks, TextFile, READ_TIMEOUT,
};
use crate::task_managers::agents::worktree;
use crate::task_managers::{valid_date, TaskError, PROVIDER};

/// A list name or a to-do title, in characters.
pub(super) const NAME_LIMIT: usize = 200;
/// A description, in characters.
pub(super) const DESCRIPTION_LIMIT: usize = 60_000;
/// Room for [`DESCRIPTION_LIMIT`] four-byte characters, percent-encoded, plus the rest.
pub(super) const DESCRIPTION_FORM_BYTES: usize = 1 << 20;
/// A `YYYY-MM-DD` date, in characters.
const DATE_LIMIT: usize = 10;
/// A directory path typed into an association or agent form.
const DIR_LIMIT: usize = 4096;

#[derive(Debug, Default, Deserialize)]
pub(super) struct ItemQuery {
    #[serde(default)]
    pub full: Option<String>,
}

impl ItemQuery {
    /// `?full=1`: the description up to 64 KiB instead of 4,000 characters.
    pub fn full(query: Option<&web::Query<Self>>) -> bool {
        query.is_some_and(|query| query.full.as_deref() == Some("1"))
    }
}

/// How an edit treats the due date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DueChange {
    Keep,
    Set(String),
    Clear,
}

/// What an edit changes; anything left out stays as it is.
#[derive(Debug, Clone)]
pub(super) struct ItemChange {
    pub title: Option<String>,
    pub description: Option<String>,
    pub due: DueChange,
    pub reopen: bool,
}

impl Tasks {
    pub(super) async fn item_full(self: Arc<Self>, provider: ProviderId, item: NumId) -> Outcome {
        let args = words(&[
            "get",
            "--provider",
            provider.as_str(),
            "--item",
            item.as_str(),
            "--full",
            "--json",
        ]);
        self.call(args, READ_TIMEOUT).await
    }

    pub(super) async fn list_create(
        self: Arc<Self>,
        provider: ProviderId,
        project: NumId,
        name: String,
        description: Option<String>,
    ) -> Outcome {
        let args = words(&[
            "list-create",
            "--provider",
            provider.as_str(),
            "--project",
            project.as_str(),
        ]);
        let mut files = vec![("--name-file", name)];
        files.extend(description.map(|text| ("--description-file", text)));
        let timeout = self.write_timeout;
        self.call_with_files(args, files, Vec::new(), timeout).await
    }

    pub(super) async fn todo_create(
        self: Arc<Self>,
        provider: ProviderId,
        list: NumId,
        title: String,
        description: Option<String>,
        due: Option<String>,
    ) -> Outcome {
        let args = words(&[
            "todo-create",
            "--provider",
            provider.as_str(),
            "--list",
            list.as_str(),
        ]);
        let mut files = vec![("--title-file", title)];
        files.extend(description.map(|text| ("--description-file", text)));
        let tail = due.map_or_else(Vec::new, |due| words(&["--due", &due]));
        let timeout = self.write_timeout;
        self.call_with_files(args, files, tail, timeout).await
    }

    pub(super) async fn todo_update(
        self: Arc<Self>,
        provider: ProviderId,
        item: NumId,
        change: ItemChange,
    ) -> Outcome {
        let args = words(&[
            "todo-update",
            "--provider",
            provider.as_str(),
            "--item",
            item.as_str(),
        ]);
        let mut files = Vec::new();
        files.extend(change.title.map(|text| ("--title-file", text)));
        files.extend(change.description.map(|text| ("--description-file", text)));
        let mut tail = match change.due {
            DueChange::Keep => Vec::new(),
            DueChange::Set(due) => words(&["--due", &due]),
            DueChange::Clear => words(&["--clear-due"]),
        };
        if change.reopen {
            tail.push("--reopen".into());
        }
        let timeout = self.write_timeout;
        self.call_with_files(args, files, tail, timeout).await
    }

    /// `args`, then each `(option, private file)`, then `tail`, then `--json`.
    /// Every file is removed when the call returns.
    async fn call_with_files(
        self: Arc<Self>,
        mut args: Vec<OsString>,
        files: Vec<(&'static str, String)>,
        tail: Vec<OsString>,
        timeout: Duration,
    ) -> Outcome {
        web::block(move || {
            let mut kept = Vec::with_capacity(files.len());
            for (option, body) in files {
                let file = TextFile::create(&body).map_err(|error| {
                    Failure::broken(format!(
                        "could not write the text to a private file: {error}"
                    ))
                })?;
                args.push(option.into());
                args.push(file.path().into());
                kept.push(file);
            }
            args.extend(tail);
            args.push("--json".into());
            let outcome = self.run(&args, timeout);
            drop(kept);
            outcome
        })
        .await
        .unwrap_or_else(|error| Err(Failure::broken(error.to_string())))
    }
}

// ---- form fields ------------------------------------------------------------

fn bad_request(message: impl Into<String>) -> HttpResponse {
    plain(StatusCode::BAD_REQUEST, message)
}

/// A list name or a to-do title: one trimmed line of 1 to 200 characters.
pub(super) fn one_line(raw: &str, what: &str) -> Result<String, HttpResponse> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("Give the {what}.")));
    }
    if text.chars().count() > NAME_LIMIT || text.chars().any(char::is_control) {
        return Err(bad_request(format!(
            "The {what} is one line of at most {NAME_LIMIT} characters."
        )));
    }
    Ok(text.to_owned())
}

/// An optional description: empty means none (or unchanged), at most 60,000 characters.
pub(super) fn description(raw: &str) -> Result<Option<String>, HttpResponse> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > DESCRIPTION_LIMIT || text.contains('\0') {
        return Err(bad_request(format!(
            "The description is limited to {DESCRIPTION_LIMIT} characters."
        )));
    }
    Ok(Some(text.to_owned()))
}

/// An optional `YYYY-MM-DD` date.
pub(super) fn due_date(raw: &str) -> Result<Option<String>, HttpResponse> {
    match raw.trim() {
        "" => Ok(None),
        day if valid_date(day) => Ok(Some(day.to_owned())),
        _ => Err(bad_request("A due date is YYYY-MM-DD.")),
    }
}

/// An optional absolute directory the owner typed (existence is the facade's check).
pub(super) fn directory(raw: &str) -> Result<Option<PathBuf>, HttpResponse> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > DIR_LIMIT
        || text.chars().any(char::is_control)
        || !Path::new(text).is_absolute()
    {
        return Err(bad_request("A directory is an absolute path."));
    }
    Ok(Some(PathBuf::from(text)))
}

/// A repository name from a form, kept only when `check` lists it as a git
/// repository.
pub(super) fn listed_repo(check: &Check, raw: &str) -> Result<Option<String>, HttpResponse> {
    match raw.trim() {
        "" => Ok(None),
        repo if check.repositories.iter().any(|found| found.name == repo) => {
            Ok(Some(repo.to_owned()))
        }
        _ => Err(bad_request("Choose a configured repository.")),
    }
}

/// A value the edit form was prefilled with, trimmed, at most `limit` characters.
fn prefilled(raw: &str, limit: usize) -> Result<&str, HttpResponse> {
    let text = raw.trim();
    if text.chars().count() > limit {
        return Err(bad_request("The form could not be read."));
    }
    Ok(text)
}

/// A checkbox: present (any value) when ticked.
fn ticked(value: &Option<String>) -> bool {
    value.is_some()
}

// ---- associations for the project and list pages -----------------------------

/// Each list's association (stored, else the configured default), as
/// `{repo, dir, source}`; `None` when the state is unavailable for this source.
pub(super) async fn associations(
    request: &HttpRequest,
    check: &Arc<Check>,
    provider: &ProviderId,
    lists: Vec<String>,
) -> Option<BTreeMap<String, Value>> {
    if provider.as_str() != PROVIDER {
        return None;
    }
    let (tm, _) = state::facade(request)?;
    let check = check.clone();
    let provider = provider.clone();
    let found = blocking(move || {
        tm.refresh()?;
        let mut found = BTreeMap::new();
        for list in lists {
            let default = check.mapped_repo(&provider, &list).map(str::to_owned);
            let association = tm
                .store()
                .resolve_association(&list, default.as_deref())
                .map_err(|error| TaskError::Internal(error.to_string()))?;
            if let Some(association) = association {
                found.insert(
                    list,
                    json!({
                        "repo_label": check.label(&association.repo),
                        "repo": association.repo,
                        "dir": association.dir,
                        "source": association.source,
                    }),
                );
            }
        }
        Ok(found)
    })
    .await;
    match found {
        Ok(found) => Some(found),
        Err(error) => {
            tracing::warn!(error = %error, "task manager associations could not be read");
            None
        }
    }
}

// ---- actions ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct NewListForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct NewTodoForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    due: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct EditForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    due: String,
    /// The title and due date the form was prefilled with: compared, never sent.
    #[serde(default)]
    title_was: String,
    #[serde(default)]
    due_was: String,
    clear_due: Option<String>,
    reopen: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AssociateForm {
    #[serde(default)]
    csrf_token: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    dir: String,
    /// The project page the form was sent from, to return to it.
    #[serde(default)]
    back_project: String,
}

/// A provider the CLI lists and a project or list id, or nothing (a 404).
fn target(check: &Check, provider: &str, id: &str) -> Option<(ProviderId, NumId)> {
    item_target(check, provider, id)
}

/// A created record's page, when its id is usable, else `fallback`.
fn created(document: &Value, key: &str, page: String, fallback: String) -> HttpResponse {
    match super::num_id(&document[key]["id"]) {
        Some(id) => super::see_other(&format!("{page}/{id}")),
        None => super::see_other(&fallback),
    }
}

/// `POST /tasks/{provider}/projects/{id}/lists`: create a to-do list.
pub(super) async fn list_create(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<NewListForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_project) = path.into_inner();
    let Some((provider, project)) = target(&check, &raw_provider, &raw_project) else {
        return not_found();
    };
    let Some(form) = form else {
        return bad_request("The form could not be read.");
    };
    let name = match one_line(&form.name, "list name") {
        Ok(name) => name,
        Err(response) => return response,
    };
    let description = match description(&form.description) {
        Ok(text) => text,
        Err(response) => return response,
    };
    let base = format!("/tasks/{}", provider.as_str());
    let back = format!("{base}/projects/{}", project.as_str());
    match tasks
        .list_create(provider, project, name, description)
        .await
    {
        Ok(document) => created(&document, "list", format!("{base}/lists"), back),
        Err(failure) => super::refused(&failure),
    }
}

/// `POST /tasks/{provider}/lists/{id}/todos`: create a to-do in the list.
pub(super) async fn todo_create(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<NewTodoForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_list) = path.into_inner();
    let Some((provider, list)) = target(&check, &raw_provider, &raw_list) else {
        return not_found();
    };
    let Some(form) = form else {
        return bad_request("The form could not be read.");
    };
    let fields = one_line(&form.title, "title")
        .and_then(|title| Ok((title, description(&form.description)?, due_date(&form.due)?)));
    let (title, description, due) = match fields {
        Ok(fields) => fields,
        Err(response) => return response,
    };
    let base = format!("/tasks/{}", provider.as_str());
    let back = format!("{base}/lists/{}", list.as_str());
    match tasks
        .todo_create(provider, list, title, description, due)
        .await
    {
        Ok(document) => created(&document, "item", format!("{base}/items"), back),
        Err(failure) => super::refused(&failure),
    }
}

/// `POST /tasks/{provider}/items/{id}/edit`: change the fields given; an empty
/// title or description, or a title or due date still as the form prefilled
/// it (`title_was`, `due_was`), leaves it as it is.
pub(super) async fn todo_update(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<EditForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (tasks, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_item) = path.into_inner();
    let Some((provider, item)) = item_target(&check, &raw_provider, &raw_item) else {
        return not_found();
    };
    let Some(form) = form else {
        return bad_request("The form could not be read.");
    };
    let was = prefilled(&form.title_was, NAME_LIMIT)
        .and_then(|title| Ok((title, prefilled(&form.due_was, DATE_LIMIT)?)));
    let (title_was, due_was) = match was {
        Ok(was) => was,
        Err(response) => return response,
    };
    let title = match form.title.trim() {
        "" => None,
        raw if raw == title_was => None,
        raw => match one_line(raw, "title") {
            Ok(title) => Some(title),
            Err(response) => return response,
        },
    };
    let description = match description(&form.description) {
        Ok(text) => text,
        Err(response) => return response,
    };
    let due = match (due_date(&form.due), ticked(&form.clear_due)) {
        (Err(response), _) => return response,
        (Ok(Some(day)), true) if day == due_was => DueChange::Clear,
        (Ok(Some(_)), true) => return bad_request("Set a due date or clear it, not both."),
        (Ok(Some(day)), false) if day == due_was => DueChange::Keep,
        (Ok(Some(day)), false) => DueChange::Set(day),
        (Ok(None), true) => DueChange::Clear,
        (Ok(None), false) => DueChange::Keep,
    };
    let change = ItemChange {
        title,
        description,
        due,
        reopen: ticked(&form.reopen),
    };
    if change.title.is_none()
        && change.description.is_none()
        && change.due == DueChange::Keep
        && !change.reopen
    {
        return bad_request("Nothing to change.");
    }
    let location = format!("/tasks/{}/items/{}", provider.as_str(), item.as_str());
    acted(tasks.todo_update(provider, item, change).await, &location)
}

/// The page an association form returns to: its project page, else the list.
fn association_back(provider: &ProviderId, list: &NumId, back_project: &str) -> String {
    match NumId::parse(back_project.trim()) {
        Some(project) => format!("/tasks/{}/projects/{}", provider.as_str(), project.as_str()),
        None => format!("/tasks/{}/lists/{}", provider.as_str(), list.as_str()),
    }
}

/// `POST /tasks/{provider}/lists/{id}/associate`: the list's repository and directory.
pub(super) async fn associate(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<AssociateForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_list) = path.into_inner();
    let Some((provider, list)) = target(&check, &raw_provider, &raw_list)
        .filter(|(provider, _)| provider.as_str() == PROVIDER)
    else {
        return not_found();
    };
    let Some(form) = form else {
        return bad_request("The form could not be read.");
    };
    let repo = match listed_repo(&check, &form.repo) {
        Ok(Some(repo)) => repo,
        Ok(None) => return bad_request("Choose a configured repository."),
        Err(response) => return response,
    };
    let dir = match directory(&form.dir) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let Some((tm, rt)) = state::facade(&request) else {
        return state::unavailable();
    };
    let back = association_back(&provider, &list, &form.back_project);
    let allowed = check.repos.clone();
    let list_id = list.as_str().to_owned();
    let done = on_runtime(move || async move {
        // A directory is checked against its repository when it is chosen,
        // and again when an agent starts in it.
        if let Some(dir) = dir.as_deref() {
            let path = check.repo_path(&repo).ok_or_else(|| {
                TaskError::Usage(format!(
                    "repos of task-managers.json has no path for {repo}"
                ))
            })?;
            worktree::require_in_repo(&rt, dir, path, check.label(&repo))?;
        }
        tm.associate(&list_id, &repo, dir.as_deref(), &allowed)
            .await
            .map(|_| ())
    })
    .await;
    match done {
        Ok(()) => super::see_other(&back),
        Err(error) => task_refused(&error),
    }
}

/// `POST /tasks/{provider}/lists/{id}/unassociate`: back to the configured default.
pub(super) async fn unassociate(
    request: HttpRequest,
    session: Session,
    path: web::Path<(String, String)>,
    form: Option<web::Form<AssociateForm>>,
) -> HttpResponse {
    let token = form.as_ref().map(|form| form.csrf_token.as_str());
    let (_, check) = match act(&request, &session, token).await {
        Ok(access) => access,
        Err(response) => return response,
    };
    let (raw_provider, raw_list) = path.into_inner();
    let Some((provider, list)) = target(&check, &raw_provider, &raw_list)
        .filter(|(provider, _)| provider.as_str() == PROVIDER)
    else {
        return not_found();
    };
    let Some((tm, _)) = state::facade(&request) else {
        return state::unavailable();
    };
    let back = association_back(
        &provider,
        &list,
        form.as_ref().map_or("", |form| form.back_project.as_str()),
    );
    let list_id = list.as_str().to_owned();
    match on_runtime(move || async move { tm.unassociate(&list_id).await }).await {
        Ok(()) => super::see_other(&back),
        Err(error) => task_refused(&error),
    }
}

/// The create, edit and association routes. Forms with a description get room for it.
pub(super) fn config(cfg: &mut web::ServiceConfig) {
    let long_form = || web::FormConfig::default().limit(DESCRIPTION_FORM_BYTES);
    cfg.service(
        web::resource("/tasks/{provider}/projects/{id}/lists")
            .app_data(long_form())
            .route(web::post().to(list_create)),
    )
    .service(
        web::resource("/tasks/{provider}/lists/{id}/todos")
            .app_data(long_form())
            .route(web::post().to(todo_create)),
    )
    .service(
        web::resource("/tasks/{provider}/items/{id}/edit")
            .app_data(long_form())
            .route(web::post().to(todo_update)),
    )
    .service(
        web::resource("/tasks/{provider}/lists/{id}/associate").route(web::post().to(associate)),
    )
    .service(
        web::resource("/tasks/{provider}/lists/{id}/unassociate")
            .route(web::post().to(unassociate)),
    );
}
