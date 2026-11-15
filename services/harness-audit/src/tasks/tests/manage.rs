//! Create, edit, associations, open items and attachable agents: what each
//! new route hands the CLI or the facade, and what it refuses first.

use super::*;
use crate::domain::task_agent::events::AgentState;
use crate::task_managers::agents::process;
use crate::task_managers::valid_code;

fn sh(dir: &Path, line: &str) {
    let status = Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{line}");
}

/// The last call of a command, e.g. the `get` among a page's reads.
fn last_call(fixture: &Fixture, command: &str) -> Vec<String> {
    fixture
        .calls()
        .into_iter()
        .rev()
        .find(|call| call[0] == command)
        .unwrap_or_default()
}

/// Which of the open-walk fixture's items a page links to.
fn item_links(html: &str) -> Vec<&'static str> {
    ["40", "41", "42", "43"]
        .into_iter()
        .filter(|id| html.contains(&format!("href=\"/tasks/basecamp/items/{id}\"")))
        .collect()
}

#[actix_web::test]
async fn creates_and_edits_hand_the_cli_exact_argv_with_text_in_private_files() {
    let fixture = Fixture::new(true);
    let app = service!(fixture);
    let (cookie, token) = signed_in!(app);
    let name = "Sprint <b>12</b> --json";
    let description = "line one\nline two; $(id) --project 99";
    let (status, location, _) = post!(
        app,
        loopback(),
        "/tasks/basecamp/projects/10/lists",
        Some(cookie.clone()),
        Some(vec![
            ("csrf_token", token.as_str()),
            ("name", name),
            ("description", description),
        ])
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/tasks/basecamp/lists/25");
    let call = fixture.calls().last().unwrap().clone();
    assert_eq!(
        call[..6],
        [
            "list-create",
            "--provider",
            "basecamp",
            "--project",
            "10",
            "--name-file"
        ]
    );
    assert_eq!(
        (call.len(), call[7].as_str(), call[9].as_str()),
        (10, "--description-file", "--json")
    );
    let texts = fixture.log("text.log");
    for (entry, path, expected) in [
        (&texts[0], &call[6], name),
        (&texts[1], &call[8], description),
    ] {
        assert_eq!(entry["text"], expected);
        assert_eq!(entry["mode"], "0o600");
        assert_eq!(entry["path"], path.as_str());
        assert!(
            !Path::new(path).exists(),
            "the file is removed after the call"
        );
    }
    // Without a description only the name travels.
    let (status, _, _) = post!(
        app,
        loopback(),
        "/tasks/basecamp/projects/10/lists",
        Some(cookie.clone()),
        Some(vec![("csrf_token", token.as_str()), ("name", "Plain")])
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    let call = fixture.calls().last().unwrap().clone();
    assert_eq!(
        (call.len(), call[5].as_str(), call[7].as_str()),
        (8, "--name-file", "--json")
    );

    let (status, location, _) = post!(
        app,
        loopback(),
        "/tasks/basecamp/lists/20/todos",
        Some(cookie.clone()),
        Some(vec![
            ("csrf_token", token.as_str()),
            ("title", "Fix <i>it</i>"),
            ("description", "body text"),
            ("due", "2026-12-24"),
        ])
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/tasks/basecamp/items/32");
    let call = fixture.calls().last().unwrap().clone();
    assert_eq!(
        call[..6],
        [
            "todo-create",
            "--provider",
            "basecamp",
            "--list",
            "20",
            "--title-file"
        ]
    );
    assert_eq!(call[7], "--description-file");
    assert_eq!(call[9..], ["--due", "2026-12-24", "--json"]);

    type Edit<'a> = (Vec<(&'a str, &'a str)>, Vec<&'a str>);
    let edits: [Edit; 7] = [
        // Only the due date changed: the prefilled title is not sent back.
        (
            vec![
                ("title", "Shown title"),
                ("title_was", "Shown title"),
                ("due", "2027-01-02"),
                ("due_was", "2026-12-01"),
            ],
            vec!["--due", "2027-01-02", "--json"],
        ),
        // Only the title changed: the prefilled date is not sent back.
        (
            vec![
                ("title", "Renamed"),
                ("title_was", "Shown title"),
                ("due", "2026-12-01"),
                ("due_was", "2026-12-01"),
            ],
            vec!["--title-file", "*", "--json"],
        ),
        // The clear box wins over a date still as it was prefilled.
        (
            vec![
                ("title", " Shown title "),
                ("title_was", "Shown title"),
                ("due", "2026-12-01"),
                ("due_was", "2026-12-01"),
                ("clear_due", "on"),
            ],
            vec!["--clear-due", "--json"],
        ),
        // Unchanged title and date: only what else changed travels.
        (
            vec![
                ("title", "Shown title"),
                ("title_was", "Shown title"),
                ("due", "2026-12-01"),
                ("due_was", "2026-12-01"),
                ("reopen", "on"),
            ],
            vec!["--reopen", "--json"],
        ),
        (
            vec![("title", "Renamed"), ("clear_due", "on"), ("reopen", "on")],
            vec!["--title-file", "*", "--clear-due", "--reopen", "--json"],
        ),
        (
            vec![("title", ""), ("due", "2027-01-02")],
            vec!["--due", "2027-01-02", "--json"],
        ),
        (
            vec![("description", "new body")],
            vec!["--description-file", "*", "--json"],
        ),
    ];
    for (fields, tail) in edits {
        let mut form = vec![("csrf_token", token.as_str())];
        form.extend(fields);
        let (status, location, body) = post!(
            app,
            loopback(),
            "/tasks/basecamp/items/30/edit",
            Some(cookie.clone()),
            Some(form)
        );
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert_eq!(location, "/tasks/basecamp/items/30");
        let call = fixture.calls().last().unwrap().clone();
        assert_eq!(
            call[..5],
            ["todo-update", "--provider", "basecamp", "--item", "30"]
        );
        assert_eq!(call.len(), 5 + tail.len(), "{call:?}");
        for (word, expected) in call[5..].iter().zip(&tail) {
            if *expected == "*" {
                assert!(Path::new(word).is_absolute(), "{word}");
            } else {
                assert_eq!(word, expected);
            }
        }
    }
    for call in fixture.calls() {
        for word in call {
            for text in [
                "Sprint",
                "line one",
                "Plain",
                "Fix",
                "body text",
                "Renamed",
                "Shown title",
                "new body",
            ] {
                assert!(!word.contains(text), "{word}");
            }
        }
    }

    // The full description is a separate read, and the short view offers it.
    let (status, html) = get!(app, loopback(), "/tasks/basecamp/items/30?full=1");
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("FULL-END"));
    assert_eq!(
        last_call(&fixture, "get"),
        [
            "get",
            "--provider",
            "basecamp",
            "--item",
            "30",
            "--full",
            "--json"
        ]
    );
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/items/30");
    assert!(html.contains("href=\"/tasks/basecamp/items/30?full=1\""));
    assert!(html.contains("action=\"/tasks/basecamp/items/30/edit\""));
    assert!(html.contains("name=\"due_was\" value=\"2026-12-01\""));
    assert!(html.contains(
        "name=\"title_was\" value=\"&lt;script&gt;alert(1)&lt;&#x2F;script&gt; &amp; co\""
    ));
    assert!(!html.contains("FULL-END"));
    assert_eq!(
        last_call(&fixture, "get"),
        ["get", "--provider", "basecamp", "--item", "30", "--json"]
    );
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/projects/10");
    assert!(html.contains("action=\"/tasks/basecamp/projects/10/lists\""));
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/lists/20");
    assert!(html.contains("action=\"/tasks/basecamp/lists/20/todos\""));
}

#[actix_web::test]
async fn bad_new_inputs_never_reach_the_cli_or_the_facade() {
    let fixture = Fixture::new(true);
    let app = service!(fixture);
    let (cookie, token) = signed_in!(app);
    let before = fixture.calls().len();
    let long_preset = format!("/tasks/basecamp/open/presets/{}/default", "a".repeat(33));
    for uri in [
        "/tasks/basecamp/projects/abc/lists",
        "/tasks/Basecamp/projects/10/lists",
        "/tasks/basecamp/lists/--list/todos",
        "/tasks/basecamp/items/1%3Brm/edit",
        "/tasks/basecamp/lists/abc/associate",
        "/tasks/linear/lists/20/associate",
        "/tasks/basecamp/lists/1.5/unassociate",
        "/tasks/linear/open/presets",
        "/tasks/basecamp/open/presets/Mine/delete",
        "/tasks/basecamp/open/presets/a%20b/default",
        long_preset.as_str(),
        "/tasks/unlisted/open/default/clear",
        "/tasks/basecamp/items/abc/agent",
        "/tasks/linear/items/30/agent",
        "/tasks/agents/tm-ABC123/stop",
        "/tasks/agents/tm-abc12/handback",
        "/tasks/agents/tm-abc1234/done",
        "/tasks/agents/xx-abc123/stop",
        "/tasks/agents/tm-abcxyz/done",
    ] {
        let (status, _, _) = post!(
            app,
            loopback(),
            uri,
            Some(cookie.clone()),
            Some(full_form(&token))
        );
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let long_name = "n".repeat(edit::NAME_LIMIT + 1);
    let long_text = "d".repeat(edit::DESCRIPTION_LIMIT + 1);
    let lists = "/tasks/basecamp/projects/10/lists";
    let todos = "/tasks/basecamp/lists/20/todos";
    let item_edit = "/tasks/basecamp/items/30/edit";
    let associate = "/tasks/basecamp/lists/20/associate";
    let presets = "/tasks/basecamp/open/presets";
    let agent = "/tasks/basecamp/items/30/agent";
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (lists, vec![("name", "")]),
        (lists, vec![("name", "two\nlines")]),
        (lists, vec![("name", long_name.as_str())]),
        (
            lists,
            vec![("name", "ok"), ("description", long_text.as_str())],
        ),
        (todos, vec![("title", "ok"), ("due", "2026-13-01")]),
        (todos, vec![("title", "ok"), ("due", "tomorrow")]),
        (todos, vec![("title", " ")]),
        (item_edit, vec![]),
        (item_edit, vec![("due", "2026-12-01"), ("clear_due", "on")]),
        (item_edit, vec![("due", "2026-02-30")]),
        (item_edit, vec![("title", long_name.as_str())]),
        // A date other than the prefilled one, with the clear box ticked.
        (
            item_edit,
            vec![
                ("due", "2026-12-02"),
                ("due_was", "2026-12-01"),
                ("clear_due", "on"),
            ],
        ),
        // Nothing differs from what the form was prefilled with.
        (
            item_edit,
            vec![
                ("title", "As shown"),
                ("title_was", "As shown"),
                ("due", "2026-12-01"),
                ("due_was", "2026-12-01"),
            ],
        ),
        (
            item_edit,
            vec![("title", "ok"), ("title_was", long_name.as_str())],
        ),
        (item_edit, vec![("title", "ok"), ("due_was", "2026-12-01x")]),
        (associate, vec![("repo", "unlisted")]),
        (associate, vec![("repo", "../bad")]),
        (associate, vec![("repo", "")]),
        (associate, vec![("repo", "arc"), ("dir", "relative/dir")]),
        (presets, vec![("preset", "Bad Name"), ("assignee", "me")]),
        (presets, vec![("preset", "mine")]),
        (presets, vec![("preset", "mine"), ("due_before", "soon")]),
        (presets, vec![("preset", "mine"), ("repo", "unlisted")]),
        (presets, vec![("preset", "mine"), ("agent", "maybe")]),
        (presets, vec![("preset", "mine"), ("project", "10;rm")]),
        (agent, vec![("agent", "gpt")]),
        (agent, vec![("agent", "claude"), ("repo", "unlisted")]),
        (agent, vec![("agent", "claude"), ("dir", "rel")]),
    ];
    for (uri, fields) in cases {
        let mut form = vec![("csrf_token", token.as_str())];
        form.extend(fields.iter().copied());
        let (status, _, body) = post!(app, loopback(), uri, Some(cookie.clone()), Some(form));
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {fields:?} {body}");
    }
    let long_assignee = format!("/tasks/basecamp/open?assignee={}", "a".repeat(101));
    for uri in [
        "/tasks/basecamp/open?due_before=2026-02-30",
        "/tasks/basecamp/open?preset=Mine",
        "/tasks/basecamp/open?project=abc",
        "/tasks/basecamp/open?list=--list",
        "/tasks/basecamp/open?agent=maybe",
        "/tasks/basecamp/open?repo=unlisted",
        "/tasks/basecamp/open?overdue=yes",
        long_assignee.as_str(),
    ] {
        assert_eq!(
            get!(app, loopback(), uri).0,
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
    assert_eq!(
        get!(app, loopback(), "/tasks/linear/open").0,
        StatusCode::NOT_FOUND
    );
    let calls = fixture.calls();
    assert!(only_checks(&calls[before..]), "{:?}", &calls[before..]);
    let state = fixture.state().await;
    assert!(state.store().presets().unwrap().is_empty());
    assert!(state.store().associations().unwrap().is_empty());
    assert!(state.store().agents().unwrap().is_empty());
}

#[actix_web::test]
async fn associations_are_stored_shown_and_removed() {
    let fixture = Fixture::new(true);
    let app = service!(fixture);
    let (cookie, token) = signed_in!(app);
    // The repository's checkout, a linked worktree of it, another clone of
    // the same origin, and a repository of its own.
    sh(
        &fixture.directory,
        "git init -q -b main ht && cd ht && git config user.name t && git config user.email t@example.com \
         && echo hi > README && git add README && git commit -qm init \
         && git remote add origin https://github.com/example/ht.git \
         && git worktree add -q -b feature ../ht-wt && cd .. \
         && git clone -q ht ht-clone && git -C ht-clone remote set-url origin git@github.com:example/ht.git \
         && git init -q foreign",
    );
    let checkout = fixture.directory.join("ht").display().to_string();
    for (name, said) in [
        ("foreign", "dir must be the checkout of harness-template at"),
        (
            "ht-clone",
            "dir is a separate clone of harness-template; use the checkout at",
        ),
        ("missing", "dir must be the checkout of harness-template at"),
    ] {
        let dir = fixture.directory.join(name).display().to_string();
        let (status, _, body) = post!(
            app,
            loopback(),
            "/tasks/basecamp/lists/20/associate",
            Some(cookie.clone()),
            Some(vec![
                ("csrf_token", token.as_str()),
                ("repo", "harness-template"),
                ("dir", dir.as_str()),
            ])
        );
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(body.contains(said), "{name}: {body}");
        assert!(body.contains(&checkout), "{name}: {body}");
        assert!(body.contains("linked worktree of it"), "{name}: {body}");
    }
    let state = fixture.state().await;
    assert!(state.store().association("20").unwrap().is_none());

    let dir = fixture.directory.join("ht-wt").display().to_string();
    let (status, location, body) = post!(
        app,
        loopback(),
        "/tasks/basecamp/lists/20/associate",
        Some(cookie.clone()),
        Some(vec![
            ("csrf_token", token.as_str()),
            ("repo", "harness-template"),
            ("dir", dir.as_str()),
            ("back_project", "10"),
        ])
    );
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(location, "/tasks/basecamp/projects/10");
    state.refresh().unwrap();
    let stored = state.store().association("20").unwrap().unwrap();
    assert_eq!(
        (stored.repo.as_str(), stored.dir.as_deref()),
        ("harness-template", Some(dir.as_str()))
    );

    let (_, html) = get!(app, loopback(), "/tasks/basecamp/projects/10");
    assert!(html.contains("action=\"/tasks/basecamp/lists/20/associate\""));
    assert!(html.contains("action=\"/tasks/basecamp/lists/20/unassociate\""));
    assert!(html.contains("<option value=\"harness-template\" selected>harness-template — "));
    assert!(html.contains("name=\"back_project\" value=\"10\""));
    // List 21 has neither a stored association nor a mapping.
    assert!(html.contains("Not associated"));
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/lists/20");
    assert!(html.contains("Repository harness-template"));

    // A directory that does not exist is refused.
    let (status, _, _) = post!(
        app,
        loopback(),
        "/tasks/basecamp/lists/20/associate",
        Some(cookie.clone()),
        Some(vec![
            ("csrf_token", token.as_str()),
            ("repo", "arc"),
            ("dir", "/nonexistent/harness-tasks-dir"),
        ])
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Removing it restores the configured default.
    let (status, location, _) = post!(
        app,
        loopback(),
        "/tasks/basecamp/lists/20/unassociate",
        Some(cookie.clone()),
        Some(vec![("csrf_token", token.as_str())])
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/tasks/basecamp/lists/20");
    state.refresh().unwrap();
    assert!(state.store().association("20").unwrap().is_none());
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/projects/10");
    assert!(html.contains("(configured default)"));
    assert!(!html.contains("/lists/20/unassociate"));
    assert!(fixture
        .calls()
        .iter()
        .all(|call| !call[0].contains("associate")));
}

#[actix_web::test]
async fn repositories_are_offered_by_their_git_identity() {
    let fixture = Fixture::with(
        true,
        r#"{"repos": ["arc", "harness-template", "odd", "../bad"], "repositories": [
            {"name": "arc", "path": os.path.join(here, "app"), "git": True, "slug": "example/arc", "label": "example/arc"},
            {"name": "harness-template", "path": os.path.join(here, "ht"), "git": False, "slug": None, "label": "ht"},
            {"name": "odd", "path": os.path.join(here, "odd"), "git": True, "slug": None, "label": "o\x01dd"},
            {"name": "../bad", "path": "/x", "git": True, "slug": None, "label": "bad"}]}"#,
        |_| (),
    );
    let app = service!(fixture);
    let (cookie, token) = signed_in!(app);
    // The label and the path are what a person reads; the configuration key is the value.
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/items/30");
    assert!(html.contains("<option value=\"arc\" selected>example&#x2F;arc — "));
    assert!(html.contains("&#x2F;app</option>"));
    // A label that cannot be shown falls back to the key.
    assert!(html.contains("<option value=\"odd\">odd — "));
    // An entry that is not a git repository is named, not offered.
    assert!(!html.contains("value=\"harness-template\""));
    assert!(html.contains("data-tm-repo-problems"));
    assert!(html.contains("harness-template: "));
    assert!(!html.contains("..&#x2F;bad") && !html.contains("../bad"));
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/open");
    assert!(html.contains("<option value=\"arc\">example&#x2F;arc — "));
    assert!(html.contains("<span class=\"mono tm-path\">example&#x2F;arc</span>"));
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/lists/20");
    assert!(html.contains("Repository example&#x2F;arc"));
    let (_, html) = get!(app, loopback(), "/tasks");
    assert!(html.contains("<span class=\"mono tm-path\">example&#x2F;arc</span>"));

    for repo in ["harness-template", "../bad", "example/arc"] {
        let (status, _, body) = post!(
            app,
            loopback(),
            "/tasks/basecamp/lists/20/associate",
            Some(cookie.clone()),
            Some(vec![("csrf_token", token.as_str()), ("repo", repo)])
        );
        assert_eq!(status, StatusCode::BAD_REQUEST, "{repo}");
        assert!(body.contains("Choose a configured repository."), "{body}");
    }
    let (status, _) = get!(
        app,
        loopback(),
        "/tasks/basecamp/open?repo=harness-template"
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A stored association whose key left the configuration shows the key.
    let state = fixture.state().await;
    state
        .associate("21", "gone", None, &["gone".to_string()])
        .await
        .unwrap();
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/projects/10");
    assert!(html.contains("<span class=\"mono tm-path\">gone</span>"));
    assert!(html.contains("<span class=\"mono tm-path\">example&#x2F;arc</span>"));
    assert!(html.contains("data-tm-repo-problems"));
}

#[actix_web::test]
async fn the_open_page_applies_the_default_preset_and_combines_filters() {
    let fixture = Fixture::new(true);
    let app = service!(fixture);
    let (cookie, token) = signed_in!(app);
    let form = |fields: &[(&'static str, &'static str)]| {
        let mut form = vec![("csrf_token", token.clone())];
        form.extend(fields.iter().map(|(key, value)| (*key, value.to_string())));
        form
    };
    macro_rules! submit {
        ($uri:expr, $fields:expr) => {{
            let fields = form($fields);
            let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
            post!(app, loopback(), $uri, Some(cookie.clone()), Some(fields))
        }};
    }

    // No filter and no default: every not-done item.
    let (status, html) = get!(app, loopback(), "/tasks/basecamp/open");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_links(&html), ["40", "41", "42"]);
    assert_eq!(
        last_call(&fixture, "open-walk"),
        [
            "open-walk",
            "--provider",
            "basecamp",
            "--limit",
            "1000",
            "--json"
        ]
    );
    assert!(last_call(&fixture, "me").is_empty());
    assert!(html.contains("&lt;b&gt;open one&lt;&#x2F;b&gt;"), "{html}");
    assert!(!html.contains("<b>open one"));
    assert!(html.contains("Ana &lt;i&gt;"));
    assert!(!html.contains("javascript:"));
    // Dispatch is offered where the list has a repository (list 20 maps to arc).
    assert!(html.contains("action=\"/tasks/basecamp/items/40/agent\""));
    assert!(!html.contains("/items/41/agent"));

    let (status, location, _) = submit!(
        "/tasks/basecamp/open/presets",
        &[("preset", "mine"), ("assignee", "me")]
    );
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, "/tasks/basecamp/open?preset=mine")
    );
    let (status, location, _) = submit!("/tasks/basecamp/open/presets/mine/default", &[]);
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, "/tasks/basecamp/open")
    );

    // Opened without a filter, the default preset applies.
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/open");
    assert_eq!(item_links(&html), ["40"]);
    assert!(html.contains("Preset mine (default)"));
    assert_eq!(
        last_call(&fixture, "me"),
        ["me", "--provider", "basecamp", "--json"]
    );
    // So does the section after the projects on the source page.
    let (status, html) = get!(app, loopback(), "/tasks/basecamp");
    assert_eq!(status, StatusCode::OK);
    let section = &html[html.find("data-tm-open-section").unwrap()..];
    assert!(
        html.find("/tasks/basecamp/projects/10").unwrap()
            < html.find("data-tm-open-section").unwrap()
    );
    assert_eq!(item_links(section), ["40"]);
    assert!(section.contains("&lt;b&gt;open one"));

    // Any filter replaces the default; filters combine with AND.
    for (query, expected) in [
        ("?project=10&overdue=on", vec!["40"]),
        ("?project=10", vec!["40", "41"]),
        ("?list=21", vec!["41"]),
        ("?repo=arc", vec!["40"]),
        ("?agent=none&due_before=2999-01-02", vec!["40", "41"]),
        ("?assignee=bo", vec!["41"]),
        ("?preset=mine&project=11", vec![]),
    ] {
        let (status, html) = get!(app, loopback(), &format!("/tasks/basecamp/open{query}"));
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(item_links(&html), expected, "{query}");
        assert!(!html.contains("tm-open-truncated"), "{query}");
    }
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/open?project=10&list=21");
    assert_eq!(item_links(&html), ["41"]);
    assert_eq!(
        last_call(&fixture, "open-walk"),
        [
            "open-walk",
            "--provider",
            "basecamp",
            "--project",
            "10",
            "--list",
            "21",
            "--limit",
            "1000",
            "--json"
        ]
    );
    // A walk cut short says so.
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/open?list=22");
    assert_eq!(item_links(&html), ["42"]);
    assert!(html.contains("tm-open-truncated"));
    assert_eq!(
        get!(app, loopback(), "/tasks/basecamp/open?preset=nosuch").0,
        StatusCode::NOT_FOUND
    );

    // Clearing the default shows everything again; deleting removes the preset.
    let (status, _, _) = submit!("/tasks/basecamp/open/default/clear", &[]);
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/open");
    assert_eq!(item_links(&html), ["40", "41", "42"]);
    assert!(html.contains("action=\"/tasks/basecamp/open/presets/mine/delete\""));
    let (status, _, _) = submit!("/tasks/basecamp/open/presets/mine/delete", &[]);
    assert_eq!(status, StatusCode::SEE_OTHER);
    let state = fixture.state().await;
    assert!(state.store().presets().unwrap().is_empty());
    let (status, _, _) = submit!("/tasks/basecamp/open/presets/mine/delete", &[]);
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn agents_start_stop_and_finish_through_the_runtime() {
    let fixture = Fixture::new(true);
    // The directory the agent works in: a clone on a feature branch with a local origin.
    sh(&fixture.directory, "git init -q --bare -b main origin.git");
    sh(
        &fixture.directory,
        "git init -q -b main app && cd app && git config user.name t && git config user.email t@example.com \
         && echo hi > README && git add README && git commit -qm init \
         && git remote add origin ../origin.git && git push -q -u origin main \
         && git remote set-head origin main && git switch -q -c feature \
         && git remote set-url origin https://github.com/example/arc.git \
         && git remote set-url --push origin \"$PWD/../origin.git\"",
    );
    let app = service!(fixture);
    let (cookie, token) = signed_in!(app);
    let dir = fixture.directory.join("app").display().to_string();
    let start = |item: &str| {
        let form = vec![
            ("csrf_token", token.clone()),
            ("repo", "arc".to_owned()),
            ("dir", dir.clone()),
            ("agent", "claude".to_owned()),
        ];
        (format!("/tasks/basecamp/items/{item}/agent"), form)
    };
    macro_rules! send_form {
        ($uri:expr, $form:expr) => {{
            let form: Vec<(&str, &str)> = $form.iter().map(|(k, v)| (*k, v.as_str())).collect();
            post!(app, loopback(), &$uri, Some(cookie.clone()), Some(form))
        }};
    }

    let (uri, form) = start("30");
    let (status, location, body) = send_form!(uri, form);
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(location, "/tasks/basecamp/items/30");
    let state = fixture.state().await;
    let agent = state.store().live_agent_for_item("30").unwrap().unwrap();
    assert!(valid_code(&agent.code));
    assert_eq!(
        (agent.repo.as_str(), agent.worktree.as_str()),
        ("arc", dir.as_str())
    );
    assert_eq!(agent.branch.as_deref(), Some("feature"));
    assert!(!agent.created_worktree);
    let args = std::fs::read_to_string(fixture.directory.join("claude.args")).unwrap();
    assert!(args.contains("--permission-mode\nplan\n"), "{args}");
    assert!(
        !args.contains("hello") && !args.contains("alert"),
        "item text stays out of argv"
    );

    // The item page's panel: code, copy, the attach command and the actions,
    // with no item text anywhere in it.
    let (_, html) = get!(app, loopback(), "/tasks/basecamp/items/30");
    let start_at = html.find("data-tm-agent-panel").unwrap();
    let panel = &html[start_at..start_at + html[start_at..].find("</section>").unwrap()];
    let code = agent.code.as_str();
    assert!(
        panel.contains(&format!("data-tm-copy=\"{code}\"")),
        "{panel}"
    );
    assert!(panel.contains(&format!("harness tasks attach {code}")));
    for action in ["stop", "done"] {
        assert!(
            panel.contains(&format!("action=\"/tasks/agents/{code}/{action}\"")),
            "{action}"
        );
    }
    assert!(
        !panel.contains("/handback"),
        "only an attached agent is handed back"
    );
    for text in ["alert", "script", "Ana", "Proj", "List A"] {
        assert!(!panel.contains(text), "{text} in {panel}");
    }
    assert!(!html.contains("action=\"/tasks/basecamp/items/30/agent\""));
    let (status, html) = get!(app, loopback(), "/tasks/agents");
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!("data-tm-copy=\"harness tasks attach {code}\"")));
    assert!(
        html.contains("href=\"&#x2F;tasks&#x2F;basecamp&#x2F;items&#x2F;30\""),
        "{html}"
    );

    // A second agent on the item is refused; hand back needs an attached one.
    let (uri, form) = start("30");
    let (status, _, body) = send_form!(uri, form);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("live agent"), "{body}");
    let back = [("csrf_token", token.clone()), ("back", "item".to_owned())];
    let (status, _, _) = send_form!(format!("/tasks/agents/{code}/handback"), back);
    assert_eq!(status, StatusCode::CONFLICT);

    // Done after the headless turn: one draft pull request.
    for _ in 0..200 {
        if !process::alive(agent.pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let (status, location, body) = send_form!(format!("/tasks/agents/{code}/done"), back);
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(location, "/tasks/basecamp/items/30");
    let gh = std::fs::read_to_string(fixture.directory.join("gh.args")).unwrap();
    assert_eq!(
        gh.lines()
            .filter(|line| line.starts_with("pr create"))
            .count(),
        1
    );
    assert!(
        gh.contains("pr create --draft --repo example/arc --head feature"),
        "{gh}"
    );
    state.refresh().unwrap();
    let finished = state.store().agent_by_code(code).unwrap().unwrap();
    assert_eq!(finished.state, AgentState::Done);
    assert_eq!(
        finished.pr_url.as_deref(),
        Some("https://github.com/example/arc/pull/9")
    );

    // Stop: another agent, stopped from the agents page.
    let (uri, form) = start("31");
    let (status, location, body) = send_form!(uri, form);
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(location, "/tasks/basecamp/items/31");
    state.refresh().unwrap();
    let second = state.store().live_agent_for_item("31").unwrap().unwrap();
    let plain_form = [("csrf_token", token.clone())];
    let (status, location, _) =
        send_form!(format!("/tasks/agents/{}/stop", second.code), plain_form);
    assert_eq!(
        (status, location.as_str()),
        (StatusCode::SEE_OTHER, "/tasks/agents")
    );
    state.refresh().unwrap();
    assert_eq!(
        state
            .store()
            .agent_by_code(&second.code)
            .unwrap()
            .unwrap()
            .state,
        AgentState::Stopped
    );
    // An agent that is no longer live is not found.
    let (status, _, _) = send_form!(format!("/tasks/agents/{}/stop", second.code), plain_form);
    assert_eq!(status, StatusCode::NOT_FOUND);
}
