use super::*;
use crate::jobs::packet::{parse, render, sections, Diagnosis, END, START};
use std::process::Command;

fn target(dir: &Path) -> Target {
    let (db, _) = crate::store::testing::db_with_target(dir);
    crate::store::resolve(&db, "demo-1").unwrap()
}

fn sample(fp: &str) -> Job {
    Job {
        version: 1,
        fingerprint: fp.to_owned(),
        target_id: "demo-1".into(),
        card_id: "app".into(),
        issue_key: "t#1".into(),
        issue_title: "Boom".into(),
        issue_url: None,
        root_frame: RootFrame {
            path: "src/app/a.php".into(),
            line: 3,
        },
        mode: "analyze".into(),
        status: Status::Queued,
        owner: Owner::Runner,
        created_at: now(),
        updated_at: now(),
        agent: AgentKind::Claude,
        session_id: None,
        worktree: WorktreeRef {
            path: "/x".into(),
            branch: "nw/x".into(),
            base_branch: "main".into(),
            base_commit: None,
        },
        machine: "m".into(),
        launcher: Launcher::Background,
        herdr_tab: None,
        pid: None,
        last_activity_at: None,
        observed_at: None,
        started_at: None,
        finished_at: None,
        diagnosis_path: "/x.md".into(),
        difit: Difit::default(),
        error: None,
        stopping: None,
        run: None,
        events: vec![],
    }
}

#[test]
fn fingerprints_are_card_specific_and_stable() {
    let a = fingerprint("demo-1", "app", "src/app/a.php", 3);
    assert_eq!(a, fingerprint("demo-1", "app", "src/app/a.php", 3));
    assert!(valid_fingerprint(&a));
    for other in [
        fingerprint("demo-2", "app", "src/app/a.php", 3),
        fingerprint("demo-1", "models", "src/app/a.php", 3),
        fingerprint("demo-1", "app", "src/app/b.php", 3),
        fingerprint("demo-1", "app", "src/app/a.php", 4),
    ] {
        assert_ne!(a, other);
    }
    // The separator keeps fields from running together.
    assert_ne!(
        fingerprint("a", "bc", "d", 1),
        fingerprint("ab", "c", "d", 1)
    );
    for bad in ["", "ABC", &"g".repeat(64), &a[..63], &format!("{a}0")] {
        assert!(!valid_fingerprint(bad), "{bad}");
    }
}

#[test]
fn jobs_are_stored_atomically_privately_and_parsed_strictly() {
    let dir = tempfile::tempdir().unwrap();
    let target = target(dir.path());
    let fp = fingerprint("demo-1", "app", "src/app/a.php", 3);
    save(&target, &sample(&fp)).unwrap();
    assert_eq!(load(&target, &fp).unwrap().unwrap().status, Status::Queued);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(job_path(&target, &fp))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mode = std::fs::metadata(jobs_dir(&target))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let leftovers: Vec<_> = std::fs::read_dir(jobs_dir(&target))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "a temporary file was left behind");

    // Unknown fields, bad enums, wrong names and oversized events are refused, not defaulted.
    let path = job_path(&target, &fp);
    let good: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut bad = good.clone();
    bad["prompt"] = "secret".into();
    std::fs::write(&path, bad.to_string()).unwrap();
    assert!(load(&target, &fp).is_err());
    let mut bad = good.clone();
    bad["status"] = "fixing".into();
    std::fs::write(&path, bad.to_string()).unwrap();
    assert!(load(&target, &fp).is_err());
    let mut bad = good.clone();
    bad["session_id"] = "../../etc".into();
    std::fs::write(&path, bad.to_string()).unwrap();
    assert!(load(&target, &fp).is_err());
    let other = fingerprint("demo-1", "app", "src/app/a.php", 9);
    std::fs::write(job_path(&target, &other), good.to_string()).unwrap();
    assert!(
        load(&target, &other).is_err(),
        "a job stored under another fingerprint"
    );
    assert!(list(&target).is_empty(), "malformed jobs are skipped");
    assert!(load(&target, "../x").unwrap().is_none());

    std::fs::write(&path, good.to_string()).unwrap();
    let (job, value) = mutate(&target, &fp, |job| {
        job.status = Status::Running;
        Ok(7)
    })
    .unwrap();
    assert_eq!((job.status, value), (Status::Running, 7));
    assert_eq!(load(&target, &fp).unwrap().unwrap().status, Status::Running);
    // A failed change writes nothing.
    let error = mutate(&target, &fp, |job| {
        job.status = Status::Failed;
        Err::<(), _>(JobError::Conflict("no".into()))
    })
    .unwrap_err();
    assert_eq!(error.exit_code(), 4);
    assert_eq!(load(&target, &fp).unwrap().unwrap().status, Status::Running);
    let missing = fingerprint("demo-1", "app", "src/app/a.php", 10);
    assert!(matches!(
        mutate(&target, &missing, |_| Ok(())),
        Err(JobError::NotFound(_))
    ));
}

