# Handoff: Task Managers (Basecamp) in the Arc audit service

Updated 2026-11-14 (local). Read this first; earlier versions are in git history.

## State

Shipped on `master` in `d2555c3`: create/edit lists and to-dos, full descriptions, list associations (repo + directory), open items with filter presets, attachable agents (`harness tasks agent ...`, `harness tasks attach`), dashboard pages for all of it. Verified (`scripts/verify.sh` exit 0, `make -C services/harness-audit test` 181 tests), independently reviewed, review findings fixed. Nothing is in progress.

Uncommitted follow-up (contract `tasks/task-repositories-git.json`): the Repository menus show the git repository (`OWNER/NAME` or `host/path`, with its path) instead of the config nickname, a configured path that is not a git checkout is not offered, and an association's directory is checked against the repository when saved. Independently reviewed; its two findings are fixed (a plain directory inside another checkout no longer counts as a repository, in `check`, the save check and worktree creation; inherited `GIT_DIR` and kin are dropped from git calls). Left from that review, low: `check` runs up to two 2-second git calls per repository in sequence under a 20-second read timeout; the "separate clone" wording is missed for non-GitHub clones whose origin URLs differ in form; a token placed in an origin URL's path would show in a label.

Where to read:

| Need | File |
| --- | --- |
| Behaviour, commands, exits, routes, security, known gaps | `docs/task-managers.md` |
| Contract (deliverables, constraints, non-goals, acceptance) | `tasks/task-managers/task.json` |
| Original plan and Basecamp prototype | `tasks/task-managers/handoff.md`, `tasks/task-managers/prototype/` |
| Python driver (HTTP, token, cache) | `scripts/task_managers.py`, `scripts/task_managers_write.py` |
| State (event-sourced aggregates, sidecar `<db>.tasks.sqlite` index, no projectors) | `services/harness-audit/src/domain/task_*/`, `src/task_managers/{mod,store,open,cli}.rs` |
| Agent runtime | `src/task_managers/agents/` (tests in `agents/tests.rs`) |
| Dashboard | `src/tasks.rs`, `src/tasks/`, `resources/views/tasks/` (route tests in `src/tasks/tests/manage.rs`) |

## Running service on this machine

- The owner opens the dashboard at `http://100.83.189.74:18080/` (tailnet IP, plain http). Not through a `tailscale serve` domain; do not add a serve route on 18080, it takes over that port on the tailnet IP.
- systemd user unit `harness-audit`, built with `cargo build --release` and run via `scripts/audit-service.sh serve`. `KillMode=process`, so agents survive a service stop.
- The repo template `ops/harness-audit.service` binds `127.0.0.1` (commit `70c5ef9`). This machine overrides it with `~/.config/systemd/user/harness-audit.service.d/listen.conf` (`APP_URL=0.0.0.0`), which survives `scripts/audit-service.sh install`. After any reinstall or restart, check `ss -ltn | grep 18080` shows `0.0.0.0` and the IP URL answers 200.
- The service builds from the working tree: a restart also ships whatever is uncommitted at the time.
- `/tasks` access: `~/.config/harness/task-managers.json` has `owner_login` and lists `100.83.189.74` and the tailnet names in `allowed_hosts`. Only `/tasks` and worktree/session-focus views are owner-gated.

## How to work here

- Harness: `harness plan start`, `contract set tasks/task-managers/task.json` (or a new contract), `plan done`, `build start`, then verify and `build done`. This project has `.harness-db/review-off`: `build done` completes the run, so run an independent review yourself (a fresh-context subagent) and show the findings to the owner.
- Run verify as `HARNESS_RUNTIME_ACTIVE=1 scripts/verify.sh --project .`, and commit with `HARNESS_RUNTIME_ACTIVE=1 git commit ...` (the pre-commit hook runs verify, about 10 minutes). Without it, the active dev override makes `scripts/harness` re-exec through the managed launcher, which drops test fixtures' `HARNESS_DB_ROOT`; fixture tests then write to the real database and fail.
- Fast loops: `sh tests/task-managers-create.sh`, `sh tests/task-managers.sh`, and in `services/harness-audit`: `cargo test --locked task_managers`, `cargo test --locked tasks::`. The `build done` gate runs `make test` with `HARNESS_DB_ROOT` set; reproduce with `HARNESS_DB_ROOT=$PWD/../../.harness-db make test </dev/null`.
- Native subagents editing in place work well; give each disjoint files. The dispatcher's worker worktrees fail here (their snapshot commit triggers the pre-commit verify).
- Commit directly on `master`. No `Co-Authored-By` trailer, no "Generated with" footer.

## Open work, by value

From the review (not acted on; also in `docs/task-managers.md` "Known gaps"):

1. Fresh agent worktrees branch from the checkout's local `HEAD` with no fetch, so unpushed local commits ride into the pull request. Fix: fetch and base on `origin/HEAD`, as runs do.
2. `done` on an `attached` agent removes the worktree while the owner's interactive session may still be in it.
3. (Done in the uncommitted follow-up above.) An association's directory is checked when saved.
4. Dashboard agent views take the SSH destination from `HARNESS_TASKS_SSH` only; call `rt.resolve_ssh()` where `main.rs` builds the runtime.
5. The supervisor loop does blocking database and file reads on the async executor every 2 seconds; move it to the blocking pool.
6. `attach --print` takes the agent over and records `attached` although it only prints.
7. Agent code uniqueness across items is enforced only by the index after the event commits (1 in 16.7 million collision stalls the index refresh).
8. Claude's read-only first turn (`--strict-mcp-config --tools Read,Grep,Glob --permission-mode plan`) was checked against `claude --help`, not a live run. `--restricted` was not added.

Harness issues found on the way (outside this feature):

- Fix the launcher re-exec dropping `HARNESS_DB_ROOT` in tests (see "How to work here"). Run `20261115T032028Z-942930` (paused, target under `/tmp`) is a leftover of it; only the owner may abort it.
- `harness-signals` test `jobs::process::tests::a_child_reaped_during_the_check_is_not_alive` is flaky (failed once in five full runs).
- `cargo clippy -- -D warnings` fails on old lints in `orchestration.rs`, `reports.rs`, `summary.rs`, `ui.rs`, `workflow.rs`.

## Turn context JSON (separate work, shipped)

Contract `tasks/turn-context-json.json`. On the workflow page, JSON in a turn context entry is indented in place (whole text, after a tool name, after a text preamble, on a line among other output, or cut off by the entry cap) and every non-empty object and array is a server-rendered `<details>`, two levels open by default; no JavaScript. `workflow::context_json` builds the `json` string, `workflow::context_display` adds the page-only `json_tokens`, and `partials/turn-context.html` renders them. Verified (`scripts/verify.sh` exit 0, audit suite 184 tests) and checked on the served pages' markup; not looked at in a browser, and not independently reviewed. Known limits: complete values are re-serialised (keys sorted, numbers may change form); a second value on one line and scalar-only arrays among other text stay as written.

## Do not touch

- `scripts/task_managers_runs.py` (the gated run pipeline; a contract non-goal).
- `~/.config/harness/config.json` dispatch changes (`timeout_seconds` 7200, `native_claude_*` profiles) were kept on purpose.
