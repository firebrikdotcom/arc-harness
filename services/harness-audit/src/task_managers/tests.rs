use super::store::AssociationSource;
use super::*;
use crate::domain::task_agent::events::{AgentOwner, AgentState};
use crate::domain::task_preset::events::AgentFilter;
use diesel::connection::SimpleConnection;
use diesel::{Connection, SqliteConnection};
use serde_json::json;
use std::path::PathBuf;

fn database() -> (String, PathBuf) {
    let path = std::env::temp_dir().join(format!("harness-tasks-{}.sqlite", uuid::Uuid::new_v4()));
    let url = path.to_str().unwrap().to_string();
    SqliteConnection::establish(&url)
        .unwrap()
        .batch_execute(include_str!(
            "../../migrations/00000000000000_arc_base/up.sql"
        ))
        .unwrap();
    (url, path)
}

fn repos() -> Vec<String> {
    vec!["app".into(), "site".into()]
}

fn tmp() -> PathBuf {
    std::env::temp_dir()
}

fn new_agent() -> NewAgent {
    NewAgent {
        agent_kind: AgentKind::Codex,
        repo: "app".into(),
        worktree: tmp().to_str().unwrap().into(),
        branch: Some("tm/agent".into()),
        created_worktree: false,
        session_id: "session-1".into(),
        pid: 4242,
        code: None,
    }
}

#[tokio::test]
async fn associate_refuses_unknown_repo_and_bad_dirs() {
    let (url, _) = database();
    let tm = TaskManagers::open(&url).await.unwrap();
    let error = tm
        .associate("100", "other", None, &repos())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "usage");
    let error = tm
        .associate("100", "app", Some(Path::new("relative/dir")), &repos())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "usage");
    let missing = tmp().join(format!("missing-{}", uuid::Uuid::new_v4()));
    let error = tm
        .associate("100", "app", Some(&missing), &repos())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "usage");
    let error = tm
        .associate("10x", "app", None, &repos())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "usage");
    assert!(tm.store().association("100").unwrap().is_none());
}

#[tokio::test]
async fn association_survives_restart_and_unassociate_restores_default() {
    let (url, path) = database();
    let tm = TaskManagers::open(&url).await.unwrap();
    let stored = tm
        .associate("100", "app", Some(&tmp()), &repos())
        .await
        .unwrap();
    assert_eq!(stored.source, AssociationSource::Stored);
    assert_eq!(stored.dir.as_deref(), tmp().to_str());

    // A new store over the same database, with the index deleted, catches up from events.
    drop(tm);
    std::fs::remove_file(format!("{}.tasks.sqlite", path.display())).unwrap();
    let store = TaskStore::open(&url).unwrap();
    let resolved = store
        .resolve_association("100", Some("site"))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.repo, "app");
    assert_eq!(resolved.source, AssociationSource::Stored);

    let tm = TaskManagers::open(&url).await.unwrap();
    tm.unassociate("100").await.unwrap();
    let resolved = tm
        .store()
        .resolve_association("100", Some("site"))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.repo, "site");
    assert_eq!(resolved.source, AssociationSource::Default);
    assert!(tm
        .store()
        .resolve_association("100", None)
        .unwrap()
        .is_none());
    // Removing again is a no-op.
    tm.unassociate("100").await.unwrap();
}

#[tokio::test]
async fn second_live_agent_refused_until_first_ends() {
    let (url, _) = database();
    let tm = TaskManagers::open(&url).await.unwrap();
    let first = tm.start_agent("55", new_agent(), &repos()).await.unwrap();
    assert_eq!(first.generation, 1);
    assert_eq!(first.state, AgentState::Running);
    assert_eq!(first.owner, Some(AgentOwner::Service));
    assert_eq!(first.session_id, "session-1");
    assert!(valid_code(&first.code));

    let error = tm
        .start_agent("55", new_agent(), &repos())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "conflict");

    let attached = tm
        .change_agent(&first.code, AgentChange::Attach { force: true })
        .await
        .unwrap();
    assert_eq!(attached.owner, Some(AgentOwner::You));
    let ended = tm
        .change_agent(&first.code, AgentChange::Stop)
        .await
        .unwrap();
    assert_eq!(ended.state, AgentState::Stopped);
    assert!(tm.store().live_agent_for_item("55").unwrap().is_none());
    assert_eq!(
        tm.change_agent(&first.code, AgentChange::Stop)
            .await
            .unwrap_err()
            .kind(),
        "not_found"
    );

    let second = tm.start_agent("55", new_agent(), &repos()).await.unwrap();
    assert_eq!(second.generation, 2);
    let done = tm
        .change_agent(
            &second.code,
            AgentChange::Done {
                pr_url: "https://github.com/o/r/pull/9".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        done.pr_url.as_deref(),
        Some("https://github.com/o/r/pull/9")
    );
    assert_eq!(tm.store().agents().unwrap().len(), 2);

    let mut other = new_agent();
    other.repo = "unlisted".into();
    assert_eq!(
        tm.start_agent("56", other, &repos())
            .await
            .unwrap_err()
            .kind(),
        "usage"
    );
}

