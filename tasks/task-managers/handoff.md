# Handoff: Task Managers create/edit, associations, open items, attachable agents

Written 2026-11-14 for an agent that has not seen the planning conversation. Nothing is implemented yet. This file is the plan and the context you need to execute it.

## Goal

Extend Task Managers (Basecamp is the only connected provider) so the machine owner can:

1. Create to-do lists inside the available projects, and to-do items inside those lists.
2. Update to-do items.
3. Read an item's full description (today it is cut at 4,000 characters).
4. Associate a to-do list with a repository and, optionally, a directory.
5. See a section of all not-done items across projects, with configurable filters, and act from it.
6. Dispatch an interactive agent on an item and get a short code that attaches to that agent's session.

## Read first

- `AGENTS.md` and `CLAUDE.md` at the repo root: phases, gates, review approval. Follow them.
- `docs/task-managers.md`: the full current behaviour, CLI contract, exit codes, security rules. Every new command must fit this contract.
- `docs/issue-agents.md`: the existing precedent for an agent job with session id, worktree, takeover, handback and resume commands. Milestone 5 copies this pattern.
- `docs/orchestration.md` if `dispatch.enabled` or `typesafe.enforce_decisions` is on in `~/.config/harness/config.json`.

## Current state (verified by reading the code and docs)

| Path | Role |
| --- | --- |
| `scripts/task_managers.py` (749 lines) | CLI entry, config, HTTP, cache, `BasecampReader`, argument parsing (`build_parser`, `dispatch`). |
| `scripts/task_managers_write.py` (145 lines) | `comment` and `complete`; `read_text_file`, `_post`, `forget` (drops the project cache). |
| `scripts/task_managers_runs.py` (876 lines) | The gated plan/build/verify/review pipeline that ends in a draft PR. Headless workers; its `--session-id tm-<run>` is a dispatcher id, not a resumable agent session. |
| `scripts/task_managers_seed.py` | The fenced packet and contract draft. |
| `services/harness-audit/src/tasks.rs` (3,436 lines) | Dashboard bridge under `/tasks`: owner gate, host check, CSRF, shells out to the CLI with fixed argv. |
| `services/harness-audit/resources/views/tasks/` | `sources`, `source`, `project`, `list`, `item`, `runs`, `run` templates. |
| `tests/fake_basecamp.py` | Offline fake provider; already serves GET, POST, PUT, DELETE. |
| `tests/task-managers*.sh` | One script per behaviour; `scripts/verify.sh` runs them all. |
| `tasks/task-managers/prototype/` | Untracked earlier prototype (HTML template, smoke scripts). Reference only. |

Existing reader commands: `check`, `projects`, `lists`, `todos`, `get`, `seed`. Existing writes: `comment`, `complete`. Existing runs: `run start|list|status|approve|reject|cancel|advance`.

Existing list-to-repo mapping lives in the per-machine config file `task-managers.json` under `mappings` (`<provider>:<list id>` to a name in `repos`). The dashboard only uses it to preselect a repo.

## Rules that must keep holding

- Task-manager text is untrusted data. It reaches agents only inside the fence, and never goes into file names, branch names, commit messages or pull requests.
- Free text reaches the CLI through a file path, never an argument. Dashboard form text is written to a `0600` temp file first.
- A write is one request, never repeated after a failure (the single exception is the repeat after a `401` and a successful refresh). After any write, call `forget` for the project.
- Ids are validated before anything runs (item, project, list: 1 to 20 digits).
- Requests go only to the configured provider hosts; redirects are not followed.
- Every `/tasks` route passes the host check, owner gate, `Origin` check and CSRF, in the documented order.
- Only repositories named in `repos` of `task-managers.json` may receive an agent or a run.
- Python modules use the standard library only.
- The gated pipeline in `task_managers_runs.py` stays as it is.

## Milestones

### 1. Create and edit

CLI (new functions in `scripts/task_managers_write.py`, parser entries in `scripts/task_managers.py`):

