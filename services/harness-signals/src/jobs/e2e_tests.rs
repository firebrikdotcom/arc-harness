//! Jobs end to end against fake agents, Herdr and difit: shell scripts in a
//! temporary directory that record their argv and working directory. No real
//! claude, codex, herdr or difit is ever started: the settings name only fakes.

use super::actions;
use super::runner::{run_once, supervise, Context};
use super::settings::Settings;
use super::*;
use crate::config::Config;
use serde_json::{json, Value};
use std::process::Command;

const GOOD: &str = r#"{"likely_cause":"the plan is null for archived plans","code_involved":"src/app/Billing.php:3","reproduce":"renew an archived plan","suggested_fix":"load archived plans too","blast_radius":"renewals only","confidence":"medium"}"#;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/bash\n{body}")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn issue(key: &str, line: u64) -> Value {
    json!({"key": key, "source": "test", "id": key, "title": format!("Boom {key} for ops@example.com"),
           "level": "error", "status": "open", "count": 3, "users": 1, "first_seen": null, "last_seen": null,
           "url": "https://errors.example/issues/1?token=secret",
           "frames": [{"path": "/var/www/vendor/lib/Thing.php", "line": 9, "in_app": false},
                      {"path": "/var/www/html/src/app/Billing.php", "line": line, "in_app": true}]})
}

/// A git project registered as a harness target, with two issues on card `app`.
fn add_target(base: &Path, db: &Path, id: &str) -> PathBuf {
    let project = base.join(format!("project-{id}"));
    std::fs::create_dir_all(project.join("src/app")).unwrap();
    std::fs::write(
        project.join("src/app/Billing.php"),
        "<?php\n\nfunction renew($plan) {\n    return $plan->price;\n}\n",
    )
    .unwrap();
    std::fs::write(project.join("README.md"), "demo\n").unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "init"]);
    let dir = db.join("targets").join(id);
    std::fs::create_dir_all(dir.join("board")).unwrap();
    std::fs::create_dir_all(dir.join("signals")).unwrap();
    std::fs::write(
        dir.join("target.state"),
        format!("TARGET_ID={id}\nTARGET_ROOT={}\n", project.display()),
    )
    .unwrap();
    std::fs::write(
        dir.join("board/board.json"),
        json!({"default_branch": "main", "nodes": [{"id": "app", "owns": ["src/app/"]}, {"id": "models", "owns": ["src/models/"]}]}).to_string(),
    )
    .unwrap();
    let issues = json!({"test#1": issue("test#1", 3), "test#2": issue("test#2", 4), "test#3": {"key": "test#3", "source": "test", "id": "3",
        "title": "No frames", "level": "error", "status": "open", "count": 1, "users": 0, "first_seen": null, "last_seen": null, "url": null}});
    std::fs::write(dir.join("signals/issues.json"), issues.to_string()).unwrap();
    project
}

struct Fixture {
    dir: tempfile::TempDir,
    db: PathBuf,
    config: Config,
    settings: Settings,
}

impl Fixture {
    fn new(targets: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let db = base.join("db");
        let fakes = base.join("fakes");
        let rec = base.join("calls");
        let home = base.join("home");
        for d in [&fakes, &rec, &home] {
            std::fs::create_dir_all(d).unwrap();
        }
        let projects: Vec<Value> = targets
            .iter()
            .map(|id| {
                add_target(&base, &db, id);
                json!({"name": id, "target": id, "path_map": [{"from": "/var/www/html/", "to": ""}],
                       "providers": [{"type": "command", "argv": ["true"]}]})
            })
            .collect();
        let config: Config = serde_json::from_value(json!({"projects": projects})).unwrap();
        let (rec_s, base_s, home_s) = (rec.display(), base.display(), home.display());
        let record = format!("printf '%s\\0' \"$(pwd -P)\" \"$@\" > \"{rec_s}/$(basename \"$0\")-$$\"\nmode=$(cat \"{base_s}/mode\" 2>/dev/null)\n");
        script(
            &fakes.join("claude"),
            &format!(
                "{record}case \"$mode\" in\n  sleep) sleep 30 ;;\n  fail) echo boom >&2; exit 3 ;;\n  *) cat \"{base_s}/claude-reply\" ;;\nesac\n"
            ),
        );
        script(
            &fakes.join("codex"),
            &format!(
                "{record}echo '{{\"type\":\"thread.started\",\"thread_id\":\"019a0000-fake-thread\"}}'\ncase \"$mode\" in\n  sleep) sleep 30 ;;\n  fail) exit 3 ;;\n  *) cat \"{base_s}/codex-reply\" ;;\nesac\n"
            ),
        );
        script(
            &fakes.join("herdr"),
            &format!(
                r#"printf '%s\0' "$@" > "{rec_s}/herdr-$1-$2-$$"
state="{base_s}/herdr-state"; mkdir -p "$state"
# Replies copy the shapes of the real herdr CLI: {{"id":"cli:<group>:<verb>","result":{{...,"type":...}}}},
# and failures print {{"error":{{"code","message"}},"id"}} to stderr and exit 1.
fail() {{ printf '{{"error":{{"code":"%s","message":"%s"}},"id":"cli:%s"}}\n' "$1" "$2" "$3" >&2; exit 1; }}
case "$1 $2" in
  "tab create")
    while [ $# -gt 0 ]; do case "$1" in --cwd) printf '%s' "$2" > "$state/cwd" ;; --label) printf '%s' "$2" > "$state/label" ;; esac; shift; done
    l=$(cat "$state/label"); rm -f "$state/closed"
    if [ -e "$state/roottab" ]; then
      echo '{{"id":"cli:tab:create","result":{{"root_pane":{{"pane_id":"w1:p7","tab_id":"w1:t7","workspace_id":"w1"}},"type":"tab_created"}}}}'
    else
      echo "{{\"id\":\"cli:tab:create\",\"result\":{{\"root_pane\":{{\"pane_id\":\"w1:p7\",\"tab_id\":\"w1:t7\",\"workspace_id\":\"w1\"}},\"tab\":{{\"agent_status\":\"unknown\",\"focused\":false,\"label\":\"$l\",\"number\":7,\"pane_count\":1,\"tab_id\":\"w1:t7\",\"workspace_id\":\"w1\"}},\"type\":\"tab_created\"}}}}"
    fi ;;
  "agent start")
    [ -e "$state/startfail" ] && fail agent_start_failed "the agent did not start" agent:start
    while [ $# -gt 0 ]; do [ "$1" = --session-id ] && printf '%s' "$2" > "$state/session"; shift; done
    echo '{{"id":"cli:agent:start","result":{{"type":"agent_started"}}}}' ;;
  "agent prompt")
    if [ -e "$state/hold" ]; then
      n=$(( $(cat "$state/prompts" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$state/prompts"
      echo working > "$state/status"
      while [ ! -e "$state/release-$n" ] && [ -d "$state" ]; do sleep 0.05; done
      exit 1
    fi
    if [ -e "$state/busy" ]; then echo working > "$state/status"; sleep 30; exit 1; fi
    s=$(cat "$state/session"); b=$(sed 's/[^A-Za-z0-9]/-/g' "$state/cwd")
    mkdir -p "{home_s}/.claude/projects/$b"
    cat "{base_s}/claude-transcript" > "{home_s}/.claude/projects/$b/$s.jsonl"
    echo '{{"id":"cli:agent:prompt","result":{{"type":"agent_prompted"}}}}' ;;
  "agent get")
    [ -e "$state/closed" ] && fail agent_not_found "agent target $3 not found" agent:get
    s=$(cat "$state/session" 2>/dev/null); st=$(cat "$state/status" 2>/dev/null || echo done)
    c=$(cat "$state/cwd" 2>/dev/null)
    session=""; [ -n "$s" ] && session="\"agent_session\":{{\"agent\":\"claude\",\"kind\":\"id\",\"source\":\"herdr:claude\",\"value\":\"$s\"}},"
    echo "{{\"id\":\"cli:agent:get\",\"result\":{{\"agent\":{{\"agent\":\"claude\",$session\"agent_status\":\"$st\",\"completion_seq\":3,\"cwd\":\"$c\",\"focused\":false,\"foreground_cwd\":\"$c\",\"interactive_ready\":true,\"name\":\"$3\",\"pane_id\":\"w1:p7\",\"revision\":2,\"state_change_seq\":3,\"tab_id\":\"w1:t7\",\"terminal_id\":\"term_1\",\"terminal_title\":\"claude\",\"terminal_title_stripped\":\"claude\",\"workspace_id\":\"w1\"}},\"type\":\"agent_info\"}}}}" ;;
  "agent send-keys")
    [ -e "$state/stuck" ] || echo idle > "$state/status"
    echo '{{"id":"cli:agent:send_keys","result":{{"type":"keys_sent"}}}}' ;;
  "agent wait")
    [ -e "$state/stuck" ] && fail timeout "timed out waiting for agent" agent:wait
    echo '{{"id":"cli:agent:wait","result":{{"type":"agent_waited"}}}}' ;;
  "tab close")
    [ -e "$state/noclose" ] && fail tab_close_failed "close refused" tab:close
    touch "$state/closed"; echo gone > "$state/status"
    echo '{{"id":"cli:tab:close","result":{{"type":"tab_closed"}}}}' ;;
  "pane process-info")
    [ -e "$state/closed" ] && [ ! -e "$state/ghost" ] && fail pane_not_found "pane not found" pane:process_info
    echo '{{"id":"cli:pane:process_info","result":{{"process_info":{{"foreground_process_group_id":4242,"foreground_processes":[{{"argv":["claude"],"argv0":"claude","cmdline":"claude","cwd":"/tmp","name":"claude","pid":4242}}],"pane_id":"w1:p7","shell_pid":4241}},"type":"pane_process_info"}}}}' ;;
  *) fail unknown_command "unexpected" "$1:$2" ;;
