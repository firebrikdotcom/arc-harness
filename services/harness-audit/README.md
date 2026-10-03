# Harness audit service

This is the versioned Arc event-sourced audit sink for the harness. It stores
JEV routing, resolved outcomes, and agent/token measurements as immutable
`AuditEventRecorded` events, then projects them into an `audit_events_view`
read model for summaries and review.

The service listens on all IPv4 interfaces (`0.0.0.0:18080`); use localhost or this machine's address to open it:

```sh
cp .env.example .env
cargo run -- migrate
cargo run -- serve
curl http://127.0.0.1:18080/health
```

The harness client treats the service as best-effort telemetry. If it is down,
the agent task continues and the local TypeSafe logs remain authoritative for
the route call itself.

Browser pages (Arc UI host, same read model as the API):

- `GET /` — audit workbench: a daily accuracy line (labeled checkpoint decisions by UTC day of the checkpoint), headline totals, checkpoint families, task-entry routes, recent events; `?include_fixtures=true` shows regression-test fixtures
- `GET /events[?type=jev.checkpoint&limit=200]` — newest-first event log with a type filter
- `GET /public/styles.css` — the dashboard stylesheet, embedded at build time

Endpoints:

- `GET /health`
- `POST /api/audit/events`
- `GET /api/audit/events?limit=1000`
- `GET /api/audit/summary`
- `GET /api/audit/summary/checkpoints[?include_fixtures=true]`
- `GET /api/audit/summary/routes[?include_fixtures=true]`

## Grouped summaries

`/api/audit/summary` only counts event types, outcomes and token totals. The
grouped endpoints answer "how is each gate doing" without pulling
`/api/audit/events` and analysing it by hand.

`/api/audit/summary/checkpoints` groups `jev.checkpoint` events by `family`,
`question_version`, `policy_version`, `model_requested` and `model_returned`.
Each group reports:

- `count`, and `unlabeled` (`count - labeled`; an `unknown` outcome is unlabeled).
- `comparable` and `agreement_rate`: checkpoints with both a `baseline_action`
  and a `recommendation`, and the share where they are equal (`null` when none).
- `labeled`, `correct`, `accuracy` (`correct / labeled`, `null` when nothing is
  labeled) and `outcomes`, the distribution of joined outcomes. Outcomes come
  from `jev.checkpoint_outcome` events joined on `call_id`; when a call has
  several labels the latest one wins, and labels without a checkpoint are ignored.
- `recommendations`: distribution of the recommended choice.
- `top_pairs`: the five most common `baseline->recommendation` pairs.
- `median_confidence`, `median_latency_ms` (`null` when no sample exists).
- `tokens`: `total` of `jev_total_tokens` (or input plus output) and the number
  of `samples` behind it. Unmeasured events add nothing.

`/api/audit/summary/routes` groups `jev.route` events by `source`,
`routing_mode`, `model_requested` and `model_returned`, with the same count,
outcome (joined from `jev.outcome`), accuracy, latency and token fields, and
`recommendations` holding the `observed_recommendation` distribution.

Regression-test fixtures (`model_requested` or `model_returned` equal to
`fixture`) are excluded by default and reported in `excluded_fixture_events`;
pass `include_fixtures=true` to keep them.

```sh
curl http://127.0.0.1:18080/api/audit/summary/checkpoints
curl 'http://127.0.0.1:18080/api/audit/summary/routes?include_fixtures=true'
```

## Cross-machine

Every machine that runs the service has its own SQLite database, so summaries
only cover events emitted on that machine. To pool them, point a second host's
`HARNESS_AUDIT_URL` at one service over the tailnet, or copy the database file.
There is no sync between databases.

## Browser workbench

The service registers an Arc UI host (`src/ui.rs`) with the scaffold's admin
layout, `components/ui.html` macros and `public/styles.css`, all embedded with
`include_str!` so no working directory or asset build is needed. `GET /` renders
the grouped checkpoint and route summaries from `summary.rs` plus the newest
events; `GET /events` lists the log. Both pages read `audit_events_view`
through the same `ReadModelStore` as the JSON endpoints. There is no sign-in:
the installed service binds to all IPv4 interfaces, and the framework's authenticated navigation is
replaced by the host's own links. The accuracy chart at the top is inline
SVG computed server-side by `summary::daily_checkpoint_accuracy` and
`ui::accuracy_chart`, so it needs no JavaScript; its daily counts sum to the
headline labeled and correct totals, and it pools families and question
versions, so use the checkpoint table to compare cohorts. Open http://127.0.0.1:18080/ after
`scripts/audit-service.sh serve` (a restart rebuilds the binary).