- `list-create --project ID --name-file F [--description-file F]` returns `{list:{id,name,url}}`.
- `todo-create --list ID --title-file F [--description-file F] [--due YYYY-MM-DD]` returns `{item}`.
- `todo-update --item ID [--title-file F] [--description-file F] [--due YYYY-MM-DD|--clear-due] [--reopen]` returns `{item}`.

Basecamp endpoints, from memory of the Basecamp 3 API and **not verified in this session**; check them against the official API docs before coding:

- Create list: `POST /buckets/<project>/todosets/<todoset>/todolists.json` with `name`, `description`. The to-do set id comes from the project's dock.
- Create item: `POST /buckets/<project>/todolists/<list>/todos.json` with `content`, `description`, `due_on`.
- Update item: `PUT /buckets/<project>/todos/<item>.json`. Basecamp clears any field omitted from the body, so read the item fresh (no cache) and re-send every field that was not changed, including assignees and dates.
- Reopen: `DELETE /buckets/<project>/todos/<item>/completion.json`.

Dashboard: a "new list" form on the project page, a "new to-do" form on the list page, an "edit" form on the item page. New `POST` routes in `tasks.rs` following `comment` and `complete`.

Acceptance: each action sends exactly one write request to the fake provider; an update leaves untouched fields unchanged; a failed write is not repeated; the cache for the project is dropped.

### 2. Full description

- `get --item ID --full`: description bounded at 64 KiB instead of 4,000 characters, still HTML-stripped and fenced in non-JSON output.
- Item page: a "show full description" view.
- The seed packet keeps its current bounds.

Acceptance: a fixture item with a description over 4,000 characters is returned whole with `--full` and cut without it.

### 3. Associate lists with a repo and directory

- `associate --list ID --repo NAME [--dir PATH]`, `unassociate --list ID`, and associations reported by `check` and `lists`.
- Stored in `HARNESS_DB_ROOT/task-managers/associations.json` (`0600`, atomic replace under a lock), not in the config file. Config `mappings` entries remain as defaults; a stored association wins.
- `--repo` must be a name in `repos`.
- `--dir` is optional. When absent, the agent creates its own worktree (milestone 5).
- Dashboard: an association form on the project and list pages.

Acceptance: a repo not in `repos` is refused with exit `2`; an association survives a restart; removing it restores the config default.

### 4. Open items section

- CLI: `open [--project ID] [--list ID] [--assignee NAME|me] [--due-before DATE] [--overdue] [--repo NAME] [--agent none|active] [--preset NAME]` returns `{items:[item], filters, truncated}`.
- Presets: `preset save NAME ...filters`, `preset list`, `preset delete NAME`, `preset default NAME`; stored in the harness database beside associations.
- Dashboard: a section after projects on the source page (and a `/tasks/{provider}/open` route) with the filters as a form, the default preset applied on load, and per-row "dispatch agent" and "open" actions.
- This walks every list of every project. Use the existing cache, bound the number of items returned, and report `truncated`.

Acceptance: filters combine with AND; done items never appear; the default preset is applied when no filter is given.

### 5. Interactive agent with attach code

New module `scripts/task_managers_agents.py`, modelled on the issue-agent jobs in `services/harness-signals/` and `docs/issue-agents.md`.

- `agent start --item ID [--repo NAME] [--dir PATH] [--agent claude|codex]`: repo and directory default to the list's association. Creates a worktree and branch when no directory is given, writes the fenced packet, starts one claude or codex session, and records agent kind, session id, worktree, branch, pid and start time. Returns `{agent_id, code, state}`.
- The attach code is short and unguessable enough for one machine (for example `tm-` plus 6 hex), unique among live agents.
- `attach CODE`: stops or releases background supervision first (two writers on one session corrupt it), then execs `claude --resume <session>` or `codex resume <session> --cd <worktree>` in the worktree. `attach CODE --print` prints the command, plus the SSH variant when an SSH destination is configured.
- `agent list`, `agent status --code C`, `agent stop --code C`, `agent handback --code C`.
- `agent done --code C`: commits the changed files, pushes the agent's branch, opens a draft pull request with `gh pr create --draft`, then removes the worktree. Same naming rules as the pipeline: no item text in branch, commit or PR.
- Refuse a second live agent for the same item, as `run start` does with `run_active`.
- Dashboard: item page and an agents page show the code with a copy button, state, worktree and branch, plus stop, hand back and done actions.