#[test]
fn events_are_scrubbed_bounded_and_capped() {
    let mut job = sample(&fingerprint("demo-1", "app", "a", 1));
    job.event(
        "x",
        &format!(
            "user ops@example.com id 4ab61b31-2fd8-45f7-a07c-faf7dd2ff2a1 {}",
            "y".repeat(400)
        ),
    );
    let summary = &job.events[0].summary;
    assert!(
        summary.contains("<email>") && summary.contains("<id>"),
        "{summary}"
    );
    assert!(summary.chars().count() <= 300);
    for i in 0..100 {
        job.event("x", &i.to_string());
    }
    assert_eq!(job.events.len(), 60);
    assert_eq!(job.events.last().unwrap().summary, "99");
    job.fail("line one\nline two");
    assert_eq!(
        (job.status, job.error.as_deref()),
        (Status::Failed, Some("line one line two"))
    );
    assert!(job.finished_at.is_some());
}

fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn locks_are_exclusive_and_reclaimed_only_from_dead_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locks/runner.lock");
    let lock = FileLock::try_acquire(&path).unwrap().expect("free lock");
    assert!(
        FileLock::try_acquire(&path).unwrap().is_none(),
        "held by a live owner"
    );
    drop(lock);
    assert!(!path.exists());
    let again = FileLock::try_acquire(&path)
        .unwrap()
        .expect("released lock");
    drop(again);

    let stale = serde_json::json!({"pid": dead_pid(), "started_at": now(), "token": "t"});
    std::fs::write(&path, stale.to_string()).unwrap();
    let reclaimed = FileLock::try_acquire(&path).unwrap();
    assert!(reclaimed.is_some(), "a dead owner's lock is reclaimed");
    drop(reclaimed);

    // A live owner (this process) is never displaced, and a foreign token is never removed.
    let live =
        serde_json::json!({"pid": std::process::id(), "started_at": now(), "token": "other"});
    std::fs::write(&path, live.to_string()).unwrap();
    assert!(FileLock::try_acquire(&path).unwrap().is_none());
    assert!(FileLock::acquire(&path, Duration::from_millis(100)).is_err());
    assert!(path.exists());
    // An empty file from an interrupted writer is kept while fresh.
    std::fs::write(&path, "").unwrap();
    assert!(FileLock::try_acquire(&path).unwrap().is_none());
}

#[test]
fn quota_counts_starts_per_utc_day_and_rolls_over() {
    let dir = tempfile::tempdir().unwrap();
    let target = target(dir.path());
    assert_eq!(starts_today(&target), 0);
    assert_eq!(count_start(&target).unwrap(), 1);
    assert_eq!(count_start(&target).unwrap(), 2);
    assert_eq!(starts_today(&target), 2);
    // Yesterday's starts do not count today; old days are dropped.
    let mut days: BTreeMap<String, u64> =
        (1..=10).map(|d| (format!("2026-01-{d:02}"), 99)).collect();
    days.insert(today(), 0);
    std::fs::write(quota_path(&target), serde_json::to_string(&days).unwrap()).unwrap();
    assert_eq!(starts_today(&target), 0);
    assert_eq!(count_start(&target).unwrap(), 1);
    let days: BTreeMap<String, u64> = read_json(&quota_path(&target)).unwrap();
    assert_eq!(days.len(), QUOTA_DAYS_KEPT);
    assert_eq!(days.get(&today()), Some(&1));
}

fn group_leader(script: &str) -> std::process::Child {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().unwrap()
}

#[test]
fn the_os_identity_is_captured_at_the_start() {
    let mut child = group_leader("sleep 30");
    let proc = process::capture(child.id());
    assert_eq!(proc.pid, child.id());
    assert_eq!(proc.pgid, child.id(), "spawned as its own group leader");
    assert!(!proc.os_start.is_empty());
    assert!(
        Path::new(&proc.exe).is_absolute(),
        "an exact path, not a name: {}",
        proc.exe
    );
    assert_eq!(process::check(&proc), process::Check::Same);
    assert!(proc_alive(Some(&proc)));
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(process::check(&proc), process::Check::Gone);
}