#[tokio::test]
async fn codes_unique_among_live_agents_and_visible_across_processes() {
    let (url, _) = database();
    let tm = TaskManagers::open(&url).await.unwrap();
    let mut codes = HashSet::new();
    for item in 1..=12 {
        let agent = tm
            .start_agent(&item.to_string(), new_agent(), &repos())
            .await
            .unwrap();
        assert!(codes.insert(agent.code.clone()));
        assert_eq!(
            tm.store()
                .agent_by_code(&agent.code)
                .unwrap()
                .unwrap()
                .item_id,
            item.to_string()
        );
    }
    // Another process over the same database sees them all.
    let other = TaskManagers::open(&url).await.unwrap();
    let live: HashSet<String> = other
        .store()
        .agents()
        .unwrap()
        .into_iter()
        .filter(|a| a.state.is_live())
        .map(|a| a.code)
        .collect();
    assert_eq!(live, codes);
    // The index refuses a duplicate live code even if one were committed.
    let code = codes.iter().next().unwrap();
    let mut conn = SqliteConnection::establish(&url).unwrap();
    conn.batch_execute(&format!(
        "INSERT INTO events(event_id,aggregate_type,aggregate_id,sequence,event_type,payload,timestamp,actor_id,timestamp_utc_us,correlation_id) VALUES('dup','TaskAgent','basecamp:99',1,'TaskAgentStarted','{}',1,'system',1,'dup')",
        json!({"generation":1,"code":code,"agent_kind":"claude","repo":"app","worktree":"/tmp","branch":null,"created_worktree":false,"session_id":"s","pid":1,"started_at":1})
    ))
    .unwrap();
    assert!(other.refresh().is_err());
}

#[tokio::test]
async fn presets_and_open_items_use_the_default() {
    let (url, _) = database();
    let tm = TaskManagers::open(&url).await.unwrap();
    assert_eq!(
        tm.save_preset("Bad", TaskFilters::default())
            .await
            .unwrap_err()
            .kind(),
        "usage"
    );
    assert_eq!(
        tm.set_default_preset(Some("nope"))
            .await
            .unwrap_err()
            .kind(),
        "not_found"
    );
    let idle = TaskFilters {
        agent: Some(AgentFilter::NoAgent),
        repo: Some("app".into()),
        ..TaskFilters::default()
    };
    tm.save_preset("idle-app", idle.clone()).await.unwrap();
    tm.set_default_preset(Some("idle-app")).await.unwrap();
    let default = tm.store().default_preset().unwrap().unwrap();
    assert_eq!(default.filters, idle);

    tm.associate("100", "app", None, &repos()).await.unwrap();
    tm.start_agent("2", new_agent(), &repos()).await.unwrap();
    let walk = json!({"items": [
        {"id": "1", "project": {"id": "9"}, "list": {"id": "100"}, "status": "open", "assignees": []},
        {"id": "2", "project": {"id": "9"}, "list": {"id": "100"}, "status": "open", "assignees": []},
        {"id": "3", "project": {"id": "9"}, "list": {"id": "200"}, "status": "open", "assignees": []},
        {"id": "4", "project": {"id": "9"}, "list": {"id": "300"}, "status": "open", "assignees": []},
        {"id": "5", "project": {"id": "9"}, "list": {"id": "300"}, "status": "done", "assignees": []}
    ], "truncated": false});
    // List 300 maps to app through the configured default only.
    let defaults = |list: &str| (list == "300").then(|| "app".to_string());
    let page = tm
        .open_items(
            &walk,
            &TaskFilters::default(),
            None,
            "2026-11-14",
            &defaults,
            50,
        )
        .unwrap();
    let ids: Vec<&str> = page
        .items
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["1", "4"]);
    assert_eq!(page.preset.as_deref(), Some("idle-app"));

    tm.delete_preset("idle-app").await.unwrap();
    assert!(tm.store().default_preset().unwrap().is_none());
    assert!(tm.store().presets().unwrap().is_empty());
}
