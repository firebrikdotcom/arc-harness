//! Pure open-item filtering over the Python driver's `open-walk` output:
//! `{items: [{id, project: {id}, list: {id}, assignees: [{id, name}], due_on,
//! status}], truncated}`. Filters combine with AND, done items never appear,
//! and the default preset applies when no filter is given.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use super::store::Preset;
use super::{valid_date, TaskError};
use crate::domain::task_preset::events::{AgentFilter, TaskFilters};

/// Most items one page returns.
pub const MAX_OPEN_LIMIT: usize = 500;

/// Caller-supplied facts the filters need.
pub struct OpenContext<'a> {
    /// The provider person id `me` resolves to.
    pub me: Option<&'a str>,
    /// Today as `YYYY-MM-DD`, for `overdue`.
    pub today: &'a str,
    /// A list's effective repository (stored association, else configured default).
    pub repo_for_list: &'a dyn Fn(&str) -> Option<String>,
    /// Item ids with a live agent.
    pub active_items: &'a HashSet<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenPage {
    pub items: Vec<Value>,
    /// The filters actually applied.
    pub filters: TaskFilters,
    /// The default preset's name when it supplied the filters.
    pub preset: Option<String>,
    pub truncated: bool,
}

/// `given` when it sets anything, else the default preset's filters.
pub fn effective_filters(
    given: &TaskFilters,
    default: Option<&Preset>,
) -> (TaskFilters, Option<String>) {
    match default {
        Some(preset) if given.is_empty() => (preset.filters.clone(), Some(preset.name.clone())),
        _ => (given.clone(), None),
    }
}

fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn ref_id(item: &Value, field: &str) -> Option<String> {
    id_of(&item[field]["id"]).or_else(|| id_of(&item[format!("{field}_id")]))
}

fn is_done(item: &Value) -> bool {
    item["status"] == "done" || item["done"] == true || item["completed"] == true
}

fn due_on(item: &Value) -> Option<&str> {
    item["due_on"]
        .as_str()
        .and_then(|due| due.get(..10))
        .filter(|due| valid_date(due))
}

fn assigned(item: &Value, assignee: &str, me: Option<&str>) -> bool {
    let Some(list) = item["assignees"].as_array() else {
        return false;
    };
    list.iter().any(|person| {
        if assignee == "me" {
            me.is_some_and(|me| id_of(&person["id"]).as_deref() == Some(me))
        } else {
            let name = person["name"].as_str().or_else(|| person.as_str());
            name.is_some_and(|name| name.trim().eq_ignore_ascii_case(assignee.trim()))
        }
    })
}

/// Whether a not-done item passes every set filter.
pub fn matches(item: &Value, filters: &TaskFilters, context: &OpenContext) -> bool {
    if is_done(item) {
        return false;
    }
    let Some(id) = id_of(&item["id"]) else {
        return false;
    };
    if let Some(project) = &filters.project {
        if ref_id(item, "project").as_ref() != Some(project) {
            return false;
        }
    }
    let list = ref_id(item, "list");
    if let Some(wanted) = &filters.list {
        if list.as_ref() != Some(wanted) {
            return false;
        }
    }
    if let Some(assignee) = &filters.assignee {
        if !assigned(item, assignee, context.me) {
            return false;
        }
    }
    if let Some(before) = &filters.due_before {
        if !due_on(item).is_some_and(|due| due < before.as_str()) {
            return false;
        }
    }
    if filters.overdue && !due_on(item).is_some_and(|due| due < context.today) {
        return false;
    }
    if let Some(repo) = &filters.repo {
        let effective = list.as_deref().and_then(context.repo_for_list);
        if effective.as_ref() != Some(repo) {
            return false;
        }
    }
    match filters.agent {
        Some(AgentFilter::Active) if !context.active_items.contains(&id) => return false,
        Some(AgentFilter::NoAgent) if context.active_items.contains(&id) => return false,
        _ => {}
    }
    true
}