#[test]
fn a_reused_or_changed_pid_is_never_signalled() {
    let mut child = group_leader("sleep 30");
    let real = process::capture(child.id());
    let grace = Duration::from_millis(200);
    // Same pid, another start time: a different process holds the pid now.
    let reused = Proc {
        os_start: "1.000000".into(),
        ..real.clone()
    };
    assert_eq!(process::check(&reused), process::Check::Gone);
    assert_eq!(
        process::terminate(&reused, grace),
        process::Terminated::Gone
    );
    // Same start time, another executable or group: cannot be confirmed.
    for changed in [
        Proc {
            exe: format!("{}x", real.exe),
            ..real.clone()
        },
        Proc {
            exe: Path::new(&real.exe)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            ..real.clone()
        },
        Proc {
            pgid: real.pgid + 1,
            ..real.clone()
        },
        // A record written before identities were kept.
        Proc {
            pgid: 0,
            os_start: String::new(),
            ..real.clone()
        },
    ] {
        assert!(
            matches!(process::check(&changed), process::Check::Unverified(_)),
            "{changed:?}"
        );
        assert!(matches!(
            process::terminate(&changed, grace),
            process::Terminated::Refused(_)
        ));
        assert!(!proc_alive(Some(&changed)));
        assert!(proc_maybe_alive(Some(&changed)));
    }
    assert!(process::alive(child.id()), "nothing was signalled");
    assert_eq!(
        process::terminate(&real, grace),
        process::Terminated::Stopped
    );
    child.wait().unwrap();
    assert!(!process::alive(real.pid));
    let dead = Proc {
        pid: dead_pid(),
        ..real.clone()
    };
    assert_eq!(process::terminate(&dead, grace), process::Terminated::Gone);
    assert!(!proc_maybe_alive(Some(&dead)));
}

#[test]
fn a_process_that_does_not_lead_its_group_is_not_signalled() {
    // The test process itself: verifiably alive, but its group is the test runner's.
    let me = process::capture(std::process::id());
    assert_eq!(process::check(&me), process::Check::Same);
    if me.pgid != me.pid {
        assert!(matches!(
            process::terminate(&me, Duration::from_millis(50)),
            process::Terminated::Refused(_)
        ));
    }
}