Acceptance: against `tests/fake_agent.py` and a fake `gh`, start records a session id and code; `attach --print` gives the exact resume command; `done` opens one draft PR and the worktree is gone; a second start for the same item is refused.

### 6. Docs and tests

- Update `docs/task-managers.md`: commands, exits, routes, state files, security notes. Add the new files to the map in `AGENTS.md` if a new module is added.
- Extend `tests/fake_basecamp.py`; add `tests/task-managers-create.sh`, `tests/task-managers-associate.sh`, `tests/task-managers-open.sh`, `tests/task-managers-agents.sh`. All offline.
- Rust tests in `services/harness-audit/src/tasks.rs` for each new route: gate order, id validation, argv handed to the CLI.

## Order and ownership

1. Milestones 1 and 2 touch the same Python files; one engineer, in sequence.
2. Milestone 3 next (5 and 4 depend on associations).
3. Milestones 4 and 5 can run in parallel: 4 owns the reader and presets, 5 owns the new agents module. Both add parser entries to `scripts/task_managers.py`, so integrate those serially.
4. Dashboard work in `tasks.rs` and the templates is one owner, after each CLI contract is fixed. `tasks.rs` is a single large file; do not split it between parallel workers.
5. Docs last, against the integrated behaviour.

The user asked that each task get the specialists it deserves rather than a fixed roster. Suggested: a Python engineer for 1 to 4, an architect pass on the session lifecycle before milestone 5, a security review of associations (paths, allowlist) and of attach and cleanup, a Rust/dashboard engineer, a technical writer, and the independent fresh-context reviewer the harness requires.

## Delivery

- Work in a new git worktree on a feature branch off `master`. Do not work in the main checkout.
- When verification and the independent review pass, open a pull request to this repository and remove the worktree.
- The main checkout has uncommitted changes that are not part of this work: `services/harness-audit/public/styles.css`, `resources/views/audit/partials/turn-context.html`, `src/reports.rs`, `src/ui.rs`, `src/workflow.rs`, and untracked `tasks/turn-context-json.json`. Leave them alone.
- No `Co-Authored-By` trailers in commits and no "Generated with" footer in the pull request (user's global rule).
- Open your own harness run with `scripts/harness plan start`, set a contract from `tasks/task.example.json`, and go through build, `scripts/verify.sh`, and review. The guard only accepts calls in the run's project, so open the run for the worktree you work in.

## Decisions the user made

- The attachable agent is a new interactive agent, separate from the gated pipeline, which stays unchanged.
- Specialists are chosen per task.
- Delivery is a feature branch in a worktree, a pull request when done, and the worktree cleaned up.
- Lists must be associable with a repo and optionally a directory.
- An open-items section with configurable filters comes after projects.

## Open questions (asked, not yet answered)

These are my readings. Confirm with the user before building the affected milestone.

1. **Worktree, PR, cleanup.** The user gave this as the delivery answer. I applied it both to how this work is delivered and to what a dispatched agent does when done (milestone 5, `agent done`). They may have meant only one.
2. **`--dir`.** I read it as an existing working directory for the agent, with a fresh worktree created when absent. The user's words: the directory is "not necessarily required since if not specified the agent will create one to be able to work on it". The other reading is a subdirectory inside the repo that scopes the agent.
3. **"In a new session after projects".** I read "session" as "section": a new dashboard section after the projects list.

## Planning run

A harness run was opened in the planning session (`20261115T014738Z-141919`, plan phase, no contract). It belongs to that session; do not reuse it. It expires after a day idle.