esac
"#
            ),
        );
        script(&fakes.join("fake-difit"), &format!("{record}sleep 30\n"));
        let reply = format!("Done.\n{}\n{GOOD}\n{}", packet::START, packet::END);
        std::fs::write(
            base.join("claude-reply"),
            json!({"type": "result", "subtype": "success", "result": reply}).to_string(),
        )
        .unwrap();
        std::fs::write(
            base.join("codex-reply"),
            json!({"type": "item.completed", "item": {"type": "agent_message", "text": reply}})
                .to_string(),
        )
        .unwrap();
        let transcript = [
            json!({"type": "user", "message": {"role": "user", "content": format!("{} prompt {}", packet::START, packet::END)}}),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": reply}]}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        std::fs::write(base.join("claude-transcript"), transcript).unwrap();

        let mut settings = Settings::for_tests(&home);
        settings.daily_cap = 20;
        settings.claude = Some(vec![fakes.join("claude").display().to_string()]);
        settings.codex = Some(vec![fakes.join("codex").display().to_string()]);
        settings.difit = Some(vec![fakes.join("fake-difit").display().to_string()]);
        Self {
            dir,
            db,
            config,
            settings,
        }
    }

    fn base(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn mode(&self, mode: &str) {
        std::fs::write(self.base().join("mode"), mode).unwrap();
    }

    fn ctx(&self) -> Context<'_> {
        Context {
            db_root: &self.db,
            config: Some(&self.config),
            settings: &self.settings,
        }
    }

    fn target(&self, id: &str) -> Target {
        crate::store::resolve(&self.db, id).unwrap()
    }

    /// One runner scan whose supervisors run on threads of this test process.
    fn run(&self) -> Value {
        let (db, config, settings) = (self.db.clone(), self.config.clone(), self.settings.clone());
        let spawn = move |target: &Target, fp: &str| -> Result<Proc> {
            let (db, config, settings, id, fp) = (
                db.clone(),
                config.clone(),
                settings.clone(),
                target.id.clone(),
                fp.to_owned(),
            );
            std::thread::spawn(move || {
                let ctx = Context {
                    db_root: &db,
                    config: Some(&config),
                    settings: &settings,
                };
                if let Err(error) = supervise(&ctx, &id, &fp) {
                    eprintln!("supervise {fp}: {error:#}");
                }
            });
            Ok(process::capture(std::process::id()))
        };
        run_once(&self.ctx(), &spawn).unwrap()
    }

    fn wait(&self, id: &str, fp: &str, what: impl Fn(&Job) -> bool) -> Job {
        let target = self.target(id);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let job = load(&target, fp).unwrap().unwrap();
            if what(&job) {
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting; job is {:?} {:?} {:?}",
                job.status,
                job.owner,
                job.events.last()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until the supervisor has let go of the job.
    fn settled(&self, id: &str, fp: &str) {
        let target = self.target(id);
        let deadline = Instant::now() + Duration::from_secs(20);
        while read_procs(&target, fp).supervisor.is_some() {
            assert!(Instant::now() < deadline, "the supervisor did not finish");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Every recorded call of one fake: `[cwd, args...]`, oldest first.
    fn calls(&self, prefix: &str) -> Vec<Vec<String>> {
        let mut files: Vec<_> = std::fs::read_dir(self.base().join("calls"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .map(|e| (e.metadata().unwrap().modified().unwrap(), e.path()))
            .collect();
        files.sort();
        files
            .into_iter()
            .map(|(_, path)| {
                let text = std::fs::read_to_string(path).unwrap();
                let text = text.strip_suffix('\0').unwrap_or(&text);
                text.split('\0').map(str::to_owned).collect()
            })
            .collect()
    }
}

fn has(argv: &[String], words: &[&str]) -> bool {
    argv.windows(words.len())
        .any(|w| w.iter().zip(words).all(|(a, b)| a == b))
}

fn tree_hash(dir: &Path) -> String {
    format!(
        "{}|{}",
        git(dir, &["rev-parse", "HEAD^{tree}"]),
        git(dir, &["status", "--porcelain", "--untracked-files=all"])
    )
}

#[test]
fn claude_background_job_diagnoses_read_only_once_per_root_cause() {
    let f = Fixture::new(&["demo-1"]);
    let ctx = f.ctx();
    let target = f.target("demo-1");
    let project_hash = tree_hash(&target.root);

    assert!(
        matches!(
            actions::analyze(&ctx, "demo-1", "test#3", "app", false),
            Err(JobError::Invalid(_))
        ),
        "no root frame"
    );
    assert!(matches!(
        actions::analyze(&ctx, "demo-1", "test#9", "app", false),
        Err(JobError::NotFound(_))
    ));
    assert!(matches!(
        actions::analyze(&ctx, "demo-1", "test#1", "nope", false),
        Err(JobError::NotFound(_))
    ));
    assert!(matches!(
        actions::analyze(&ctx, "../x", "test#1", "app", false),
        Err(JobError::NotFound(_))
    ));
    let (job, created) = actions::analyze(&ctx, "demo-1", "test#1", "app", false).unwrap();
    assert!(created);
    assert_eq!(
        job.fingerprint,
        fingerprint("demo-1", "app", "src/app/Billing.php", 3)
    );
    assert_eq!(
        job.root_frame,
        RootFrame {
            path: "src/app/Billing.php".into(),
            line: 3
        }
    );
    assert_eq!(
        (job.status, job.owner, job.agent, job.launcher),
        (
            Status::Queued,
            Owner::Runner,
            AgentKind::Claude,
            Launcher::Background
        )
    );
    assert_eq!(
        job.worktree.branch,
        format!("nw/{}", &job.fingerprint[..12])
    );
    assert_eq!(job.worktree.base_branch, "main");
    assert!(
        !job.issue_title.contains("ops@example.com"),
        "titles are scrubbed"
    );
    assert_eq!(
        job.issue_url.as_deref(),
        Some("https://errors.example/issues/1")
    );
    let fp = job.fingerprint.clone();
    let (again, created) = actions::analyze(&ctx, "demo-1", "test#1", "app", false).unwrap();
    assert!(
        !created && again.fingerprint == fp,
        "one job per root cause"
    );

    let report = f.run();
    assert_eq!(report["started"][0]["job"], fp.as_str(), "{report}");
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    assert_eq!(job.status, Status::Diagnosed, "{:?}", job.error);
    f.settled("demo-1", &fp);

    let calls = f.calls("claude");
    assert_eq!(calls.len(), 1);
    let argv = &calls[0];
    let tree = PathBuf::from(&job.worktree.path);
    assert_eq!(
        argv[0],
        tree.canonicalize().unwrap().display().to_string(),
        "the agent runs in the worktree"
    );
    assert!(has(
        argv,
        &["--session-id", job.session_id.as_deref().unwrap()]
    ));
    assert!(
        has(argv, &["--tools", "Read,Grep,Glob"]) && has(argv, &["--permission-mode", "dontAsk"])
    );
    assert!(
        has(
            argv,
            &[
                "--strict-mcp-config",
                "--mcp-config",
                r#"{"mcpServers":{}}"#
            ]
        ),
        "no MCP tools"
    );
    assert!(has(argv, &["--setting-sources", ""]) && has(argv, &["-p", "--output-format", "json"]));
    let prompt = argv.last().unwrap();
    assert!(
        prompt.contains("Billing.php") && prompt.contains(packet::START),
        "the prompt carries the packet"
    );
    assert!(
        !prompt.contains("token=secret") && !prompt.contains("ops@example.com"),
        "the packet is bounded and scrubbed"
    );

    let stored = std::fs::read_to_string(job_path(&target, &fp)).unwrap();
    assert!(
        !stored.contains(packet::START) && !stored.contains("Read-only"),
        "no prompt in the job file"
    );
    let leftovers = std::fs::read_dir(jobs_dir(&target))
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains(".packet-"));
    assert!(!leftovers, "the packet file is removed after launch");

    let shown = actions::show(&ctx, "demo-1", &fp).unwrap();
    assert!(shown["diagnosis"]["markdown"]
        .as_str()
        .unwrap()
        .starts_with("# Diagnosis"));
    assert_eq!(
        shown["diagnosis"]["sections"][0]["text"],
        "the plan is null for archived plans"
    );
    let session = job.session_id.clone().unwrap();
    assert_eq!(
        shown["resume_commands"]["local"],
        format!("cd '{}' && claude --resume '{session}'", job.worktree.path)
    );
    assert!(shown["resume_commands"]["ssh"].is_null());
    let listed = actions::list_all(&ctx, None).unwrap();
    assert_eq!(listed["jobs"][0]["has_diagnosis"], true);

    // Read-only: the worktree is the base commit, untouched, and the project is unchanged.
    assert_eq!(
        git(&tree, &["rev-parse", "HEAD"]),
        git(&target.root, &["rev-parse", "main"])
    );
    assert_eq!(
        git(&tree, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );
    assert_eq!(tree_hash(&target.root), project_hash);

    // A finished job is returned as is unless reanalyze is asked for.
    let (same, created) = actions::analyze(&ctx, "demo-1", "test#1", "app", false).unwrap();
    assert!(!created && same.status == Status::Diagnosed);
    let (reset, created) = actions::analyze(&ctx, "demo-1", "test#1", "app", true).unwrap();
    assert!(created && reset.status == Status::Queued && reset.session_id.is_none());
    assert!(!diagnosis_path(&target, &fp).exists());
    f.run();
    assert_eq!(
        f.wait("demo-1", &fp, |j| j.status.terminal()).status,
        Status::Diagnosed
    );
    f.settled("demo-1", &fp);
    assert_eq!(starts_today(&target), 2);
    assert_eq!(
        f.calls("codex").len() + f.calls("herdr").len() + f.calls("fake-difit").len(),
        0
    );
}

#[test]
fn take_over_lets_a_background_agent_finish_its_current_step() {
    let mut f = Fixture::new(&["demo-1"]);
    f.settings.takeover_wait = Duration::from_secs(15);
    let ctx = f.ctx();
    f.mode("sleep");
    let fp = actions::analyze(&ctx, "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    f.run();
    let running = f.wait("demo-1", &fp, |j| j.session_id.is_some() && j.pid.is_some());
    let (session, agent_pid) = (running.session_id.clone().unwrap(), running.pid.unwrap());
    let bucket = f
        .settings
        .home
        .join(".claude/projects")
        .join(agent::claude_bucket(Path::new(&running.worktree.path)));
    std::fs::create_dir_all(&bucket).unwrap();
    let transcript = bucket.join(format!("{session}.jsonl"));
    let line = |v: Value| format!("{v}\n");
    std::fs::write(
        &transcript,
        line(json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "Reading the billing code."},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}}]}})),
    )
    .unwrap();

    let started = Instant::now();
    let (taken, _) = actions::takeover(&ctx, "demo-1", &fp).unwrap();
    assert!(taken.stopping.is_some() && taken.owner == Owner::You);
    assert!(
        matches!(
            actions::handback(&ctx, "demo-1", &fp),
            Err(JobError::Conflict(_))
        ),
        "no hand back while the agent finishes its step"
    );
    std::thread::sleep(f.settings.step_quiet * 4);
    assert!(
        process::alive(agent_pid),
        "an agent with a tool call in flight is not stopped"
    );
    assert!(load(&f.target("demo-1"), &fp)
        .unwrap()
        .unwrap()
        .stopping
        .is_some());

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    use std::io::Write;
    file.write_all(
        (line(json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}}))
            + &line(json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "The plan is null."}]}})))
            .as_bytes(),
    )
    .unwrap();
    drop(file);
    let ended = f.wait("demo-1", &fp, |j| j.stopping.is_none());
    assert!(
        started.elapsed() < f.settings.takeover_wait,
        "it did not wait out the bound"
    );
    let said = &ended.events.last().unwrap().summary;
    assert!(said.contains("stopped after its current step"), "{said}");
    f.settled("demo-1", &fp);
    assert!(
        !process::alive(agent_pid),
        "the agent is stopped at the boundary"
    );
    assert_eq!(ended.owner, Owner::You);
    actions::handback(&ctx, "demo-1", &fp).unwrap();
}

