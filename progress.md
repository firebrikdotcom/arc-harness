# Handoff: Task Managers in the Arc service (create/edit, associations, open items, attachable agents)

Written 2026-11-14 (local) for an agent that has not seen the conversation. Earlier versions are in git history.

## Where things stand

All five slices are done, verified, independently reviewed and committed on `master`. The audit service runs the new binary.

| Slice | State |
| --- | --- |
| S1 Python provider driver | Done |
| S2 Arc state (aggregates, store, filters) | Done |
| S3 Agent runtime + `harness tasks attach|agent` | Done |
| S4 Dashboard (routes, templates, route tests) | Done |
| S5 Docs (`docs/task-managers.md`, `AGENTS.md` map) | Done |
| Verify, independent review, fixes | Done |

Evidence: `scripts/verify.sh --project .` exit 0 and `harness build done` with `create`, `existing`, `service` passing (run `20261115T045339Z-1784166`); `make -C services/harness-audit test` 181 Rust tests. The plan is `tasks/task-managers/handoff.md`, the contract `tasks/task-managers/task.json`, the reference `docs/task-managers.md`.

## Independent review

A fresh-context reviewer returned one blocker and five should-fix findings; all six are fixed with tests:

- `agent done` refuses when the default branch cannot be resolved (`origin/HEAD` unset) and never pushes the default branch.
- `--dir` (CLI, form or stored association) must belong to the allowlisted repository (same git common directory); checked at every start.
- `gh pr create` gets `--repo OWNER/NAME` derived from `origin`; a non-GitHub origin is refused.
- `done` can be retried: an existing open pull request of the branch is recorded instead of created again.
- The edit form sends the title and due date only when changed; "Clear the due date" works with a prefilled date.
- Claude's headless first turn runs with `--strict-mcp-config --tools Read,Grep,Glob --permission-mode plan`.

Also changed: `check` emits the `ssh` key; a failed agent start leaves nothing behind; the agent runtime ignores an inherited `HARNESS_DB_ROOT`; `KillMode=process` in `ops/harness-audit.service`; `tests/harness-hook.sh` no longer reads the machine's shared config.

The project has `.harness-db/review-off`, so the review was advisory: no gated review phase ran.

## Left open

Review notes not acted on (see `docs/task-managers.md`, "Known gaps"):

- A fresh agent worktree is based on the checkout's local `HEAD` with no fetch, so unpushed local commits ride along into the pull request.
- `attach --print` takes the agent over and records `attached` even though it only prints.
- `done` on an `attached` agent removes its worktree while the owner's interactive session may still be in it.
- Agent code uniqueness across items is enforced only by the index after the event is committed (a 1 in 16.7 million collision would stall the index refresh).
- The supervisor loop does blocking database and file reads on the async executor every 2 seconds.
- An association's directory is checked at agent start, not when it is saved. Dashboard agent views take the SSH destination from `HARNESS_TASKS_SSH` only.
- The Claude first-turn flags were checked against `claude --help`, not against a live run. `--restricted` was not added (it ignores the owner's settings files; untested).

Harness and environment:

- With the dev override pointing at this checkout, `scripts/harness` re-executes through the managed launcher and drops a test fixture's `HARNESS_DB_ROOT`, so fixture tests write to the real database and fail. Run verify (and commit, for the pre-commit hook) with `HARNESS_RUNTIME_ACTIVE=1` until that is fixed. Run `20261115T032028Z-942930` (paused, target under `/tmp`) is a leftover of that leak; only the owner can abort it.
- `harness-signals` test `jobs::process::tests::a_child_reaped_during_the_check_is_not_alive` failed once in four full runs (a race; unrelated to this work).
- `~/.config/harness/config.json`: `dispatch.timeout_seconds` 7200 and profiles `native_claude_build`, `native_claude_review` were kept.
- The dispatcher's worker worktrees do not work in this repo (its snapshot commit triggers `.githooks/pre-commit`); native subagents edited in place instead.
- `cargo clippy -- -D warnings` fails on lints in `orchestration.rs`, `reports.rs`, `summary.rs`, `ui.rs`, `workflow.rs` that predate this work.

## Do not touch

- Unrelated uncommitted work by someone else: `services/harness-audit/public/styles.css`, `resources/views/audit/partials/turn-context.html`, `src/reports.rs`, `src/ui.rs`, `src/workflow.rs`, `tasks/turn-context-json.json`.
- `scripts/task_managers_runs.py` (the gated pipeline; a contract non-goal path).
