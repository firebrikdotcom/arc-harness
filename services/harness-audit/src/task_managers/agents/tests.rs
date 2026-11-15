//! Offline end-to-end: fake claude, codex, gh and task-manager CLI, a temp git
//! repository with a local bare remote, and a real event database.

use super::*;
use crate::domain::task_agent::events::AgentState;
use diesel::connection::SimpleConnection;
use diesel::{Connection, SqliteConnection};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

struct Fixture {
    root: PathBuf,
    url: String,
    rt: Runtime,
}

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

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("tm-agents-{}", uuid::Uuid::new_v4()));
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let r = root.display().to_string();
    sh(&root, "git init -q --bare -b main origin.git");
    sh(
        &root,
        "git init -q -b main app && cd app && git config user.name t && git config user.email t@example.com \
         && echo hi > README && git add README && git commit -qm init \
         && git remote add origin ../origin.git && git push -q -u origin main \
         && git remote set-head origin main",
    );
    // origin reads as a GitHub repository; pushes still land in the local bare one.
    sh(
        &root,
        &format!(
            "cd app && git remote set-url origin https://github.com/o/r.git \
             && git remote set-url --push origin {r}/origin.git"
        ),
    );
    // A supplied directory: a linked worktree of the repository on a feature branch.
    sh(&root, "git -C app worktree add -q -b feature ../app2");
    script(
        &bin.join("claude"),
        &format!(
            "for a; do printf '%s\\n' \"$a\"; done >> {r}/claude.args\necho --- >> {r}/claude.args\nsleep 0.3\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"plan\"}}'\n"
        ),
    );
    script(
        &bin.join("codex"),
        &format!(
            "for a; do printf '%s\\n' \"$a\"; done >> {r}/codex.args\necho '{{\"type\":\"thread.started\",\"thread_id\":\"codex-thread-1\"}}'\nsleep 0.3\necho '{{\"type\":\"turn.completed\"}}'\n"
        ),
    );
    script(
        &bin.join("gh"),
        &format!(
            r#"echo "$*" >> {r}/gh.args
case "$*" in
  "pr list"*) if [ -f {r}/pr.exists ]; then echo '[{{"url":"https://github.com/o/r/pull/7"}}]'; else echo '[]'; fi ;;
  *) [ -f {r}/gh.fail ] && {{ echo boom >&2; exit 1; }}
     # Created, then the reply is lost.
     [ -f {r}/gh.flaky ] && {{ touch {r}/pr.exists; echo lost >&2; exit 1; }}
     echo https://github.com/o/r/pull/1 ;;
esac
"#
        ),
    );
    // The task-manager CLI: check, get, seed.
    script(
        &bin.join("tasks"),
        &format!(
            r#"cmd=; item=
prev=
for a; do
  case $a in check|get|seed) cmd=$a ;; esac
  [ "$prev" = --item ] && item=$a
  prev=$a
done
case $cmd in
  check) echo '{{"enabled":true,"repos":["app"],"repo_paths":{{"app":"{r}/app"}},"mappings":{{"basecamp:100":"app"}},"ssh":"cfg@host"}}' ;;
  get) [ "$item" = 404 ] && {{ echo '{{"error":{{"code":"not_found","message":"no item"}}}}'; exit 4; }}
       echo '{{"item":{{"id":"'$item'","list":{{"id":"100"}}}}}}' ;;
  seed) [ "$item" = 500 ] && {{ echo '{{"error":{{"code":"upstream","message":"no seed"}}}}'; exit 5; }}
        echo '{{"packet":"<<<TASK-MANAGER-UNTRUSTED sha256=x bytes=5>>>\nhello\n<<<END-TASK-MANAGER-UNTRUSTED>>>"}}' ;;
  *) exit 2 ;;