/// Filter an `open-walk` result into at most `limit` items (clamped to
/// 1..=[`MAX_OPEN_LIMIT`]). `truncated` is set when more matched or the walk
/// itself was cut short.
pub fn open_items(
    walk: &Value,
    given: &TaskFilters,
    default: Option<&Preset>,
    context: &OpenContext,
    limit: usize,
) -> Result<OpenPage, TaskError> {
    given.validate().map_err(TaskError::Usage)?;
    if !valid_date(context.today) {
        return Err(TaskError::Usage("today must be a YYYY-MM-DD date".into()));
    }
    let (filters, preset) = effective_filters(given, default);
    if filters.assignee.as_deref() == Some("me") && context.me.is_none() {
        return Err(TaskError::Usage(
            "assignee me needs the provider identity".into(),
        ));
    }
    let items = walk["items"]
        .as_array()
        .ok_or_else(|| TaskError::Usage("open-walk output has no items array".into()))?;
    let limit = limit.clamp(1, MAX_OPEN_LIMIT);
    let mut selected = Vec::new();
    let mut truncated = walk["truncated"] == true;
    for item in items.iter().filter(|item| matches(item, &filters, context)) {
        if selected.len() == limit {
            truncated = true;
            break;
        }
        selected.push(item.clone());
    }
    Ok(OpenPage {
        items: selected,
        filters,
        preset,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn walk() -> Value {
        json!({"items": [
            {"id": "1", "project": {"id": "10"}, "list": {"id": "100"}, "status": "open",
             "assignees": [{"id": "7", "name": "Ana"}], "due_on": "2026-11-01"},
            {"id": "2", "project": {"id": "10"}, "list": {"id": "101"}, "status": "open",
             "assignees": [{"id": "8", "name": "Bo"}], "due_on": "2026-12-01"},
            {"id": "3", "project": {"id": "11"}, "list": {"id": "102"}, "status": "done",
             "assignees": [{"id": "7", "name": "Ana"}], "due_on": "2026-10-01"},
            {"id": "4", "project": {"id": "11"}, "list": {"id": "102"}, "status": "open",
             "assignees": [], "due_on": null}
        ], "truncated": false})
    }

    fn ids(page: &OpenPage) -> Vec<&str> {
        page.items
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect()
    }

    fn run(filters: TaskFilters, default: Option<&Preset>, limit: usize) -> OpenPage {
        let active: HashSet<String> = ["2".to_string()].into();
        let repo = |list: &str| (list == "100" || list == "102").then(|| "app".to_string());
        let context = OpenContext {
            me: Some("7"),
            today: "2026-11-14",
            repo_for_list: &repo,
            active_items: &active,
        };
        open_items(&walk(), &filters, default, &context, limit).unwrap()
    }

    #[test]
    fn done_items_never_appear() {
        assert_eq!(ids(&run(TaskFilters::default(), None, 50)), ["1", "2", "4"]);
        let ana = TaskFilters {
            assignee: Some("ana".into()),
            ..TaskFilters::default()
        };
        assert_eq!(ids(&run(ana, None, 50)), ["1"]);
    }

    #[test]
    fn filters_combine_with_and() {
        let project = TaskFilters {
            project: Some("10".into()),
            ..TaskFilters::default()
        };
        assert_eq!(ids(&run(project.clone(), None, 50)), ["1", "2"]);
        let both = TaskFilters {
            repo: Some("app".into()),
            ..project.clone()
        };
        assert_eq!(ids(&run(both, None, 50)), ["1"]);
        let none = TaskFilters {
            assignee: Some("me".into()),
            agent: Some(AgentFilter::Active),
            ..project
        };
        assert!(run(none, None, 50).items.is_empty());
        let overdue = TaskFilters {
            overdue: true,
            ..TaskFilters::default()
        };
        assert_eq!(ids(&run(overdue, None, 50)), ["1"]);
        let before = TaskFilters {
            due_before: Some("2026-12-02".into()),
            agent: Some(AgentFilter::NoAgent),
            ..TaskFilters::default()
        };
        assert_eq!(ids(&run(before, None, 50)), ["1"]);
    }

    #[test]
    fn default_preset_applies_only_without_filters() {
        let preset = Preset {
            name: "mine".into(),
            filters: TaskFilters {
                assignee: Some("me".into()),
                ..TaskFilters::default()
            },
            is_default: true,
        };
        let page = run(TaskFilters::default(), Some(&preset), 50);
        assert_eq!(ids(&page), ["1"]);
        assert_eq!(page.preset.as_deref(), Some("mine"));
        let given = TaskFilters {
            list: Some("101".into()),
            ..TaskFilters::default()
        };
        let page = run(given, Some(&preset), 50);
        assert_eq!(ids(&page), ["2"]);
        assert_eq!(page.preset, None);
    }

    #[test]
    fn bounded_result_is_truncated() {
        let page = run(TaskFilters::default(), None, 2);
        assert_eq!(ids(&page), ["1", "2"]);
        assert!(page.truncated);
        assert!(!run(TaskFilters::default(), None, 3).truncated);
    }
}