#[test]
fn codex_job_records_its_thread_and_continues_it_after_hand_back() {
    let mut f = Fixture::new(&["demo-1"]);
    f.settings.agent = Some(AgentKind::Codex);
    f.settings.ssh = Some("dev@build-box".into());
    f.settings.takeover_wait = Duration::from_secs(1);
    let ctx = f.ctx();
    f.mode("sleep");
    let (job, _) = actions::analyze(&ctx, "demo-1", "test#1", "app", false).unwrap();
    let fp = job.fingerprint.clone();
    assert_eq!(job.agent, AgentKind::Codex);
    f.run();
    let running = f.wait("demo-1", &fp, |j| j.session_id.is_some() && j.pid.is_some());
    assert_eq!(running.session_id.as_deref(), Some("019a0000-fake-thread"));
    let agent_pid = running.pid.unwrap();

    let (taken, commands) = actions::takeover(&ctx, "demo-1", &fp).unwrap();
    assert_eq!(taken.owner, Owner::You);
    let said = taken
        .events
        .iter()
        .find(|e| e.kind == "takeover")
        .unwrap()
        .summary
        .clone();
    assert!(
        said.contains("stops after its current step (at most 1s)"),
        "{said}"
    );
    assert!(taken.stopping.is_some(), "the job shows Stopping meanwhile");
    // The fake codex writes no transcript, so no step boundary is ever seen.
    let ended = f.wait("demo-1", &fp, |j| j.stopping.is_none());
    let said = &ended.events.last().unwrap().summary;
    assert!(
        said.contains("did not reach a step boundary within 1s")
            && said.contains("stopped immediately"),
        "the take-over says plainly the agent was stopped mid-step: {said}"
    );
    let tree = &taken.worktree.path;
    assert_eq!(
        commands["local"],
        format!("codex resume '019a0000-fake-thread' --cd '{tree}'")
    );
    assert_eq!(commands["ssh"], format!("ssh -t 'dev@build-box' 'codex resume '\\''019a0000-fake-thread'\\'' --cd '\\''{tree}'\\'''"));
    assert!(commands["herdr"]["tab_id"].is_null());
    f.settled("demo-1", &fp);
    assert!(
        !process::alive(agent_pid),
        "a taken-over background agent is stopped"
    );
    let job = load(&f.target("demo-1"), &fp).unwrap().unwrap();
    assert_eq!(
        (job.status, job.owner, job.error.as_deref()),
        (Status::Running, Owner::You, None),
        "not marked failed"
    );
    assert_eq!(
        f.run()["started"],
        json!([]),
        "the runner leaves a job that is yours alone"
    );

    f.mode("ok");
    let back = actions::handback(&ctx, "demo-1", &fp).unwrap();
    assert_eq!((back.status, back.owner), (Status::Queued, Owner::Runner));
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    assert_eq!(job.status, Status::Diagnosed, "{:?}", job.error);
    f.settled("demo-1", &fp);
    let calls = f.calls("codex");
    assert_eq!(calls.len(), 2);
    let canonical = Path::new(tree)
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    assert!(
        has(&calls[0], &["exec", "--cd", tree, "--sandbox", "read-only"]),
        "{:?}",
        &calls[0][..8]
    );
    assert!(has(
        &calls[0],
        &[
            "-c",
            "approval_policy=\"never\"",
            "-c",
            "mcp_servers={}",
            "--json"
        ]
    ));
    assert_eq!(calls[1][0], canonical);
    assert!(
        has(&calls[1], &["exec", "resume", "019a0000-fake-thread"])
            && has(&calls[1], &["-c", "sandbox_mode=\"read-only\""])
    );
    assert_eq!(
        starts_today(&f.target("demo-1")),
        1,
        "a continuation is not a new start"
    );
}