esac
"#
        ),
    );
    let db = root.join("events.sqlite");
    let url = db.to_str().unwrap().to_string();
    SqliteConnection::establish(&url)
        .unwrap()
        .batch_execute(include_str!(
            "../../../migrations/00000000000000_arc_base/up.sql"
        ))
        .unwrap();
    let rt = Runtime {
        tasks_cli: vec![bin.join("tasks").into()],
        state_dir: root.join("state"),
        claude: bin.join("claude").into(),
        codex: bin.join("codex").into(),
        gh: bin.join("gh").into(),
        git: "git".into(),
        ssh: Some("me@box".into()),
        takeover_wait: Duration::from_secs(5),
        session_wait: Duration::from_secs(5),
    };
    Fixture { root, url, rt }
}

fn wait_exit(pid: u32) {
    for _ in 0..100 {
        if !process::alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("fake agent {pid} did not exit");
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// How many `gh pr create` calls the fake gh saw.
fn creates(f: &Fixture) -> usize {
    read(f.root.join("gh.args"))
        .lines()
        .filter(|line| line.starts_with("pr create"))
        .count()
}

/// Whether the bare origin has `branch`.
fn pushed(f: &Fixture, branch: &str) -> bool {
    Command::new("git")
        .args(["--git-dir=origin.git", "rev-parse", "--verify", "-q"])
        .arg(format!("refs/heads/{branch}"))
        .current_dir(&f.root)
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// A claude agent on `item` in the supplied directory `dir`, its turn ended.
async fn started_in(f: &Fixture, tm: &TaskManagers, item: &str, dir: &Path) -> AgentRecord {
    let agent = start(
        tm,
        &f.rt,
        request(
            item,
            Some("app"),
            Some(dir.to_path_buf()),
            AgentKind::Claude,
        ),
    )
    .await
    .unwrap()
    .record;
    wait_exit(agent.pid);
    agent
}

fn request(item: &str, repo: Option<&str>, dir: Option<PathBuf>, kind: AgentKind) -> StartRequest {
    StartRequest {
        item_id: item.into(),
        repo: repo.map(str::to_string),
        dir,
        agent_kind: kind,
    }
}

#[actix_web::test]
async fn agent_lifecycle_end_to_end() {
    let f = fixture();
    let tm = TaskManagers::open(&f.url).await.unwrap();
    let rt = &f.rt;

    // Start on the list's configured default repo: a fresh worktree, read-only turn.
    let started = start(&tm, rt, request("7", None, None, AgentKind::Claude))
        .await
        .unwrap();
    let agent = started.record.clone();
    assert_eq!(started.agent_id, "basecamp:7");
    assert!(crate::task_managers::valid_code(&agent.code));
    assert_eq!(agent.state, AgentState::Running);
    assert!(agent.created_worktree);
    assert!(!agent.session_id.is_empty());
    let tree = PathBuf::from(&agent.worktree);
    assert!(tree.is_dir());
    assert!(tree
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("app-tma-"));
    assert_eq!(tree.parent(), f.root.join("app").parent());
    assert!(agent
        .branch
        .as_deref()
        .unwrap()
        .starts_with("tm/basecamp-7-"));
    let args = read(f.root.join("claude.args"));
    assert!(args.contains("--permission-mode\nplan\n"), "{args}");
    // No MCP servers, and only read-only local tools.
    assert!(
        args.contains("--strict-mcp-config\n--tools\nRead,Grep,Glob\n--permission-mode\n"),
        "{args}"
    );
    assert!(!args.contains("--mcp-config"), "{args}");
    assert!(args.contains(&format!("--session-id\n{}\n", agent.session_id)));
    assert!(!args.contains("hello"), "item text must not reach argv");
    let packet = agent_dir(rt, "7", &agent.code).join("packet.txt");
    assert!(read(packet.clone()).contains("hello"));
    assert_eq!(
        std::fs::metadata(&packet).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // A second live agent on the same item is refused.
    let error = start(&tm, rt, request("7", None, None, AgentKind::Claude))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "conflict");
    // A repository outside repos is refused before anything is created.
    let error = start(
        &tm,
        rt,
        request("8", Some("other"), None, AgentKind::Claude),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "usage");
    // An unknown item is not found.
    let error = start(&tm, rt, request("404", None, None, AgentKind::Claude))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "not_found");

    // The supervisor records the end of the headless turn.
    wait_exit(agent.pid);
    assert_eq!(supervise_once(&tm, rt).await.unwrap(), 1);
    assert_eq!(
        status(&tm, rt, &agent.code).unwrap().record.state,
        AgentState::Waiting
    );

    // attach --print: the exact resume command, and its SSH variant.
    let plan = attach(&tm, rt, &agent.code, false).await.unwrap();
    let local = format!(
        "cd '{}' && claude --resume '{}'",
        agent.worktree, agent.session_id
    );
    assert_eq!(plan.commands.local, local);
    assert_eq!(
        plan.commands.ssh.as_deref(),
        Some(format!("ssh -t 'me@box' {}", quote(&local)).as_str())
    );
    assert_eq!(plan.record.state, AgentState::Attached);
    assert_eq!(plan.args, ["--resume", agent.session_id.as_str()]);

    // The supervisor leaves an agent you hold alone.
    assert_eq!(supervise_once(&tm, rt).await.unwrap(), 0);
    assert_eq!(
        status(&tm, rt, &agent.code).unwrap().record.state,
        AgentState::Attached
    );

    // done: one commit, one push, exactly one draft PR, the worktree removed.
    std::fs::write(tree.join("change.txt"), "work").unwrap();
    let finished = done(&tm, rt, &agent.code).await.unwrap();
    assert_eq!(finished.pr_url, "https://github.com/o/r/pull/1");
    assert!(finished.worktree_removed);
    assert!(!tree.exists());
    assert_eq!(finished.agent.record.state, AgentState::Done);
    let gh = read(f.root.join("gh.args"));
    assert_eq!(creates(&f), 1);
    assert!(gh.contains("pr create --draft --repo o/r --head tm/basecamp-7-"));
    assert!(pushed(&f, agent.branch.as_deref().unwrap()));

    // A codex agent in a supplied directory: that directory is left in place.
    let dir = f.root.join("app2");
    let started = start(
        &tm,
        rt,
        request("9", Some("app"), Some(dir.clone()), AgentKind::Codex),
    )
    .await
    .unwrap();
    let agent = started.record;
    assert_eq!(agent.session_id, "codex-thread-1");
    assert!(!agent.created_worktree);
    assert_eq!(agent.branch.as_deref(), Some("feature"));
    let args = read(f.root.join("codex.args"));
    assert!(args.contains("--sandbox\nread-only\n"), "{args}");
    wait_exit(agent.pid);
    std::fs::write(dir.join("more.txt"), "work").unwrap();
    let finished = done(&tm, rt, &agent.code).await.unwrap();
    assert!(!finished.worktree_removed);
    assert!(dir.is_dir());
    assert_eq!(creates(&f), 2);

    // Stop never signals an attached agent; handback needs an attached one.
    let started = start(&tm, rt, request("10", None, None, AgentKind::Claude))
        .await
        .unwrap();
    let code = started.record.code.clone();
    assert_eq!(
        handback(&tm, rt, &code).await.unwrap_err().kind(),
        "conflict"
    );
    let stopped = stop(&tm, rt, &code).await.unwrap();
    assert_eq!(stopped.record.state, AgentState::Stopped);
    assert!(!process::alive(started.record.pid));
    assert_eq!(list(&tm, rt).unwrap().len(), 3);
}

/// The entries of `dir` whose names start with `prefix`.
fn names(dir: &Path, prefix: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rows| {
            rows.filter_map(|row| row.ok()?.file_name().into_string().ok())
                .filter(|name| name.starts_with(prefix))
                .collect()
        })
        .unwrap_or_default()
}

#[actix_web::test]
async fn failed_start_removes_its_directory_and_worktree() {
    let f = fixture();
    let tm = TaskManagers::open(&f.url).await.unwrap();
    let branches = |f: &Fixture| {
        let out = Command::new("git")
            .args(["-C", "app", "branch", "--list", "tm/*"])
            .current_dir(&f.root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // The seed fails after the directory and the worktree exist.
    let error = start(&tm, &f.rt, request("500", None, None, AgentKind::Claude))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "internal");
    assert!(!f.rt.state_dir.join("500").exists());

    // The launch fails after the packet and the log were written; another
    // agent's files under the same item are left alone.
    let other = f.rt.state_dir.join("11").join("other");
    std::fs::create_dir_all(&other).unwrap();
    let mut rt = f.rt.clone();
    rt.claude = f.root.join("bin/absent").into();
    let error = start(&tm, &rt, request("11", None, None, AgentKind::Claude))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "usage");
    assert_eq!(names(&f.rt.state_dir.join("11"), ""), ["other"]);

    // Neither left a worktree, a branch or a record behind.
    assert_eq!(names(&f.root, "app-tma-"), Vec::<String>::new());
    assert_eq!(branches(&f), "");
    assert!(list(&tm, &f.rt).unwrap().is_empty());
}

#[actix_web::test]
async fn ssh_destination_from_check_and_the_environment() {
    let f = fixture();
    // HARNESS_TASKS_SSH (the runtime's own value) wins over task-managers.json.
    assert_eq!(f.rt.config().unwrap().ssh.as_deref(), Some("me@box"));
    let mut rt = f.rt.clone();
    rt.ssh = None;
    assert_eq!(rt.config().unwrap().ssh.as_deref(), Some("cfg@host"));
    // Resolved once, every view (list, status, stop, handback, done) agrees with start.
    rt.resolve_ssh();
    assert_eq!(rt.ssh.as_deref(), Some("cfg@host"));
    let tm = TaskManagers::open(&f.url).await.unwrap();
    let agent = started_in(&f, &tm, "50", &f.root.join("app2")).await;
    let shown = status(&tm, &rt, &agent.code).unwrap().resume.ssh.unwrap();
    assert!(shown.starts_with("ssh -t 'cfg@host' "), "{shown}");
    f.rt.clone().resolve_ssh();
    assert_eq!(f.rt.ssh.as_deref(), Some("me@box"));
    assert!(valid_ssh("host-1.example") && !valid_ssh("-oProxyCommand=x") && !valid_ssh("a b"));
}

#[test]
fn turn_output_classification() {
    assert!(supervise::turn_failure(r#"{"type":"result","is_error":true}"#).is_some());
    assert!(supervise::turn_failure(r#"{"type":"turn.failed"}"#).is_some());
    assert!(supervise::turn_failure("{\"type\":\"result\",\"is_error\":false}\nnoise").is_none());
    assert_eq!(
        launch::codex_thread("x\n{\"type\":\"thread.started\",\"thread_id\":\"abc-1\"}\n")
            .as_deref(),
        Some("abc-1")
    );
}

#[actix_web::test]
async fn done_never_pushes_without_a_known_default_branch() {
    let f = fixture();
    let tm = TaskManagers::open(&f.url).await.unwrap();
    let dir = f.root.join("app2");
    let agent = started_in(&f, &tm, "20", &dir).await;
    std::fs::write(dir.join("work.txt"), "work").unwrap();
    let untouched = |f: &Fixture| {
        assert!(!pushed(f, "feature") && read(f.root.join("gh.args")).is_empty());
        let out = Command::new("git")
            .args(["-C", "app2", "status", "--porcelain"])
            .current_dir(&f.root)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("work.txt"));
    };

    // origin/HEAD unset: the guard fails closed and names the fix.
    sh(&f.root, "git -C app remote set-head origin -d");
    let error = done(&tm, &f.rt, &agent.code).await.unwrap_err();
    assert_eq!(error.kind(), "conflict");
    assert!(error.to_string().contains("git remote set-head origin -a"));
    untouched(&f);

    // Known again, but origin is not a GitHub repository: still nothing pushed.
    sh(
        &f.root,
        "cd app && git remote set-head origin main && git remote set-url origin ../origin.git",
    );
    let error = done(&tm, &f.rt, &agent.code).await.unwrap_err();
    assert_eq!(error.kind(), "conflict");
    assert!(error.to_string().contains("github.com"));
    untouched(&f);
    stop(&tm, &f.rt, &agent.code).await.unwrap();

    // A directory on the default branch itself is refused.
    sh(
        &f.root,
        "git -C app remote set-url origin https://github.com/o/r.git",
    );
    let main = f.root.join("app");
    let agent = started_in(&f, &tm, "21", &main).await;
    assert_eq!(agent.branch.as_deref(), Some("main"));
    std::fs::write(main.join("work.txt"), "work").unwrap();
    let error = done(&tm, &f.rt, &agent.code).await.unwrap_err();
    assert_eq!(error.kind(), "conflict");
    assert!(read(f.root.join("gh.args")).is_empty());
    sh(&f.root, "test -z \"$(git -C app log origin/main..main)\"");
}

#[actix_web::test]
async fn start_refuses_a_directory_outside_the_repository() {
    let f = fixture();
    let tm = TaskManagers::open(&f.url).await.unwrap();
    // Another clone of the same origin, a repository of its own, and a plain directory.
    sh(
        &f.root,
        "git clone -q origin.git clone && git init -q other && mkdir plain \
         && git clone -q origin.git twin && git -C twin remote set-url origin git@github.com:o/r.git",
    );
    let checkout = f.root.join("app").display().to_string();
    for name in ["clone", "other", "plain", "twin"] {
        let error = start(
            &tm,
            &f.rt,
            request(
                "30",
                Some("app"),
                Some(f.root.join(name)),
                AgentKind::Claude,
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), "usage", "{name}");
        // The refusal names the repository and where its checkout is; a clone
        // of the same origin is told apart from an unrelated directory.
        let said = if name == "twin" {
            format!("dir is a separate clone of app; use the checkout at {checkout} or a linked worktree of it")
        } else {
            format!("dir must be the checkout of app at {checkout} or a linked worktree of it")
        };
        assert_eq!(error.to_string(), said, "{name}");
    }
    // A plain directory inside some other checkout does not stand for it.
    sh(&f.root, "mkdir other/inside");
    let error = worktree::require_in_repo(
        &f.rt,
        &f.root.join("other"),
        &f.root.join("other/inside"),
        "inside",
    )
    .unwrap_err()
    .to_string();
    assert!(error.ends_with("is not a git repository"), "{error}");
    let error = worktree::create(&f.rt, &f.root.join("other/inside"), "30")
        .unwrap_err()
        .to_string();
    assert!(error.ends_with("is not a git repository"), "{error}");
    // The label is what the caller passes; no origin URL or credential is echoed.
    sh(
        &f.root,
        "git -C twin remote set-url origin https://user:hunter2@github.com/o/r.git",
    );
    let error = worktree::require_in_repo(&f.rt, &f.root.join("twin"), &f.root.join("app"), "o/r")
        .unwrap_err()
        .to_string();
    assert!(error.contains("separate clone of o/r;"), "{error}");
    assert!(
        !error.contains("hunter2") && !error.contains("github.com"),
        "{error}"
    );
    let plain = f.root.join("plain");
    let error = worktree::require_in_repo(&f.rt, &f.root.join("app"), &plain, "o/r").unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "the repository path of o/r ({}) is not a git repository",
            plain.display()
        )
    );
    worktree::require_in_repo(&f.rt, &f.root.join("app2"), &f.root.join("app"), "o/r").unwrap();
    // A stored association's directory is checked the same way.
    tm.associate(
        "100",
        "app",
        Some(&f.root.join("clone")),
        &["app".to_string()],
    )
    .await
    .unwrap();
    let error = start(&tm, &f.rt, request("30", None, None, AgentKind::Claude))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "usage");
    // Nothing ran and nothing was left behind.
    assert!(read(f.root.join("claude.args")).is_empty());
    assert!(!f.rt.state_dir.join("30").exists());
    assert!(list(&tm, &f.rt).unwrap().is_empty());

    // The main checkout and a linked worktree, also through a symlink.
    std::os::unix::fs::symlink(f.root.join("app2"), f.root.join("link")).unwrap();
    let agent = started_in(&f, &tm, "31", &f.root.join("link")).await;
    assert_eq!(agent.branch.as_deref(), Some("feature"));
}

#[actix_web::test]
async fn done_can_be_retried() {
    let f = fixture();
    let tm = TaskManagers::open(&f.url).await.unwrap();
    let rt = &f.rt;
    let dir = f.root.join("app2");

    // gh fails with no pull request: the agent stays live, the work is pushed.
    let agent = started_in(&f, &tm, "40", &dir).await;
    std::fs::write(dir.join("work.txt"), "work").unwrap();
    std::fs::write(f.root.join("gh.fail"), "").unwrap();
    let error = done(&tm, rt, &agent.code).await.unwrap_err();
    assert_eq!(error.kind(), "internal");
    assert!(pushed(&f, "feature"));
    assert!(status(&tm, rt, &agent.code).unwrap().record.state.is_live());
    // The retry has nothing to commit and nothing new to push.
    std::fs::remove_file(f.root.join("gh.fail")).unwrap();
    let finished = done(&tm, rt, &agent.code).await.unwrap();
    assert_eq!(finished.pr_url, "https://github.com/o/r/pull/1");
    assert_eq!(creates(&f), 2);
    sh(
        &f.root,
        "test \"$(git -C app2 rev-list --count main..feature)\" = 1",
    );

    // gh creates the pull request and then fails: the existing one is recorded.
    let agent = started_in(&f, &tm, "41", &dir).await;
    std::fs::write(f.root.join("gh.flaky"), "").unwrap();
    let finished = done(&tm, rt, &agent.code).await.unwrap();
    assert_eq!(finished.pr_url, "https://github.com/o/r/pull/7");
    assert_eq!(finished.agent.record.state, AgentState::Done);
    assert_eq!(creates(&f), 3);

    // A pull request that exists already is recorded without a second create.
    let agent = started_in(&f, &tm, "42", &dir).await;
    let finished = done(&tm, rt, &agent.code).await.unwrap();
    assert_eq!(finished.pr_url, "https://github.com/o/r/pull/7");
    assert_eq!(creates(&f), 3);
    let gh = read(f.root.join("gh.args"));
    assert!(gh.contains("pr list --head feature --repo o/r --state open --json url --limit 1"));
}

#[test]
fn github_repo_from_origin_urls() {
    for (url, repo) in [
        ("https://github.com/o/r.git", Some("o/r")),
        (
            "https://user:secret@GitHub.com/Own-1/my.repo_x/",
            Some("Own-1/my.repo_x"),
        ),
        ("ssh://git@github.com:22/o/r.git", Some("o/r")),
        ("git@github.com:o/r.git\n", Some("o/r")),
        ("github.com:o/r", Some("o/r")),
        ("https://github.com/o/.git", Some("o/.git")),
        ("../origin.git", None),
        ("/srv/git/o/r.git", None),
        ("https://gitlab.com/o/r.git", None),
        ("https://github.com.evil.example/o/r", None),
        ("https://evil.example/github.com/o/r", None),
        ("https://github.com/o/r/extra", None),
        ("https://github.com/-o/r", None),
        ("https://github.com/o/..", None),
        ("https://github.com/o/r;x", None),
        ("git@github.com:o/r --flag", None),
        ("https://github.com:port/o/r", None),
    ] {
        assert_eq!(done::github_repo(url).as_deref(), repo, "{url}");
    }
}