#[test]
fn an_unreaped_child_is_stopped_through_its_handle() {
    let mut child = group_leader("trap '' TERM; sleep 30");
    std::thread::sleep(Duration::from_millis(100));
    let started = std::time::Instant::now();
    process::terminate_child(&mut child, Duration::from_millis(300));
    assert!(
        child.try_wait().unwrap().is_some(),
        "KILL follows an ignored TERM"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

fn reply(body: &str) -> Vec<String> {
    vec![format!("Here it is.\n{START}\n{body}\n{END}\n")]
}

const GOOD: &str = r#"{"likely_cause":"null plan","code_involved":"src/app/a.php:3","reproduce":"archive a plan","suggested_fix":"load archived plans","blast_radius":"renewals","confidence":"high"}"#;

#[test]
fn diagnoses_need_exactly_one_marked_object_with_six_fields() {
    let diagnosis = parse(&reply(GOOD)).unwrap();
    assert_eq!(diagnosis.confidence, "high");
    // A fenced block and earlier replies are fine; the last marked reply wins.
    let fenced = parse(&[
        format!("{START}\n{GOOD}\n{END}"),
        format!(
            "{START}\n```json\n{}\n```\n{END}",
            GOOD.replace("high", "low")
        ),
    ])
    .unwrap();
    assert_eq!(fenced.confidence, "low");
    let missing = GOOD.replace(r#","confidence":"high""#, "");
    let extra = GOOD.replace(r#""high""#, r#""high","notes":"x""#);
    let empty = GOOD.replace(r#""high""#, r#""  ""#);
    let number = GOOD.replace(r#""high""#, "5");
    let long = GOOD.replace("high", &"h".repeat(4001));
    let big = GOOD
        .replace("null plan", &"a".repeat(3000))
        .replace("archive a plan", &"b".repeat(3000))
        .replace("load archived plans", &"c".repeat(3000))
        .replace("renewals", &"d".repeat(3000))
        .replace("high", &"e".repeat(1000));
    for bad in [
        missing.as_str(),
        extra.as_str(),
        empty.as_str(),
        number.as_str(),
        long.as_str(),
        big.as_str(),
        "not json",
        "[1]",
    ] {
        assert!(parse(&reply(bad)).is_err(), "{}", &bad[..bad.len().min(60)]);
    }
    assert!(parse(&["no markers".into()]).is_err());
    assert!(parse(&[]).is_err());
    assert!(
        parse(&[format!("{START}{GOOD}{END}{START}{GOOD}{END}")]).is_err(),
        "two blocks"
    );
    assert!(
        parse(&[format!("{END}{GOOD}{START}")]).is_err(),
        "markers out of order"
    );
    assert!(parse(&[format!("{START}{GOOD}")]).is_err(), "unterminated");
}

#[test]
fn diagnoses_render_to_fixed_sections_and_back() {
    let job = sample(&fingerprint("demo-1", "app", "a", 1));
    let diagnosis: Diagnosis =
        serde_json::from_str(&GOOD.replace("null plan", "<script>x</script>\\n## Injected"))
            .unwrap();
    let markdown = render(&job, &diagnosis, "2026-10-25T12:00:00Z");
    assert!(markdown.starts_with("# Diagnosis\n"));
    assert!(markdown.contains(&format!("Job: {}", job.fingerprint)));
    for heading in [
        "Likely cause",
        "Code involved",
        "Reproduce",
        "Suggested fix",
        "Blast radius",
        "Confidence",
    ] {
        assert!(markdown.contains(&format!("\n## {heading}\n")), "{heading}");
    }
    let parsed = sections(&markdown);
    assert_eq!(parsed.len(), 6, "an agent cannot add a section: {parsed:?}");
    assert_eq!(parsed[0]["heading"], "Likely cause");
    assert!(
        parsed[0]["text"].as_str().unwrap().contains("<script>"),
        "text is kept for escaping, not interpreted"
    );
    assert_eq!(parsed[5]["text"], "high");
}

#[test]
fn replies_come_only_from_the_assistant_in_every_format() {
    use crate::jobs::agent::{replies, thread_started};
    let claude_result =
        r#"{"type":"result","subtype":"success","session_id":"s","result":"final"}"#;
    assert_eq!(replies(claude_result), ["final"]);
    let transcript = [
        r#"{"type":"user","message":{"role":"user","content":"<<<ISSUE_DIAGNOSIS_JSON>>> in the prompt"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"one"}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}"#,
    ]
    .join("\n");
    assert_eq!(replies(&transcript), ["one", "two"]);
    let codex_exec = [
        r#"{"type":"thread.started","thread_id":"019a-thread"}"#,
        r#"{"type":"item.completed","item":{"type":"reasoning","text":"no"}}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"yes"}}"#,
    ]
    .join("\n");
    assert_eq!(replies(&codex_exec), ["yes"]);
    assert_eq!(
        thread_started(codex_exec.lines().next().unwrap()).as_deref(),
        Some("019a-thread")
    );
    assert_eq!(
        thread_started(r#"{"type":"thread.started","thread_id":"../x"}"#),
        None
    );
    assert_eq!(thread_started(r#"{"type":"turn.started"}"#), None);
    let rollout = [
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"prompt"}]}}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}"#,
        r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"last"}}"#,
    ]
    .join("\n");
    assert_eq!(replies(&rollout), ["answer", "last"]);
}

#[test]
fn launch_profiles_are_read_only_without_mcp() {
    use crate::jobs::agent::{background_args, interactive_args};
    let tree = Path::new("/w");
    let claude = background_args(AgentKind::Claude, tree, "sid", false, "go");
    let joined = claude.join(" ");
    assert_eq!(&claude[..2], ["--session-id", "sid"]);
    assert!(joined.contains("--tools Read,Grep,Glob --permission-mode dontAsk --setting-sources  --strict-mcp-config --mcp-config {\"mcpServers\":{}}"), "{joined}");
    assert!(joined.contains("-p --output-format json"));
    assert_eq!(claude.last().unwrap(), "go");
    assert!(
        !joined.contains("--no-session-persistence")
            && !joined.contains("Bash")
            && !joined.contains("Edit")
    );
    assert_eq!(
        &background_args(AgentKind::Claude, tree, "sid", true, "go")[..2],
        ["--resume", "sid"]
    );
    let codex = background_args(AgentKind::Codex, tree, "", false, "go").join(" ");
    assert!(codex.starts_with("exec --cd /w --sandbox read-only -c approval_policy=\"never\" -c mcp_servers={} --json"), "{codex}");
    let resume = background_args(AgentKind::Codex, tree, "tid", true, "go").join(" ");
    assert!(resume.starts_with("exec resume tid -c approval_policy=\"never\" -c mcp_servers={} -c sandbox_mode=\"read-only\" --json"), "{resume}");
    let herdr_claude = interactive_args(AgentKind::Claude, Some("sid")).join(" ");
    assert!(herdr_claude
        .starts_with("--session-id sid --tools Read,Grep,Glob --permission-mode dontAsk"));
    assert!(herdr_claude.contains("--append-system-prompt"));
    assert_eq!(
        interactive_args(AgentKind::Codex, None).join(" "),
        "--sandbox read-only --ask-for-approval never -c mcp_servers={}"
    );
}

#[test]
fn test_overrides_need_test_mode_and_ids_are_safe() {
    // Outside test mode the environment cannot name an agent executable.
    if !settings::test_mode() {
        let settings = settings::Settings::from_config(None);
        assert!(!settings.test_mode);
    }
    let uuid = settings::uuid_v4();
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "4");
    assert!(settings::safe_token(&uuid));
    assert_ne!(uuid, settings::uuid_v4());
    for bad in ["", "-x", "a b", "a/b", "a'b", &"a".repeat(129)] {
        assert!(!settings::safe_token(bad), "{bad}");
    }
    assert!(!settings::machine().is_empty());
}

#[test]
fn a_step_boundary_needs_every_tool_call_answered() {
    use super::agent::step_boundary;
    let lines = |v: &[serde_json::Value]| {
        v.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let call = serde_json::json!({"type": "assistant", "message": {"content": [
        {"type": "text", "text": "Reading."}, {"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]}});
    let result = serde_json::json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}});
    let reply = serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Done."}]}});
    let prompt = serde_json::json!({"type": "user", "message": {"content": "go on"}});
    assert!(!step_boundary(""), "nothing seen yet");
    assert!(
        !step_boundary(&lines(std::slice::from_ref(&call))),
        "a tool call in flight"
    );
    assert!(
        step_boundary(&lines(&[call.clone(), result.clone()])),
        "the tool result is in"
    );
    assert!(step_boundary(&lines(&[
        call.clone(),
        result.clone(),
        reply.clone()
    ])));
    assert!(step_boundary(&lines(std::slice::from_ref(&reply))));
    assert!(
        !step_boundary(&lines(&[reply.clone(), prompt])),
        "a new prompt is being worked on"
    );

    let item = |payload: serde_json::Value| serde_json::json!({"type": "response_item", "payload": payload});
    let fcall =
        item(serde_json::json!({"type": "function_call", "call_id": "c1", "name": "shell"}));
    let output = item(serde_json::json!({"type": "function_call_output", "call_id": "c1"}));
    let said = item(serde_json::json!({"type": "message", "role": "assistant", "content": []}));
    let thinking = item(serde_json::json!({"type": "reasoning"}));
    let done = serde_json::json!({"type": "event_msg", "payload": {"type": "task_complete"}});
    assert!(!step_boundary(&lines(&[said.clone(), fcall.clone()])));
    assert!(step_boundary(&lines(&[fcall.clone(), output.clone()])));
    assert!(
        !step_boundary(&lines(&[fcall.clone(), output.clone(), thinking])),
        "still reasoning"
    );
    assert!(step_boundary(&lines(&[
        fcall.clone(),
        output.clone(),
        said.clone()
    ])));
    assert!(step_boundary(&lines(&[fcall, output, done])));
    let other = item(serde_json::json!({"type": "custom_tool_call", "call_id": "c2"}));
    assert!(
        !step_boundary(&lines(&[said, other])),
        "any kind of call counts"
    );
}

/// Replies copied from the real herdr CLI (paths and titles anonymised).
#[test]
fn real_herdr_replies_parse() {
    use super::herdr::Herdr;
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("herdr");
    let tab = r#"{"id":"cli:tab:create","result":{"root_pane":{"pane_id":"w7:p94","tab_id":"w7:t4E","workspace_id":"w7"},"tab":{"agent_status":"unknown","focused":false,"label":"issue-0123abcd","number":142,"pane_count":1,"tab_id":"w7:t4E","workspace_id":"w7"},"type":"tab_created"}}"#;
    let agent = r#"{"id":"cli:agent:get","result":{"agent":{"agent":"codex","agent_session":{"agent":"codex","kind":"id","source":"herdr:codex","value":"01a17432-0cae-7ad1-b12f-1dd15451e004"},"agent_status":"done","completion_seq":344,"cwd":"/work/project","focused":false,"foreground_cwd":"/work/project","interactive_ready":true,"name":"issue-0123abcd","pane_id":"w7:p94","revision":4,"state_change_seq":344,"tab_id":"w7:t4E","terminal_id":"term_65ebf608fa0ab4","terminal_title":"Investigate | project","terminal_title_stripped":"Investigate | project","workspace_id":"w7"},"type":"agent_info"}}"#;
    let gone = r#"{"error":{"code":"pane_not_found","message":"pane not found"},"id":"cli:pane:process_info"}"#;
    let info = r#"{"id":"cli:pane:process_info","result":{"process_info":{"foreground_process_group_id":36858,"foreground_processes":[{"argv":["codex","resume","01a17432-0cae-7ad1-b12f-1dd15451e004"],"argv0":"codex","cmdline":"codex resume 01a17432-0cae-7ad1-b12f-1dd15451e004","cwd":"/work/project","name":"codex","pid":36858}],"pane_id":"w7:p94","shell_pid":36526},"type":"pane_process_info"}}"#;
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  'tab create') echo '{tab}' ;;\n  'agent get') echo '{agent}' ;;\n  'pane process-info') [ \"$4\" = w7:p94 ] && echo '{info}' && exit 0; echo '{gone}' >&2; exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let argv = vec![fake.display().to_string()];
    let herdr = Herdr { argv: &argv };
    let created = herdr
        .create_tab("w7", "/work/project", "issue-0123abcd")
        .unwrap();
    assert_eq!(
        (created.tab_id.as_str(), created.pane_id.as_str()),
        ("w7:t4E", "w7:p94")
    );
    let (state, session) = herdr.get("issue-0123abcd").unwrap();
    assert_eq!(state, "done", "the state is result.agent.agent_status");
    assert_eq!(
        session.as_deref(),
        Some("01a17432-0cae-7ad1-b12f-1dd15451e004"),
        "the session id is result.agent.agent_session.value"
    );
    assert!(herdr.pane_exists("w7:p94"));
    assert!(
        !herdr.pane_exists("w7:pZZ"),
        "a missing pane exits 1 with an error object"
    );
}

#[test]
fn a_claude_transcript_is_found_in_a_shortened_hashed_bucket() {
    use super::agent::{claude_bucket, transcript};
    let home = tempfile::tempdir().unwrap();
    let projects = home.path().join(".claude/projects");
    let worktree = home
        .path()
        .join("signals-worktrees")
        .join("a-very-long-directory-name-".repeat(8))
        .join("45969e104e2c638edd8c94897e1f0a2b3c4d5e6f");
    std::fs::create_dir_all(&worktree).unwrap();
    let session = "9cda1e4e-6f93-4cd8-9e96-842a5a04f54e";
    let file = format!("{session}.jsonl");
    let full = claude_bucket(&worktree.canonicalize().unwrap());
    assert!(
        full.len() > 200,
        "the bucket name is long enough to be shortened"
    );
    let put = |bucket: &str| {
        let dir = projects.join(bucket);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&file), "{}\n").unwrap();
        dir.join(&file)
    };
    let find = || transcript(AgentKind::Claude, home.path(), &worktree, session);

    // Only another session elsewhere: nothing to find.
    std::fs::create_dir_all(projects.join("-other-project")).unwrap();
    std::fs::write(projects.join("-other-project/another-session.jsonl"), "").unwrap();
    assert_eq!(find(), None);

    // Claude's shortened bucket: the first 150 characters and a hash.
    let hashed = put(&format!("{}-gzw5x5", &full[..150]));
    assert_eq!(find(), Some(hashed.clone()));

    // A stray copy elsewhere loses to the bucket that matches the worktree.
    put("-elsewhere");
    assert_eq!(find(), Some(hashed.clone()));

    // With no bucket matching the worktree, two copies are ambiguous; one is accepted.
    std::fs::remove_dir_all(hashed.parent().unwrap()).unwrap();
    put("-somewhere-else");
    assert_eq!(
        find(),
        None,
        "two unrelated matches are not guessed between"
    );
    std::fs::remove_dir_all(projects.join("-somewhere-else")).unwrap();
    assert_eq!(find(), Some(projects.join("-elsewhere").join(&file)));
    assert!(
        full.len() > 255,
        "the full name could not even be a directory, hence Claude's shortening"
    );
}