#[test]
fn failures_timeouts_and_stops_are_recorded() {
    let mut f = Fixture::new(&["demo-1"]);
    f.settings.timeout = Duration::from_secs(30);
    let target = f.target("demo-1");
    let analyze = |f: &Fixture, reanalyze| {
        actions::analyze(&f.ctx(), "demo-1", "test#1", "app", reanalyze)
            .unwrap()
            .0
            .fingerprint
    };

    f.mode("fail");
    let fp = analyze(&f, false);
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    assert_eq!(job.status, Status::Failed);
    assert!(
        job.error.as_deref().unwrap().contains("exited with code 3"),
        "{:?}",
        job.error
    );
    f.settled("demo-1", &fp);

    f.mode("garbage");
    std::fs::write(
        f.base().join("claude-reply"),
        json!({"type": "result", "result": "no markers"}).to_string(),
    )
    .unwrap();
    analyze(&f, true);
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    assert_eq!(job.status, Status::Failed);
    assert!(
        job.error.as_deref().unwrap().contains("diagnosis"),
        "{:?}",
        job.error
    );
    assert!(
        !diagnosis_path(&target, &fp).exists(),
        "a malformed reply writes no diagnosis"
    );
    f.settled("demo-1", &fp);

    f.settings.timeout = Duration::from_secs(1);
    f.mode("sleep");
    analyze(&f, true);
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    assert!(
        job.error.as_deref().unwrap().contains("timed out"),
        "{:?}",
        job.error
    );
    f.settled("demo-1", &fp);

    f.settings.timeout = Duration::from_secs(30);
    analyze(&f, true);
    f.run();
    let running = f.wait("demo-1", &fp, |j| j.pid.is_some());
    let pid = running.pid.unwrap();
    let stopped = actions::stop(&f.ctx(), "demo-1", &fp).unwrap();
    assert_eq!(stopped.status, Status::Stopped);
    assert!(!process::alive(pid));
    f.settled("demo-1", &fp);
    let job = load(&target, &fp).unwrap().unwrap();
    assert_eq!(
        (job.status, job.error),
        (Status::Stopped, None),
        "a stopped job is not failed by its supervisor"
    );
    assert!(
        Path::new(&job.worktree.path).is_dir(),
        "stop keeps the worktree"
    );
    assert!(job.session_id.is_some(), "stop keeps the session");
    assert!(
        actions::stop(&f.ctx(), "demo-1", &fp).is_ok(),
        "stopping again is harmless"
    );
    assert!(matches!(
        actions::takeover(&f.ctx(), "demo-1", "0".repeat(64).as_str()),
        Err(JobError::NotFound(_))
    ));
}