## Jev and Workflow collection

The workbench now has two independent **Audit collection** switches:

- **Jev** records the existing routes, checkpoints, outcome labels, completion measurements, and token events. It does not control evaluation, routing, or shadow mode.
- **Workflow** records correlated session/task lifecycles, accepted harness phase transitions, tool return/failure metadata, verification and review results, curated task descriptions, decisions and options, and explicit final outcomes.

Save the switches on `/`. They persist across restarts and apply to both local collectors and API ingestion. Disabling collection preserves stored history and discards undelivered records of that type. `/workflow` offers a session selector, task summaries, phase states, task filters, and a timeline ordered by source time. Missing metadata is shown as unspecified; a returned tool call does not imply verification success. Historical sessions are not reconstructed.

`GET /api/audit/settings` returns `{ "jev": true, "workflow": true }`.
`PUT /api/audit/settings` replaces those two booleans (JSON, same-origin browser requests only).
`GET /api/audit/workflow` returns grouped sessions/tasks/events.
Both categories default to enabled on service startup. Switches apply to collection only.

Configuration lives at `../../.harness-db/audit-settings.json`; use `HARNESS_AUDIT_SETTINGS` to select another path, with the same value in the service and every collector. `HARNESS_AUDIT_ENABLED=0` remains a process-level hard opt-out; `=1` opts in when no settings file exists. Once the service has created its settings file, it controls collection for local clients even if the legacy variable is unset. Defaults and API ingestion fail closed on a malformed settings file.

Telemetry is first saved in a SQLite outbox (`HARNESS_AUDIT_OUTBOX`, default `../../.harness-db/audit-outbox.sqlite`). The running service retries delivery every two seconds; every explicit emission also attempts a bounded drain. Hook collection only queues locally. Repeated HTTP delivery uses the same event ID and is idempotent; conflicting data with the same ID is rejected. On outages, events remain queued. `scripts/harness workflow flush` manually retries delivery. A failed delivery does not fail a phase, hook, or verification gate. The service worker requires `python3` and the companion harness scripts on this machine. Shared switches and this worker are local-machine features; remote collectors must use the same configured settings through their own deployment configuration.

### Recording tasks and decisions

Run `scripts/install-hooks.sh` to install additive lifecycle hooks. Existing unrelated hooks are preserved and backed up. The collector listens for SessionStart, UserPromptSubmit, PostToolUse and SessionEnd, plus PostToolUseFailure where supported. It uses the native session ID; child harness commands use `CODEX_THREAD_ID`, `CLAUDE_SESSION_ID`, or `HARNESS_SESSION_ID`. On runtimes providing `CLAUDE_ENV_FILE`, the start hook exports that binding. If a direct command has no native ID, phase/sensor telemetry is skipped instead of guessing a session.

The enabled SessionStart hook provides the recording commands as session context, so new sessions can supply curated task summaries, meaningful decisions, and explicit outcomes as they work. A generic task is created when the session is first observed. Supply curated metadata to name it; use `--new` for a subsequent task and `--parent-task-id` for a dependency. Final outcomes require an explicit command. Ending a session does not automatically mark its work complete.

```sh
scripts/harness workflow task --name "Improve audit dashboard" --description "Add session task timelines and collection switches"
scripts/harness workflow decision --description "Choose verification scope" --option focused --option full --selected full
scripts/harness workflow outcome --status completed --description "Required checks and live rendering passed"
```

Outside a native session, add `--session-id ACTUAL_SESSION_ID`. Workflow context lives in `HARNESS_WORKFLOW_STATE` (default `../../.harness-db/workflow-state.sqlite`). Prompt bodies are collected only through the separately enabled prompt switch. Transcript contents, tool inputs, command text, and tool output are never copied into Workflow events. Only curated explicit descriptions/options are stored, so do not supply secrets in those fields. Session/tool delivery is observation, not a complete transcript audit.

Offline regression coverage: `sh tests/workflow-audit.sh` from the harness root and `cargo test` from this service directory.

Prompt collection is controlled by **Store user prompts** on the dashboard (`workflow_prompts`, default false). It requires Workflow to be enabled. Future submitted prompts appear on the Workflow page, with the first captured prompt labeled as the initial goal. Existing sessions are not backfilled. Turning capture off retains stored history. Task summaries continue to use `harness workflow task`.

The dashboard uses consistent panel spacing, responsive collection controls and tables, and session cards with status/phase summaries. Expand **Task details** for task IDs, parent relationships, and full outcomes. Session metadata keeps the full ID, runtime, model, and working-directory path visible.

Visual direction follows the original Arc scaffold: warm paper surfaces, ink outlines, amber accents, square components, and monospace labels. Layout refinements preserve those tokens and component treatments.