#[test]
fn ended_is_a_terminal_status_named_ended() {
    assert!(Status::Ended.terminal());
    assert_eq!(serde_json::to_value(Status::Ended).unwrap(), "ended");
    let mut job = sample(&"e".repeat(64));
    job.status = Status::Running;
    job.owner = Owner::You;
    job.end("the agent is gone");
    assert_eq!(
        (job.status, job.owner),
        (Status::Ended, Owner::You),
        "the owner is kept"
    );
    assert!(job.finished_at.is_some() && job.error.is_none());
    assert_eq!(job.events.last().unwrap().kind, "ended");
    let old: serde_json::Value = serde_json::to_value(&job).unwrap();
    let mut without = old.as_object().unwrap().clone();
    without.remove("observed_at");
    let parsed: Job = serde_json::from_value(serde_json::Value::Object(without)).unwrap();
    assert_eq!(
        parsed.observed_at, None,
        "job files from before observation still load"
    );
}

#[test]
fn observation_ends_or_diagnoses_only_an_agent_that_is_over() {
    use super::observe::{decide, Presence, Transcript, Verdict};
    let good = r#"{"likely_cause":"a","code_involved":"b","reproduce":"c","suggested_fix":"d","blast_radius":"e","confidence":"low"}"#;
    let diagnosis = parse(&reply(good)).unwrap();
    let seen = |boundary: bool, diagnosis: Option<Result<Diagnosis, String>>| Transcript {
        modified: std::time::SystemTime::now(),
        boundary,
        diagnosis,
    };
    let finished = seen(true, Some(Ok(diagnosis.clone())));
    let replied = seen(true, None);
    let working = seen(false, Some(Ok(diagnosis.clone())));
    let broken = seen(
        true,
        Some(Err("the diagnosis field reproduce is empty".into())),
    );
    let idle_end = Duration::from_secs(600);
    let short = Some(Duration::from_secs(30));
    let long = Some(Duration::from_secs(720));
    let gone = Presence::Gone("Herdr no longer has agent issue-1 and its tab is gone".into());
    let idle = Presence::Alive { settled: true };
    let live = Presence::Alive { settled: false };

    match decide(&gone, None, short, idle_end) {
        Verdict::Ended(how) => assert!(
            how.contains("its tab is gone") && how.contains("no diagnosis"),
            "{how}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(decide(&gone, Some(&finished), short, idle_end), Verdict::Diagnosed(d, _) if d == diagnosis)
    );
    assert!(
        matches!(
            decide(&gone, Some(&working), short, idle_end),
            Verdict::Diagnosed(..)
        ),
        "a gone agent has said all it will"
    );
    match decide(&gone, Some(&broken), short, idle_end) {
        Verdict::Ended(how) => assert!(how.contains("not valid"), "{how}"),
        other => panic!("{other:?}"),
    }

    assert!(matches!(
        decide(&idle, Some(&finished), short, idle_end),
        Verdict::Diagnosed(..)
    ));
    assert!(
        matches!(
            decide(&idle, Some(&working), long, idle_end),
            Verdict::Running
        ),
        "a tool call is still open"
    );
    assert!(
        matches!(
            decide(&idle, Some(&replied), short, idle_end),
            Verdict::Running
        ),
        "idle, but not for long"
    );
    match decide(&idle, Some(&replied), long, idle_end) {
        Verdict::Ended(how) => assert!(
            how.contains("idle in Herdr") && how.contains("no activity for 12 min"),
            "{how}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(decide(&idle, None, long, idle_end), Verdict::Running),
        "no transcript shows it finished"
    );

    for transcript in [None, Some(&finished), Some(&replied)] {
        assert!(
            matches!(decide(&live, transcript, long, idle_end), Verdict::Running),
            "a live process is left alone"
        );
    }
    assert!(matches!(
        decide(
            &Presence::Unknown("herdr is down".into()),
            Some(&finished),
            long,
            idle_end
        ),
        Verdict::Running
    ));
    assert!(
        matches!(decide(&Presence::Unverified("pid reused".into()), Some(&finished), long, idle_end), Verdict::Detach(why) if why.contains("nothing was signalled"))
    );

    assert!(matches!(
        decide(&Presence::NoAgent, Some(&finished), short, idle_end),
        Verdict::Diagnosed(..)
    ));
    assert!(
        matches!(
            decide(&Presence::NoAgent, None, short, idle_end),
            Verdict::Running
        ),
        "just taken over"
    );
    match decide(&Presence::NoAgent, None, long, idle_end) {
        Verdict::Ended(how) => assert!(how.contains("no agent process runs"), "{how}"),
        other => panic!("{other:?}"),
    }
}

/// Real herdr prints error JSON to stderr, with nothing on stdout, and exits 1.
#[test]
fn herdr_replies_read_error_json_from_stderr() {
    use super::herdr::parse_reply;
    let no_agent = r#"{"error":{"code":"agent_not_found","message":"agent target issue-45969e10 not found"},"id":"cli:agent:get"}"#;
    let (ok, value) = parse_reply(false, "", &format!("{no_agent}\n")).unwrap();
    assert!(!ok);
    assert_eq!(value["error"]["code"], "agent_not_found");
    let gone = r#"{"error":{"code":"pane_not_found","message":"pane not found"},"id":"cli:pane:process_info"}"#;
    assert_eq!(
        parse_reply(false, "\n", gone).unwrap().1["error"]["code"],
        "pane_not_found"
    );
    let stdout = r#"{"error":{"code":"from_stdout"}}"#;
    assert_eq!(
        parse_reply(false, stdout, gone).unwrap().1["error"]["code"],
        "from_stdout",
        "stdout JSON still wins"
    );
    let agent = r#"{"id":"cli:agent:get","result":{"agent":{"agent_status":"idle"}}}"#;
    assert_eq!(
        parse_reply(true, agent, "").unwrap(),
        (true, serde_json::from_str(agent).unwrap())
    );
    let neither = parse_reply(false, "", "connection refused").unwrap_err();
    assert!(neither.contains("failed: connection refused"), "{neither}");
    assert_eq!(
        parse_reply(true, "", r#"{"error":{"code":"x"}}"#).unwrap_err(),
        "printed invalid JSON",
        "a successful call is never read from stderr"
    );
}

#[test]
fn herdr_presence_reads_agent_get_then_the_pane() {
    use super::herdr::{Herdr, Presence};
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("herdr");
    let mode = dir.path().join("mode");
    let agent = r#"{"id":"cli:agent:get","result":{"agent":{"agent":"claude","agent_status":"idle","name":"issue-0123abcd","pane_id":"w7:p94"},"type":"agent_info"}}"#;
    let no_agent = r#"{"error":{"code":"agent_not_found","message":"agent target issue-0123abcd not found"},"id":"cli:agent:get"}"#;
    let gone = r#"{"error":{"code":"pane_not_found","message":"pane not found"},"id":"cli:pane:process_info"}"#;
    let info = |name: &str| {
        format!(
            r#"{{"id":"cli:pane:process_info","result":{{"process_info":{{"foreground_processes":[{{"argv":["{name}"],"argv0":"{name}","name":"{name}","pid":4}}],"pane_id":"w7:p94"}},"type":"pane_process_info"}}}}"#
        )
    };
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1 $2\" >> '{calls}'\nm=$(cat '{mode}')\ncase \"$1 $2:$m\" in\n  'agent get:live') echo '{agent}' ;;\n  'agent get:down') echo 'connection refused' >&2; exit 1 ;;\n  'agent get:'*) echo '{no_agent}' >&2; exit 1 ;;\n  'pane process-info:shell') echo '{shell}' ;;\n  'pane process-info:claude') echo '{claude}' ;;\n  'pane process-info:'*) echo '{gone}' >&2; exit 1 ;;\nesac\n",
            calls = dir.path().join("calls").display(),
            mode = mode.display(),
            shell = info("zsh"),
            claude = info("claude"),
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let argv = vec![fake.display().to_string()];
    let herdr = Herdr { argv: &argv };
    let look = |m: &str| {
        std::fs::write(&mode, m).unwrap();
        herdr.presence("issue-0123abcd", "w7:p94", AgentKind::Claude)
    };
    assert_eq!(look("live"), Presence::Present("idle".into()));
    assert!(matches!(look("down"), Presence::Unknown(why) if why.contains("connection refused")));
    assert!(matches!(look("tabgone"), Presence::Gone(how) if how.contains("its tab is gone")));
    assert!(matches!(look("shell"), Presence::Gone(how) if how.contains("no longer runs claude")));
    assert!(
        matches!(look("claude"), Presence::Unknown(_)),
        "the pane still runs the agent"
    );
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    assert!(
        calls
            .lines()
            .all(|l| l == "agent get" || l == "pane process-info"),
        "presence only reads: {calls}"
    );
}