#[test]
fn caps_limit_what_starts() {
    let mut f = Fixture::new(&["demo-1", "demo-2"]);
    f.settings.global_cap = 1;
    f.settings.daily_cap = 2;
    f.mode("sleep");
    let ctx = f.ctx();
    let one = actions::analyze(&ctx, "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    let two = actions::analyze(&ctx, "demo-1", "test#2", "app", false)
        .unwrap()
        .0
        .fingerprint;
    let c = actions::analyze(&ctx, "demo-2", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    assert_ne!(one, c, "the same frame in another project is another job");
    let report = f.run();
    assert_eq!(
        report["started"].as_array().unwrap().len(),
        1,
        "global cap: {report}"
    );
    let a = report["started"][0]["job"].as_str().unwrap().to_owned();
    let b = if a == one { two } else { one };
    f.wait("demo-1", &a, |j| j.pid.is_some());
    assert_eq!(f.run()["started"], json!([]));

    f.settings.global_cap = 4;
    let report = f.run();
    assert_eq!(
        report["started"],
        json!([{"target": "demo-2", "job": c}]),
        "one running job per project: {report}"
    );
    f.wait("demo-2", &c, |j| j.pid.is_some());
    let ctx = f.ctx();
    actions::stop(&ctx, "demo-1", &a).unwrap();
    f.settled("demo-1", &a);
    let report = f.run();
    assert_eq!(report["started"][0]["job"], b.as_str(), "{report}");
    f.wait("demo-1", &b, |j| j.pid.is_some());
    actions::stop(&ctx, "demo-1", &b).unwrap();
    actions::stop(&ctx, "demo-2", &c).unwrap();
    f.settled("demo-1", &b);
    f.settled("demo-2", &c);

    // demo-1 has started two jobs today: a new or re-run job is refused, at enqueue and at start.
    let error = actions::analyze(&ctx, "demo-1", "test#1", "app", true).unwrap_err();
    assert_eq!(error.exit_code(), 6, "{error}");
    let target = f.target("demo-2");
    let (_, ()) = mutate(&target, &c, |job| {
        job.status = Status::Queued;
        job.session_id = None;
        Ok(())
    })
    .unwrap();
    count_start(&target).unwrap();
    let report = f.run();
    assert_eq!(report["waiting"][0]["job"], c.as_str(), "{report}");
    let job = load(&target, &c).unwrap().unwrap();
    assert_eq!(
        (job.status, job.events.last().unwrap().kind.as_str()),
        (Status::Queued, "waiting")
    );
}

#[test]
fn herdr_runs_the_agent_in_a_tab_and_reads_its_transcript() {
    let mut f = Fixture::new(&["demo-1"]);
    let base = f.base();
    f.settings.herdr = Some(vec![base.join("fakes/herdr").display().to_string()]);
    f.settings.herdr_workspace = Some("w1".into());
    let ctx = f.ctx();
    let (job, _) = actions::analyze(&ctx, "demo-1", "test#1", "app", false).unwrap();
    assert_eq!(job.launcher, Launcher::Herdr);
    let fp = job.fingerprint.clone();
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    assert_eq!(job.status, Status::Diagnosed, "{:?}", job.error);
    f.settled("demo-1", &fp);
    assert_eq!(
        job.herdr_tab,
        Some(HerdrTab {
            tab_id: "w1:t7".into(),
            pane_id: "w1:p7".into()
        })
    );
    let session = job.session_id.clone().unwrap();
    let name = format!("issue-{}", &fp[..8]);

    let tab = &f.calls("herdr-tab-create")[0];
    assert!(has(
        tab,
        &[
            "--workspace",
            "w1",
            "--cwd",
            &job.worktree.path,
            "--label",
            &name,
            "--no-focus"
        ]
    ));
    let start = &f.calls("herdr-agent-start")[0];
    assert!(has(
        start,
        &[
            "agent",
            "start",
            &name,
            "--kind",
            "claude",
            "--pane",
            "w1:p7",
            "--",
            "--session-id",
            &session
        ]
    ));
    assert!(has(start, &["--tools", "Read,Grep,Glob"]) && has(start, &["--strict-mcp-config"]));
    let prompt = &f.calls("herdr-agent-prompt")[0];
    assert!(has(
        prompt,
        &[
            "--wait",
            "--until",
            "done",
            "--until",
            "idle",
            "--until",
            "blocked",
            "--timeout"
        ]
    ));
    assert_eq!(
        f.calls("claude").len(),
        0,
        "the agent ran in Herdr, not in the background"
    );

    let commands = actions::resume_commands(&ctx, &job);
    assert_eq!(commands["herdr"]["tab_id"], "w1:t7");
    assert_eq!(
        commands["herdr"]["command"],
        format!("herdr agent focus '{name}'")
    );
}

#[test]
fn the_diff_viewer_is_local_idempotent_and_stopped_with_its_job() {
    let f = Fixture::new(&["demo-1"]);
    let ctx = f.ctx();
    let target = f.target("demo-1");
    let fp = actions::analyze(&ctx, "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    assert!(
        matches!(
            actions::difit_start(&ctx, "demo-1", &fp),
            Err(JobError::Conflict(_))
        ),
        "no worktree yet"
    );
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    f.settled("demo-1", &fp);

    let started = actions::difit_start(&ctx, "demo-1", &fp).unwrap();
    let (port, pid) = (started.difit.port.unwrap(), started.difit.pid.unwrap());
    assert!(difit::running(&target, &started));
    let call = &f.calls("fake-difit")[0];
    assert_eq!(
        call[0],
        Path::new(&job.worktree.path)
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    assert!(has(
        call,
        &[
            ".",
            "main",
            "--no-open",
            "--port",
            &port.to_string(),
            "--host",
            "127.0.0.1",
            "--keep-alive"
        ]
    ));
    assert_eq!(
        actions::difit_start(&ctx, "demo-1", &fp).unwrap().difit.pid,
        Some(pid),
        "start is idempotent"
    );
    assert_eq!(f.calls("fake-difit").len(), 1);
    assert!(
        matches!(
            actions::cleanup(&ctx, "demo-1", &fp, false),
            Err(JobError::Conflict(_))
        ),
        "cleanup waits for the viewer"
    );

    let stopped = actions::difit_stop(&ctx, "demo-1", &fp).unwrap();
    assert_eq!(stopped.difit, Difit::default());
    assert!(!process::alive(pid));
    actions::difit_stop(&ctx, "demo-1", &fp).unwrap();

    // A viewer started while the agent runs stops when the job is stopped.
    f.mode("sleep");
    actions::analyze(&ctx, "demo-1", "test#1", "app", true).unwrap();
    f.run();
    f.wait("demo-1", &fp, |j| j.pid.is_some());
    let pid = actions::difit_start(&ctx, "demo-1", &fp)
        .unwrap()
        .difit
        .pid
        .unwrap();
    actions::stop(&ctx, "demo-1", &fp).unwrap();
    assert!(!process::alive(pid));
    f.settled("demo-1", &fp);

    let (job, done) = actions::cleanup(&ctx, "demo-1", &fp, false).unwrap();
    assert!(!Path::new(&job.worktree.path).exists(), "{done:?}");
    assert_eq!(
        git(&target.root, &["branch", "--list", &job.worktree.branch]),
        "",
        "an unchanged branch is removed"
    );
}

#[test]
fn jobs_whose_supervisor_is_gone_are_failed_on_the_next_scan() {
    let f = Fixture::new(&["demo-1"]);
    let ctx = f.ctx();
    let target = f.target("demo-1");
    let fp = actions::analyze(&ctx, "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    let mut child = Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    mutate(&target, &fp, |job| {
        job.status = Status::Running;
        job.difit = Difit {
            port: Some(1),
            pid: Some(dead),
            started_at: Some(now()),
        };
        let gone = Proc {
            pid: dead,
            pgid: dead,
            os_start: "1.000000".into(),
            exe: "/usr/local/bin/harness-signals".into(),
            started_at: now(),
        };
        write_procs(&target, &fp, |p| {
            p.supervisor = Some(gone.clone());
            p.difit = Some(gone.clone());
        })?;
        Ok(())
    })
    .unwrap();
    let report = f.run();
    assert_eq!(report["recovered"], json!([fp]), "{report}");
    let job = load(&target, &fp).unwrap().unwrap();
    assert_eq!(job.status, Status::Failed);
    assert!(job
        .error
        .as_deref()
        .unwrap()
        .contains("stopped supervising"));
    assert_eq!(job.difit, Difit::default(), "a dead viewer is cleared");
    assert_eq!(read_procs(&target, &fp), Procs::default());
    assert_eq!(f.calls("claude").len(), 0);
}

#[test]
fn an_agent_whose_identity_changed_is_detached_not_signalled() {
    let f = Fixture::new(&["demo-1"]);
    let ctx = f.ctx();
    let target = f.target("demo-1");
    let fp = actions::analyze(&ctx, "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    // A live process in its own group standing where the agent's pid was recorded.
    let mut bystander = Command::new("/bin/sh");
    bystander.args(["-c", "sleep 30"]);
    {
        use std::os::unix::process::CommandExt;
        bystander.process_group(0);
    }
    let mut dead = Command::new("true").spawn().unwrap();
    let dead_pid = dead.id();
    dead.wait().unwrap();
    // The job runs, but its supervisor is gone and the agent's recorded identity is wrong.
    let detach_over = |bystander: &std::process::Child| {
        let real = process::capture(bystander.id());
        mutate(&target, &fp, |job| {
            job.status = Status::Running;
            write_procs(&target, &fp, |p| {
                p.supervisor = Some(Proc {
                    pid: dead_pid,
                    pgid: dead_pid,
                    os_start: "1.000000".into(),
                    exe: "/usr/local/bin/harness-signals".into(),
                    started_at: now(),
                });
                p.agent = Some(Proc {
                    exe: "/opt/agents/claude".into(),
                    ..real.clone()
                });
            })?;
            Ok(())
        })
        .unwrap();
    };
    let mut bystander = bystander.spawn().unwrap();
    detach_over(&bystander);
    let report = f.run();
    assert_eq!(report["recovered"], json!([fp]), "{report}");
    let job = load(&target, &fp).unwrap().unwrap();
    assert_eq!(job.status, Status::Detached);
    let said = job.events.last().unwrap();
    assert_eq!(said.kind, "detached");
    assert!(
        said.summary.contains("not signalled") && said.summary.contains("instead of"),
        "{}",
        said.summary
    );
    assert!(
        process::alive(bystander.id()),
        "the bystander was not signalled"
    );
    assert!(
        matches!(
            actions::analyze(&ctx, "demo-1", "test#1", "app", true),
            Err(JobError::Conflict(_))
        ),
        "a detached job is not reanalyzed over a possibly running agent"
    );
    assert_eq!(
        read_procs(&target, &fp).agent.map(|p| p.pid),
        Some(bystander.id()),
        "the unresolved identity stays on the job"
    );
    let shown = actions::show(&ctx, "demo-1", &fp).unwrap();
    assert_eq!(shown["unresolved_processes"][0]["pid"], bystander.id());
    match actions::cleanup(&ctx, "demo-1", &fp, false) {
        Err(JobError::Conflict(why)) => assert!(why.contains("--force-detached"), "{why}"),
        other => panic!("cleanup must refuse while the process may run: {other:?}"),
    }
    assert!(process::alive(bystander.id()));

    // Once the recorded process is confirmed gone, cleanup goes ahead.
    bystander.kill().unwrap();
    bystander.wait().unwrap();
    let (cleaned, _) = actions::cleanup(&ctx, "demo-1", &fp, false).unwrap();
    assert_eq!(
        cleaned.status,
        Status::Stopped,
        "cleanup releases a detached job"
    );
    assert_eq!(read_procs(&target, &fp), Procs::default());

    // With --force-detached the person vouches it ended; nothing is signalled.
    actions::analyze(&ctx, "demo-1", "test#1", "app", true).unwrap();
    let mut bystander = Command::new("/bin/sh");
    bystander.args(["-c", "sleep 30"]);
    {
        use std::os::unix::process::CommandExt;
        bystander.process_group(0);
    }
    let mut bystander = bystander.spawn().unwrap();
    detach_over(&bystander);
    f.run();
    assert_eq!(
        load(&target, &fp).unwrap().unwrap().status,
        Status::Detached
    );
    assert!(actions::cleanup(&ctx, "demo-1", &fp, false).is_err());
    let (forced, _) = actions::cleanup(&ctx, "demo-1", &fp, true).unwrap();
    assert_eq!(forced.status, Status::Stopped);
    assert!(forced
        .events
        .last()
        .unwrap()
        .summary
        .contains("--force-detached"));
    assert!(process::alive(bystander.id()), "force never signals");
    bystander.kill().unwrap();
    bystander.wait().unwrap();
}

#[test]
fn an_existing_branch_is_reused_only_when_it_is_this_jobs() {
    let f = Fixture::new(&["demo-1"]);
    let ctx = f.ctx();
    let root = f.target("demo-1").root;
    let fp = actions::analyze(&ctx, "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    let branch = format!("nw/{}", &fp[..12]);
    let rerun = |expect: Status| -> Job {
        let (job, _) = actions::analyze(&ctx, "demo-1", "test#1", "app", true).unwrap();
        assert_eq!(job.status, Status::Queued);
        f.run();
        let job = f.wait("demo-1", &fp, |j| j.status.terminal());
        f.settled("demo-1", &fp);
        assert_eq!(job.status, expect, "{:?}", job.error);
        job
    };

    // A branch the job did not create.
    git(&root, &["branch", &branch, "main"]);
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    f.settled("demo-1", &fp);
    let error = job.error.unwrap();
    assert!(
        error.contains("never recorded creating it")
            && error.contains(&format!("jobs cleanup --target demo-1 --job {fp}")),
        "{error}"
    );

    // Created by the job: its base commit is recorded, and its own commits are fine.
    git(&root, &["branch", "-D", &branch]);
    let job = rerun(Status::Diagnosed);
    let main = git(&root, &["rev-parse", "main"]);
    assert_eq!(job.worktree.base_commit.as_deref(), Some(main.as_str()));
    let tree = PathBuf::from(&job.worktree.path);
    std::fs::write(tree.join("note.txt"), "mine\n").unwrap();
    git(&tree, &["add", "note.txt"]);
    git(&tree, &["commit", "-q", "-m", "made on the job's branch"]);
    rerun(Status::Diagnosed);

    // Checked out in another worktree.
    git(&root, &["worktree", "remove", &job.worktree.path]);
    let other = f.base().join("other-checkout");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            &other.display().to_string(),
            &branch,
        ],
    );
    assert!(rerun(Status::Failed)
        .error
        .unwrap()
        .contains("is checked out in"));
    git(&root, &["worktree", "remove", &other.display().to_string()]);

    // Moved to commits that also exist on another branch.
    git(
        &root,
        &["commit", "-q", "--allow-empty", "-m", "main moves on"],
    );
    git(&root, &["branch", "-f", &branch, "main"]);
    assert!(rerun(Status::Failed)
        .error
        .unwrap()
        .contains("also exist elsewhere"));

    // Moved to an unrelated history.
    let orphan = git(&root, &["commit-tree", "main^{tree}", "-m", "unrelated"]);
    git(&root, &["branch", "-f", &branch, &orphan]);
    assert!(rerun(Status::Failed)
        .error
        .unwrap()
        .contains("does not descend from the job's base commit"));
    assert_eq!(
        f.calls("claude").len(),
        2,
        "no agent ran on a refused branch"
    );
}

fn herdr_fixture() -> Fixture {
    let mut f = Fixture::new(&["demo-1"]);
    let base = f.base();
    f.settings.herdr = Some(vec![base.join("fakes/herdr").display().to_string()]);
    f.settings.herdr_workspace = Some("w1".into());
    std::fs::create_dir_all(base.join("herdr-state")).unwrap();
    f
}

/// Start a Herdr job whose agent keeps working until it is interrupted.
fn working_herdr_job(f: &Fixture, flags: &[&str]) -> String {
    let fp = start_busy_herdr_job(f, flags);
    let state = f.base().join("herdr-state");
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::fs::read_to_string(state.join("status"))
        .ok()
        .as_deref()
        != Some("working\n")
    {
        assert!(
            Instant::now() < deadline,
            "the fake agent never started working"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fp
}

fn start_busy_herdr_job(f: &Fixture, flags: &[&str]) -> String {
    let state = f.base().join("herdr-state");
    for flag in ["busy"].iter().chain(flags) {
        std::fs::write(state.join(flag), "").unwrap();
    }
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    f.run();
    fp
}

fn summaries(job: &Job) -> String {
    job.events
        .iter()
        .map(|e| format!("{}: {}", e.kind, e.summary))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stop, then Analyze again while the stopped run's supervisor is still waiting
/// on its prompt: when that prompt finally ends, the old supervisor must leave
/// the new run alone (no status, no Herdr interrupt, no process records).
#[test]
fn a_supervisor_left_from_a_stopped_run_does_not_touch_the_rerun() {
    let mut f = herdr_fixture();
    // The old supervisor must not notice the stop by heartbeat before its prompt ends.
    f.settings.heartbeat = Duration::from_secs(120);
    let state = f.base().join("herdr-state");
    let target = f.target("demo-1");
    let prompts = || {
        std::fs::read_to_string(state.join("prompts"))
            .ok()
            .and_then(|n| n.trim().parse::<u32>().ok())
            .unwrap_or(0)
    };
    let wait_prompts = |n: u32| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while prompts() < n {
            assert!(Instant::now() < deadline, "prompt {n} never started");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    std::fs::write(state.join("hold"), "").unwrap();
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    f.run();
    wait_prompts(1);
    let first = load(&target, &fp).unwrap().unwrap().run;
    assert!(first.is_some(), "a started job carries its run token");

    let stopped = actions::stop(&f.ctx(), "demo-1", &fp).unwrap();
    assert_eq!(stopped.status, Status::Stopped, "{}", summaries(&stopped));
    actions::analyze(&f.ctx(), "demo-1", "test#1", "app", true).unwrap();
    f.run();
    wait_prompts(2);
    let rerun = load(&target, &fp).unwrap().unwrap();
    assert_eq!(rerun.status, Status::Running);
    assert!(
        rerun.run.is_some() && rerun.run != first,
        "each start gets a new token"
    );
    let supervisor = read_procs(&target, &fp).supervisor;
    assert!(supervisor.is_some());
    let interrupts = f.calls("herdr-agent-send").len();

    std::fs::write(state.join("release-1"), "").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let after = load(&target, &fp).unwrap().unwrap();
    assert_eq!(after.status, Status::Running, "{}", summaries(&after));
    assert_eq!(after.run, rerun.run);
    assert_eq!(
        after.events, rerun.events,
        "the old supervisor recorded nothing"
    );
    assert_eq!(
        f.calls("herdr-agent-send").len(),
        interrupts,
        "nor interrupted the new agent"
    );
    assert_eq!(
        read_procs(&target, &fp).supervisor,
        supervisor,
        "nor cleared the new supervisor"
    );

    std::fs::write(state.join("release-2"), "").unwrap();
    let failed = f.wait("demo-1", &fp, |j| j.status == Status::Failed);
    f.settled("demo-1", &fp);
    assert_eq!(
        failed.events.iter().filter(|e| e.kind == "failed").count(),
        1,
        "only the new run's own supervisor ended it: {}",
        summaries(&failed)
    );
}

#[test]
fn herdr_stop_interrupts_the_agent_and_keeps_its_tab_when_it_goes_idle() {
    let f = herdr_fixture();
    let fp = working_herdr_job(&f, &[]);
    let name = format!("issue-{}", &fp[..8]);
    let job = actions::stop(&f.ctx(), "demo-1", &fp).unwrap();
    assert_eq!(job.status, Status::Stopped, "{}", summaries(&job));
    assert!(job.stopping.is_none());
    assert!(job.herdr_tab.is_some(), "the tab stays open");
    assert!(summaries(&job).contains("is idle in Herdr tab w1:t7"));
    assert!(has(
        &f.calls("herdr-agent-send")[0],
        &["agent", "send-keys", &name, "esc"]
    ));
    assert!(f.calls("herdr-tab-close").is_empty());
    f.settled("demo-1", &fp);
    let after = load(&f.target("demo-1"), &fp).unwrap().unwrap();
    assert_eq!(
        after.status,
        Status::Stopped,
        "the supervisor let go without a status of its own"
    );
    assert!(
        actions::resume_commands(&f.ctx(), &after)["herdr"]["command"]
            .as_str()
            .unwrap()
            .contains("herdr agent focus")
    );
}

#[test]
fn herdr_stop_closes_the_tab_of_an_agent_that_does_not_go_idle() {
    let f = herdr_fixture();
    let fp = working_herdr_job(&f, &["stuck"]);
    let name = format!("issue-{}", &fp[..8]);
    let job = actions::stop(&f.ctx(), "demo-1", &fp).unwrap();
    assert_eq!(job.status, Status::Stopped, "{}", summaries(&job));
    assert!(job.herdr_tab.is_none(), "the closed tab is forgotten");
    assert!(summaries(&job).contains("so Herdr tab w1:t7 was closed"));
    let wait = &f.calls("herdr-agent-wait")[0];
    assert!(has(
        wait,
        &[
            "agent",
            "wait",
            &name,
            "--until",
            "idle",
            "--until",
            "done",
            "--timeout",
            "500"
        ]
    ));
    assert!(has(
        &f.calls("herdr-tab-close")[0],
        &["tab", "close", "w1:t7"]
    ));
    assert!(has(
        &f.calls("herdr-pane-process")[0],
        &["pane", "process-info", "--pane", "w1:p7"]
    ));
    f.settled("demo-1", &fp);
}

#[test]
fn herdr_stop_detaches_an_agent_it_cannot_stop() {
    for flags in [&["stuck", "noclose"][..], &["stuck", "ghost"][..]] {
        let f = herdr_fixture();
        let fp = working_herdr_job(&f, flags);
        let job = actions::stop(&f.ctx(), "demo-1", &fp).unwrap();
        assert_eq!(
            job.status,
            Status::Detached,
            "{flags:?}: {}",
            summaries(&job)
        );
        assert!(
            job.error
                .as_deref()
                .unwrap()
                .contains("may still be working in Herdr tab w1:t7"),
            "{:?}",
            job.error
        );
        assert!(
            !summaries(&job).contains("stopped:"),
            "never called stopped"
        );
        f.settled("demo-1", &fp);
        assert_eq!(
            load(&f.target("demo-1"), &fp).unwrap().unwrap().status,
            Status::Detached
        );
    }
}

#[test]
fn a_herdr_timeout_stops_the_agent_before_failing_the_job() {
    for (flags, expected, says) in [
        (
            &[][..],
            Status::Failed,
            "interrupted and is idle in Herdr tab w1:t7",
        ),
        (
            &["stuck"][..],
            Status::Failed,
            "so Herdr tab w1:t7 was closed",
        ),
        (
            &["stuck", "noclose"][..],
            Status::Detached,
            "may still be working",
        ),
    ] {
        let mut f = herdr_fixture();
        f.settings.timeout = Duration::from_millis(600);
        let fp = start_busy_herdr_job(&f, flags);
        let job = f.wait("demo-1", &fp, |j| j.status.terminal());
        f.settled("demo-1", &fp);
        assert_eq!(job.status, expected, "{flags:?}: {}", summaries(&job));
        let error = job.error.clone().unwrap();
        assert!(
            error.starts_with("timed out after") && error.contains(says),
            "{error}"
        );
        assert!(
            !f.calls("herdr-agent-send").is_empty(),
            "the agent was interrupted"
        );
    }
}

#[test]
fn a_herdr_launch_that_fails_after_its_tab_exists_closes_the_tab() {
    let f = herdr_fixture();
    let state = f.base().join("herdr-state");
    std::fs::write(state.join("startfail"), "").unwrap();
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    f.settled("demo-1", &fp);
    assert_eq!(job.status, Status::Failed);
    assert!(
        job.error.as_deref().unwrap_or("").contains("agent start"),
        "{:?}",
        job.error
    );
    assert!(
        summaries(&job)
            .contains("the launch failed after Herdr tab w1:t7 was created, so the tab was closed"),
        "{}",
        summaries(&job)
    );
    assert_eq!(job.herdr_tab, None, "a closed tab is not kept on the job");
    assert!(has(
        &f.calls("herdr-tab-close")[0],
        &["tab", "close", "w1:t7"]
    ));

    // When the tab cannot be closed, the job says so and keeps the tab to close by hand.
    std::fs::write(state.join("noclose"), "").unwrap();
    actions::analyze(&f.ctx(), "demo-1", "test#1", "app", true).unwrap();
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    f.settled("demo-1", &fp);
    assert_eq!(job.status, Status::Failed);
    assert!(
        summaries(&job).contains("the tab could not be closed")
            && summaries(&job).contains("close it yourself"),
        "{}",
        summaries(&job)
    );
    assert_eq!(job.herdr_tab.map(|t| t.tab_id).as_deref(), Some("w1:t7"));
}

#[test]
fn a_herdr_tab_id_is_read_from_the_root_pane_when_the_tab_is_missing() {
    let f = herdr_fixture();
    std::fs::write(f.base().join("herdr-state/roottab"), "").unwrap();
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    f.run();
    let job = f.wait("demo-1", &fp, |j| j.status.terminal());
    f.settled("demo-1", &fp);
    assert_eq!(job.status, Status::Diagnosed, "{:?}", job.error);
    assert_eq!(job.herdr_tab.map(|t| t.tab_id).as_deref(), Some("w1:t7"));
}

/// Every call the fake herdr received, as `group verb`, oldest first.
fn herdr_verbs(f: &Fixture) -> Vec<String> {
    f.calls("herdr-")
        .into_iter()
        .map(|argv| argv[..2.min(argv.len())].join(" "))
        .collect()
}

/// A Herdr job the person took over while its agent works, with the supervisor gone.
fn taken_over_herdr_job(f: &Fixture) -> (String, Job) {
    let fp = working_herdr_job(f, &[]);
    f.wait("demo-1", &fp, |j| j.session_id.is_some());
    let (taken, _) = actions::takeover(&f.ctx(), "demo-1", &fp).unwrap();
    assert_eq!((taken.owner, taken.status), (Owner::You, Status::Running));
    f.settled("demo-1", &fp);
    std::fs::remove_file(f.base().join("herdr-state/busy")).unwrap();
    (fp, taken)
}

#[test]
fn a_taken_over_herdr_job_whose_tab_vanished_ends_without_being_touched() {
    let f = herdr_fixture();
    let (fp, _) = taken_over_herdr_job(&f);
    let before = herdr_verbs(&f).len();

    // While the agent still works in its tab, observation only records that it looked.
    let report = f.run();
    assert_eq!(report["observed"], json!([]), "{report}");
    let job = load(&f.target("demo-1"), &fp).unwrap().unwrap();
    assert_eq!((job.status, job.owner), (Status::Running, Owner::You));
    assert!(job.observed_at.is_some(), "observed_at is recorded");

    // The agent ends and Herdr drops its tab. The start-up reconcile closes the job.
    std::fs::write(f.base().join("herdr-state/closed"), "").unwrap();
    let report = super::runner::reconcile(&f.ctx()).unwrap();
    assert_eq!(
        report["observed"],
        json!([{"job": fp, "status": "ended", "target": "demo-1"}]),
        "{report}"
    );
    let job = load(&f.target("demo-1"), &fp).unwrap().unwrap();
    assert_eq!(
        (job.status, job.owner),
        (Status::Ended, Owner::You),
        "{}",
        summaries(&job)
    );
    let said = job.events.last().unwrap();
    assert_eq!(said.kind, "ended");
    assert!(
        said.summary.contains("Herdr no longer has agent")
            && said.summary.contains("its tab is gone"),
        "the event says how the end was seen: {}",
        said.summary
    );
    assert!(job.finished_at.is_some() && job.error.is_none());
    let after: Vec<String> = herdr_verbs(&f).split_off(before);
    assert!(
        after
            .iter()
            .all(|v| v == "agent get" || v == "pane process-info"),
        "observation of a job that is yours only reads: {after:?}"
    );
    assert!(
        after.iter().any(|v| v == "pane process-info"),
        "the tab was checked"
    );
    assert_eq!(
        f.run()["observed"],
        json!([]),
        "an ended job is not observed again"
    );

    let listed = actions::list_all(&f.ctx(), None).unwrap();
    assert!(listed["runner"].is_null(), "no tick recorded yet");
    super::runner::record_tick(&f.db);
    let listed = actions::list_all(&f.ctx(), None).unwrap();
    assert!(listed["runner"]["ticked_at"].is_string(), "{listed}");
    assert!(listed["jobs"][0]["observed_at"].is_string());
}

#[test]
fn a_taken_over_job_whose_transcript_has_a_diagnosis_is_diagnosed() {
    // Herdr: the person's agent replies with the diagnosis and goes idle in its tab.
    let f = herdr_fixture();
    let (fp, taken) = taken_over_herdr_job(&f);
    let session = taken.session_id.clone().unwrap();
    let bucket = f
        .settings
        .home
        .join(".claude/projects")
        .join(agent::claude_bucket(Path::new(&taken.worktree.path)));
    std::fs::create_dir_all(&bucket).unwrap();
    std::fs::copy(
        f.base().join("claude-transcript"),
        bucket.join(format!("{session}.jsonl")),
    )
    .unwrap();
    std::fs::write(f.base().join("herdr-state/status"), "idle\n").unwrap();
    let before = herdr_verbs(&f).len();
    let report = f.run();
    assert_eq!(report["observed"][0]["status"], "diagnosed", "{report}");
    let job = load(&f.target("demo-1"), &fp).unwrap().unwrap();
    assert_eq!(
        (job.status, job.owner),
        (Status::Diagnosed, Owner::You),
        "{}",
        summaries(&job)
    );
    assert!(job
        .events
        .last()
        .unwrap()
        .summary
        .contains("read from the session transcript"));
    let shown = actions::show(&f.ctx(), "demo-1", &fp).unwrap();
    assert_eq!(
        shown["diagnosis"]["sections"][0]["text"],
        "the plan is null for archived plans"
    );
    assert!(
        herdr_verbs(&f)[before..].iter().all(|v| v == "agent get"),
        "nothing but agent get: {:?}",
        &herdr_verbs(&f)[before..]
    );
    assert!(job.herdr_tab.is_some(), "the tab is left open");

    // Background: taken over, its agent stopped; the person resumes elsewhere and the reply lands.
    let mut f = Fixture::new(&["demo-1"]);
    f.settings.takeover_wait = Duration::from_millis(300);
    f.mode("sleep");
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    f.run();
    let running = f.wait("demo-1", &fp, |j| j.session_id.is_some() && j.pid.is_some());
    actions::takeover(&f.ctx(), "demo-1", &fp).unwrap();
    f.wait("demo-1", &fp, |j| j.stopping.is_none());
    f.settled("demo-1", &fp);
    assert_eq!(
        f.run()["observed"],
        json!([]),
        "no transcript yet: still the person's"
    );
    let bucket = f
        .settings
        .home
        .join(".claude/projects")
        .join(agent::claude_bucket(Path::new(&running.worktree.path)));
    std::fs::create_dir_all(&bucket).unwrap();
    let path = bucket.join(format!("{}.jsonl", running.session_id.unwrap()));
    std::fs::copy(f.base().join("claude-transcript"), &path).unwrap();
    f.run();
    let job = load(&f.target("demo-1"), &fp).unwrap().unwrap();
    assert_eq!(
        (job.status, job.owner),
        (Status::Diagnosed, Owner::You),
        "{}",
        summaries(&job)
    );
    let written = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        job.last_activity_at.as_deref(),
        Some(super::observe::stamp(written).as_str()),
        "activity is the transcript's last write"
    );
}

#[test]
fn a_live_background_agent_that_is_yours_is_observed_but_never_signalled() {
    let f = Fixture::new(&["demo-1"]);
    let target = f.target("demo-1");
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    // Keep a stable executable and enough lifetime for the full suite under load.
    let mut agent = Command::new("/bin/sleep");
    agent.arg("120");
    {
        use std::os::unix::process::CommandExt;
        agent.process_group(0);
    }
    let mut agent = agent.spawn().unwrap();
    let identity = process::capture(agent.id());
    let session = "5e551011-0000-4000-8000-000000000001";
    let worktree = PathBuf::from(&load(&target, &fp).unwrap().unwrap().worktree.path);
    let bucket = f
        .settings
        .home
        .join(".claude/projects")
        .join(agent::claude_bucket(&worktree));
    std::fs::create_dir_all(&bucket).unwrap();
    // Even a finished reply with a diagnosis does not end a job whose agent still runs.
    std::fs::copy(
        f.base().join("claude-transcript"),
        bucket.join(format!("{session}.jsonl")),
    )
    .unwrap();
    let make_yours = |agent: Proc| {
        mutate(&target, &fp, |job| {
            job.status = Status::Running;
            job.owner = Owner::You;
            job.session_id = Some(session.into());
            job.last_activity_at = Some("2026-01-01T00:00:00Z".into());
            write_procs(&target, &fp, |p| p.agent = Some(agent.clone()))?;
            Ok(())
        })
        .unwrap();
    };
    make_yours(identity.clone());
    let mut settings = f.settings.clone();
    settings.idle_end = Duration::from_millis(10);
    let ctx = Context {
        db_root: &f.db,
        config: Some(&f.config),
        settings: &settings,
    };
    let report = super::runner::reconcile(&ctx).unwrap();
    assert_eq!(report["observed"], json!([]), "{report}");
    assert_eq!(report["recovered"], json!([]), "{report}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(agent.try_wait(), Ok(None)),
        "the agent was not signalled"
    );
    let job = load(&target, &fp).unwrap().unwrap();
    assert_eq!(
        (job.status, job.owner, job.pid),
        (Status::Running, Owner::You, None)
    );
    assert!(job.observed_at.is_some());
    assert_ne!(
        job.last_activity_at.as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "activity comes from the transcript"
    );
    assert_eq!(
        read_procs(&target, &fp).agent,
        Some(identity.clone()),
        "its record is kept"
    );

    // A recorded identity that no longer matches is detached, still without a signal.
    make_yours(Proc {
        exe: "/opt/agents/claude".into(),
        ..identity
    });
    super::runner::reconcile(&ctx).unwrap();
    let job = load(&target, &fp).unwrap().unwrap();
    assert_eq!(job.status, Status::Detached, "{}", summaries(&job));
    assert!(job
        .error
        .as_deref()
        .unwrap()
        .contains("nothing was signalled"));
    assert!(matches!(agent.try_wait(), Ok(None)), "still not signalled");

    // Once the agent has exited, the job ends.
    agent.kill().unwrap();
    agent.wait().unwrap();
    let fp2 = actions::analyze(&f.ctx(), "demo-1", "test#2", "app", false)
        .unwrap()
        .0
        .fingerprint;
    mutate(&target, &fp2, |job| {
        job.status = Status::Running;
        job.owner = Owner::You;
        job.last_activity_at = Some("2026-01-01T00:00:00Z".into());
        Ok(())
    })
    .unwrap();
    let report = super::runner::reconcile(&ctx).unwrap();
    assert_eq!(
        report["observed"],
        json!([{"job": fp2, "status": "ended", "target": "demo-1"}]),
        "{report}"
    );
    let ended = load(&target, &fp2).unwrap().unwrap();
    assert!(
        summaries(&ended).contains("no agent process runs for this job"),
        "{}",
        summaries(&ended)
    );
    assert_eq!(f.calls("claude").len(), 0);
}

#[test]
fn an_orphaned_runner_job_with_a_diagnosis_in_its_transcript_is_diagnosed_not_failed() {
    let f = Fixture::new(&["demo-1"]);
    let target = f.target("demo-1");
    let fp = actions::analyze(&f.ctx(), "demo-1", "test#1", "app", false)
        .unwrap()
        .0
        .fingerprint;
    let session = "5e551011-0000-4000-8000-000000000002";
    let worktree = PathBuf::from(&load(&target, &fp).unwrap().unwrap().worktree.path);
    let bucket = f
        .settings
        .home
        .join(".claude/projects")
        .join(agent::claude_bucket(&worktree));
    std::fs::create_dir_all(&bucket).unwrap();
    std::fs::copy(
        f.base().join("claude-transcript"),
        bucket.join(format!("{session}.jsonl")),
    )
    .unwrap();
    let mut dead = Command::new("true").spawn().unwrap();
    let dead_pid = dead.id();
    dead.wait().unwrap();
    mutate(&target, &fp, |job| {
        job.status = Status::Running;
        job.session_id = Some(session.into());
        write_procs(&target, &fp, |p| {
            p.supervisor = Some(Proc {
                pid: dead_pid,
                pgid: dead_pid,
                os_start: "1.000000".into(),
                exe: "/usr/local/bin/harness-signals".into(),
                started_at: now(),
            });
        })?;
        Ok(())
    })
    .unwrap();
    let report = super::runner::reconcile(&f.ctx()).unwrap();
    assert_eq!(report["recovered"], json!([fp]), "{report}");
    let job = load(&target, &fp).unwrap().unwrap();
    assert_eq!(job.status, Status::Diagnosed, "{}", summaries(&job));
    assert!(diagnosis_path(&target, &fp).is_file());
    assert_eq!(read_procs(&target, &fp), Procs::default());
}