Workflow uses two-step navigation. `/workflow` lists sessions in a searchable, sortable, paginated table. Filters cover agent, model, session status, full working-directory path, current task phase, captured-prompt availability, and inclusive UTC activity dates. Search matches all words across recorded metadata, tasks, outcomes, decisions, and captured prompts. Open a description for session details; **Back to sessions** retains filters, sorting and page. Existing `?session_id=` links remain supported. Metadata refreshes do not advance the session activity date.


### Timeline analysis

Session details explain each event and provide search, type and result filters. Session-wide counts distinguish tool success (explicit exit 0), failures, returns with unknown results, process handles and starts without observed returns. Checks and explicit task outcomes are separate evidence. Expand Event evidence for native tool call ID, source and recorded timestamps, harness run, exit code and paired tool round-trip duration. PreToolUse hooks capture starts; matching native call IDs preserve the original task even if another task begins before the return. Missing IDs/timing stay unavailable. Process handles do not prove final process completion; later tool/check events provide that evidence. Inputs and outputs are not stored. Existing history is enriched from its saved facts without inventing missing details. Reinstall hooks and restart native sessions to enable new start hooks.


### Browser timezone

All displayed instants use the browser's current IANA timezone and locale. A session-only `audit_timezone` cookie shares that timezone with server-rendered calendar grouping and activity-date filters. An initial page load or timezone change refreshes the current URL once; filter/navigation state is retained. Daily accuracy and inclusive activity dates use the client's calendar, including historical daylight-saving offsets. Tool durations remain elapsed milliseconds. Stored event timestamps and API responses retain their original UTC/epoch representation. Without cookies, instants still localize but calendar grouping is hidden and date filters are disabled instead of presenting a different timezone. The service reuses the existing chrono and chrono-tz dependencies for calendar conversion.


## Audited todo enforcement

Install or update native hooks with `scripts/install-hooks.sh` on each machine, then restart existing native sessions to load them. The additive `scripts/workflow_gate.py` hook requires a structured plan, reconfirmation or revision for every prompt, and one active todo before covered local execution. `scripts/workflow_todos.py` keeps policy state in the local Workflow database even when audit collection is disabled. Collection settings control visibility and storage of emitted events, not this policy.

```sh
scripts/harness workflow task --name "Repair session navigation" --description "Fix navigation and verify the served dashboard"
scripts/harness workflow todo plan --items '[{"id":"repair","description":"Repair navigation","criterion":"Navigation regression passes"},{"id":"checks","description":"Verify and review","criterion":"Fresh full verification and review pass"}]' --reason "Complete initial plan"
scripts/harness workflow todo update --id repair --status in_progress --reason "Start repair"
scripts/harness workflow todo update --id repair --status completed --evidence "Navigation regression passes" --reason "Repair verified"
scripts/harness workflow todo update --id checks --status in_progress --reason "Run required checks"
scripts/verify.sh --project /absolute/path/to/project
scripts/review.sh --project /absolute/path/to/project
scripts/harness workflow todo update --id checks --status completed --evidence "Full verification and review passed" --reason "Checks complete"
scripts/harness workflow outcome --status completed --description "Navigation repaired and reviewed"
```

Add `--session-id ACTUAL_SESSION_ID` to Workflow commands outside a native session. `todo show` displays IDs, criteria, state and confirmation. For a new prompt, use `todo confirm --reason SUMMARY` or submit the complete revised list with `todo plan`. Keep IDs for retained items; omissions are audited removals with a reason. Changed criteria reset the item and require fresh checks. Completion requires evidence on each completed item, all required items resolved, and passing verify/review records after the latest execution and scope revision. Switching to a new task cannot silently discard unfinished required todos. Questions that require no execution use `todo exempt --reason SUMMARY`; unfinished execution plans must first be resolved or explicitly reported blocked.

The task card displays criteria, status, evidence, and revision history. Select a todo to filter its timeline, including correlated tools and checks. Historical tasks have no fabricated todo list.

The gate uses UserPromptSubmit, PreToolUse and Stop hooks; policy errors return exit 2 with guidance. Telemetry outages do not bypass it. This enforces declared plans on covered native tools, not semantic completeness or a security sandbox: specialized tools outside native hook coverage, hook timeouts, and runtime configuration can bypass enforcement. Codex hosted WebSearch has no PreToolUse; a returned process handle is not tool completion. Stop may request another continuation rather than cancel a session. Existing phase/denylist gates and permissions remain authoritative. Review phase completion may precede deployment todos; explicit completed outcomes and Stop enforce the full task criteria.
