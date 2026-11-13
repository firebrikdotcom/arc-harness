# Audit RAM repair handoff

## Turn context capture — 2026-11-13

Contract: `tasks/turn-context-capture.json` (run `20261113T181625Z-12323`). Integration branch: `feature/turn-context-capture` (from master `e0fd3d7`).

Wire contract (decided): setting `telemetry.workflow_context` (bool, default false, needs `workflow`); event `workflow.context_recorded` with `prompt_id`, `entry_id`, `entry_index`, `role`, `kind`, `part_index`, `part_count`, `text` (≤2000 bytes), `entry_bytes`, `truncated`, optional `sha256` and token counts (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`). Entry cap 16000 bytes; turn cap 500 entries.

Done (worktrees and task branches removed after fast-forward merge):
- [x] docs — `830e90f`.
- [x] hook — `79b958d`; `python3 -m unittest tests.test_workflow_audit tests.test_unified_config tests.test_hook_preferences`: 63 OK.
- [x] service — `cbef86e`; `cargo test --manifest-path services/harness-audit/Cargo.toml`: 126 passed.
- [ ] integrate — `scripts/verify.sh --project .`, independent review, then PR.

Decisions by workers: `record()` is positional-only (wire fact `kind`); unread log slices over 4 MB keep only the newest 4 MB; Codex logs carry no token counts; the page reads at most 300 chunks per turn; turns without a stored prompt show in their own panel (10 most recent).

Follow-ups: API recording proxy for hidden system instructions and tool definitions (non-goal here); Cursor `stop` path does not run capture; the raw `/events` explorer lists context events with their text.

## Jev provider diagnostics — 2026-11-11 (v0.2.10)

- The TypeSafe router (vault `agent-skills/typesafe-routing`, live through `~/.agents/skills`) now raises `ProviderRejectedError` carrying `http_status` and `provider_error_code`. The harness kept only the exception type: 58 of the last seven days' advice calls were recorded as a bare `BadInputError`.
- `context_advice.provider_failure()` now keeps the type plus an HTTP integer (100–599) and a pattern-checked code (`[A-Za-z0-9_.:-]{1,64}`), never exception text. It is used by advice records, decision rows/telemetry, task-route reasons (`provider_error:<Type>:http_<n>:<code>`) and audit checkpoint events.
- Evidence: `test_context_advice` (new rejection regression), `test_decision_checkpoints`, `test_audit_emit`, `test_jev_moments` and `test_failure_followup` pass. A full `scripts/verify.sh` in a fresh worktree (own empty `.harness-db`, clean env) passed everything except one flaky Python 3.14 `ResourceWarning` in `test_failure_followup`, which passes 4/4 when run alone with or without the change.
- **Open gate:** the build gate and pre-commit verify do not pass in the developer checkout. Fixtures (`auto-init.sh`, `harness-cli.sh`, `review.sh`, `verify-evidence.sh`, `test_task_routing` history tests) write into the checkout's own `.harness-db` and assume it is empty. A paused fixture run becomes `runs/current` and cascades. These failures happen identically without this change. The owner approved committing and pushing with this gate open; the commit used `--no-verify`.
- Residue archived (moved, not deleted) to `~/.local/share/harness/archived-test-residue/2026-11-11/`: the stray `/tmp` target `tmp-e9671acd2448` (registered by fixtures since 2026-10-30) and a fixture-created `runs/current` + paused run. Later verify runs created another fixture `runs/current` (`20261111T200854Z-1435853`); it was left in place.
- **Follow-ups:** isolate harness test fixtures from the developer checkout's database (`HARNESS_DB_ROOT` per fixture). Fix the board ticker framing tests (`test_board_agents.BoardAgentsDom.test_ticker_*`): they have failed in GitHub CI since `39483ee` and block updater activation of any newer master. The vault's local `master` (`1a442630`) and its remote (`f4e9a8bf`) both carry the router fix and have diverged.

## Audit race regression diagnosis — 2026-11-06

- On preserved master `03e1a5c`, temporary dispatch diagnostics reproduced `conflict-race: [503, 503]`: both real Arc append attempts returned `AppendFailed / DatabaseError: database is locked`; this was not a successful append followed by a publish error. Evidence: `/tmp/audit-race-diagnostic.log`. Temporary diagnostic logging was removed.
- Corrected the test's assumption that overlapping SQLite writers always produce a 201: when neither fact committed, require both responses to be retryable 503 and require a subsequent sequential retry to return 201; otherwise require an acknowledged durable success. Validate each successful response against its durable payload, reject false conflicts for identical data, and query the authoritative source for exactly one immutable fact after matching retries and conflicting requests. Production ingestion behavior remains unchanged.
- Verification: 20 consecutive scoped regression passes (`/tmp/audit-race-repeated.log`); full audit suite 106 passed plus isolated child, 74.76 seconds (`/tmp/audit-race-full.log`); formatter and `git diff --check` passed. Child RSS: 29,164 KiB for 512 events and 34,304 KiB for 8,192 events.
- Changed only `services/harness-audit/src/routes.rs` test and this handoff; no commit, deployment, branch, worktree, or production database change. Parent still owns integration harness gates, review, commit and CI.

## Current urgent handoff — source write isolation

- Current master; no worker commit, branch, worktree, deployment, or live-database changes. Parent build run: `20261106T055658Z-968313`.
- Preserved the preceding ingestion repair and moved all derived report writes to `<resolved source SQLite file>.dashboard.sqlite`; authoritative reads and bounded source batches use the original database through a separate connection. Sidecar transactions serialize refreshers and commit reports plus their cursor atomically.
- Full audit suite passed: 106 tests plus isolated memory child, 76.32 seconds; `/tmp/audit-sidecar-tests.log`. Child peak RSS: 28,040 KiB at 512 facts and 33,380 KiB at 8,192 facts.
- Tests prove a real Arc CommandBus can append while the projection writer transaction is held, with duplicate/conflict behavior and eventual exact totals; independent stores serialize refresh, process termination rolls back partial reports/cursor, restart resumes correctly, and sidecar deletion rebuilds without altering original legacy tables.
- Next: parent reviews the combined diff, runs required full integration/build/review gates, then verifies the actual service ingestion while historical catch-up is active; live acceptance and CI remain pending.


- **Goal:** Fix the harness audit service's excessive RAM use with durable, incremental projections and bounded report reads, preserving existing audit events, dashboards, filters, time zones and settings. Work directly on `master` in `the repository root`; the user requested a commit and explicitly asked the current agent to stop spawning agents.
- **State:** Current master HEAD `d0810be`; the disk projection implementation is uncommitted in `services/harness-audit/`, including new `src/reports/session.rs` and `src/reports/orchestration.rs`. No commit, push or deployment has been performed by the editing worker. Preserve unrelated `.playwright-mcp/`, `tasks/delegation-worker-review.json`, `tasks/pstack-integration-plan.md`, and `progress-prior-audit-handoff.md`.
- **Evidence:** The unchanged live service previously measured about 5 GiB RSS. The new isolated regression measured about 26 MiB peak at 512 history events and 31 MiB at 8,192 events (12 MiB of revision text); this is test-process evidence, not a deployed-service measurement. Final scoped suite results are recorded below.
- **Implemented:** Disk-backed task/todo heads and source histories; deduplicated prompt chunks; identity-cached event annotations with dependency invalidation; streamed summaries/signal grouping; bounded task/todo/prompt/timeline/flow readers; paged orchestration and workbench groups with global metrics; SQLite disk temporary storage and 8 MiB connection caches. Existing source paging, chart timezone handling, and vendored Arc startup optimization remain intact.
- **Remaining gates:** Parent integration verification, validation of the two independent review corrections, required review approval, and requested commit. No live-service update or memory acceptance has been performed. Catch-up still scans affected-session order-dependent summaries and signal boundaries, so elapsed time remains history-dependent despite bounded retained event state; unchanged annotations and signal rows are reused.
- **Next:** Review the uncommitted audit diff, run the managed harness verification/build/review gates, obtain the required independent review and approval, then commit. The first independent review found task/todo tie ordering and terminal-page next-link defects; both are corrected with passing regressions. Preserve the machine database, existing secrets, and unrelated work; do not claim a deployed memory reduction from the isolated regression.
- **Harness:** Active build run is `20261106T043419Z-3976791`; use `~/.local/bin/harness` and `~/.local/bin/harness-runtime exec` for harness operations. Parent handles integration gates. No `.harness-handoff` file was found.

## Historical allocation-only milestone (superseded by disk projection below)

- At this earlier milestone, work proceeded directly on master without a branch or worktree; later implementation and independent review used delegated agents. Active harness run: `20261106T043419Z-3976791`.
- Removed simultaneous owned copies of all four timeline orderings, the full prompt-body lookup clone, and cloning of histories discarded by directory compaction. These reduce transient allocation; they do not make the reducer bounded.
- All three allocation changes passed all 97 audit tests; `git diff --check` also passed. The running unchanged service still measured 5,113,344 KiB RSS, so no deployed memory improvement is claimed.
- At that milestone, disk-backed reduction, bounded readers, memory measurements and review remained pending; the implementation and local measurements are now complete as documented below.

## Disk projection implementation (locally verified)

- Working on current master without a worktree or commit; only audit service and this handoff are being changed.
- Replaced full-session `SessionReducer` refresh with disk-backed entity heads and streaming timeline projection; task/todo histories stay in individual source facts, prompt chunks deduplicate by identity, and orchestration heads/aggregate metrics use bounded SQL reads.
- The directory equivalence regression now exercises the production SQL search and compares rendered results against the original reducer; the former in-memory concatenated search assumption was removed.
- Final scoped verification: `cargo test --manifest-path services/harness-audit/Cargo.toml --locked -q -- --nocapture` passed all 101 tests in 57.86 seconds; the isolated child also passed. Peak RSS was 25,772 KiB at 512 events and 30,736 KiB at 8,192 events. Log: `/tmp/audit-repair-tests-final.log`. `cargo fmt --manifest-path services/harness-audit/Cargo.toml -- --check` and `git diff --check` passed.

### Parent handoff

- Added regression evidence for unchanged annotation reuse (one appended event produces one annotation insert), exact global metrics across 205 paged cohorts/workers, complete tail revisions and filtered timeline pages after restart, and reducer equivalence through replacements, late prompt chunks and paired tool returns.
- UI/API pagination is explicit; task/todo revision history remains reachable through the raw timeline. API workflow output is now compact, paged session metadata; see the service README for its pagination contract.
- No commit was made, as requested by the parent; no production process or machine database was modified. Parent still owns full harness checks, independent review, approval, commit and any later deployment decision.

## Independent review corrections (locally verified)

- Task pages, task sequence numbers and todo pages now share the first chronological fact's exact `(occurred_at, recorded_at_us, position)` ordering, including late arrivals and ties; scalar first-fact lookups avoid expanding entity reads across their entire histories.
- Workflow API emits `rel=next` only when `offset + returned length < total`; regression covers empty, full, partial-final and beyond-end pages.
- `cargo fmt --manifest-path services/harness-audit/Cargo.toml -- --check` passed; `cargo test --manifest-path services/harness-audit/Cargo.toml --locked -q` passed all 103 tests in 57.75 seconds; `git diff --check` passed. Log: `/tmp/audit-review-fixes-tests.log`.
- An initial regression compile error was corrected; an intermediate join-based ordering query was stopped after slowing the memory regression and replaced with scalar lookups before the passing final suite.
- No commit, push, deployment, machine-database or secret modification; parent retains integration gates, review approval and commit responsibility.

## Integration acceptance

- Full managed harness verification passed after both review fixes; build gate reran and passed all 103 audit tests. Release build passed. Independent final review approved with no blocking findings. The configured review gate is off and build completion closed run `20261106T043419Z-3976791`.
- Live service updated after a private SQLite backup; health returned HTTP 200. Initial RSS was 64,800 KiB versus the prior 5,113,344 KiB. Catch-up is still running, so final catch-up and loaded-page measurements remain pending.
- Final commit/push and hosted CI acceptance remain pending; do not equate the passing local gates with final delivery.

## Delivery record

- Implementation committed and pushed on master as `ceae2e4`; CI run `42169675951` passed: https://github.com/firebrikdotcom/arc-harness/actions/runs/42169675951. All 103 audit tests, full local harness verification, release build, and independent final review passed.
- Deployed to the existing local audit service after SQLite backup. Health, workbench, Workflow, Orchestration, and paged workflow API returned HTTP 200; sampled page responses took 4–59 ms.
- Live historical rebuild reached 110,336 of 357,986 source events at 241,268 KiB RSS versus the previous 5,113,344 KiB. This is an in-progress measurement; complete historical catch-up remains background work, not a claim of full replay completion. Two transient database-lock retries recovered with the durable checkpoint intact.
- Database backup is retained privately under the machine state directory; source events, settings and secrets were preserved. No branch or worktree was created.

## Live ingestion repair (run 20261106T054717Z-806979)

- Live acceptance exposed request-triggered full dashboard catch-up before ID checks and after writes; concurrent POSTs competed with background replay and failed before recording new source events.
- Removed projection refresh from ingestion. Duplicate/conflict checks now read the authoritative `events` row through `(aggregate_type, aggregate_id, sequence)`; successful bus dispatch acknowledges durable persistence immediately. Failed dispatch rechecks committed source for raced/publish-failed writes; transient storage errors return retryable HTTP 503, while command validation remains HTTP 400.
- Inspected the resolved Arc SQLite 0.8.11 implementation: `load_stream` delegates to `load_stream_from`, filtering aggregate type and ID with the existing composite index. The aggregate records one event per ID; it does not replay global history per POST. No vendor changes were necessary.
- Added real SQLite command-bus regression covering a 600-event pending backlog, prompt ingest, unprojected duplicate/conflict checks, concurrent identical/conflicting IDs with retry semantics, dedupe under an active writer, an unrelated malformed source stream, and eventual exact projection totals.
- Editing worker made no commit, push, deployment, or live-database change; parent owns final integration gates and actual-service acceptance. Scoped full-suite result follows after completion.
- Verification: `cargo test --manifest-path services/harness-audit/Cargo.toml --locked -q` passed all 104 tests in 78.70 seconds (log `/tmp/audit-ingest-tests.log`); `cargo fmt --manifest-path services/harness-audit/Cargo.toml -- --check` and `git diff --check` passed.

## Source/projection isolation implementation

- Sidecar path is determined from SQLite's resolved source filename; the source event table, snapshots, legacy derived tables and credentials are preserved. Source SELECTs finish before reduction and never join the projection write transaction; no ATTACH is used. Each batch remains bounded at 256 source rows.
- Recovery and backups are documented in `services/harness-audit/README.md`: back up authoritative SQLite state safely; with the service stopped, discard only the sidecar and its WAL/SHM companions for replay, including after restoring an older source snapshot. Reports are eventually consistent during replay; ingestion acknowledges source durability.
- Adapted test fixtures to insert source facts into the source connection and derived fixtures into the sidecar, preserving reducer, pagination, restart and memory assertions. Existing production files were not opened or changed by this worker.
- Verification: `cargo test --manifest-path services/harness-audit/Cargo.toml --locked -q -- --nocapture` passed 106 tests (log `/tmp/audit-sidecar-tests.log`); `cargo fmt --manifest-path services/harness-audit/Cargo.toml -- --check` and `git diff --check` also passed.

## Final live ingestion acceptance

- Separate-sidecar implementation passed 106 audit tests, full managed harness verification, release build and independent review. Deployed after verification without changing source events, secrets or settings.
- Live ingestion during pending catch-up: create HTTP 201 in 11 ms, identical retry HTTP 200 in 1 ms, conflicting retry HTTP 409; authoritative source contained exactly one event for the validation ID. Source advanced to 358,351 while the sidecar cursor was 15,360, confirming collection no longer waits for historical projection.
- Health/workbench/Workflow/Orchestration returned HTTP 200 in 1–35 ms. Initial new-process RSS was 44,092 KiB; the earlier deployed bounded reducer plateaued near 250,000 KiB during large-history catch-up compared with the original 5,113,344 KiB. Historical sidecar catch-up continues normally; full replay completion is not claimed.
- Prior same-database lock failures are superseded by this independently reviewed separation and physical-lock regression. Pending final commit and final-head CI are the only delivery gates left.

---

# Session focus handoff

Current task: session focus for the architecture board (contract `tasks/session-focus.json`, run 20261109T134106Z-15293, review phase).
Prototype: https://claude.ai/artifact/Xp9UHPuLd3Ebo9ZVBgS9id (plain HTML; the target look and behaviour).

## Decisions

- The main folder (TARGET_KIND=repo) owns the one map. A worktree links to it through `git rev-parse --git-common-dir` and reuses its board; it falls back to its own board only when the main folder has none (or is a bare repo).
- Signals per card: changed (branch diff since merge-base with the default branch, plus uncommitted), planned (this session's run `task.json` deliverables), read (files the session's tool calls touched, from the workflow audit). Read is the weakest signal.
- Neighbors off by default; neighbors come only from changed or planned cards.
- Owner only: loopback requests are the machine owner; tailnet requests are matched with `tailscale whois --json <peer ip>` against the owner login recorded at session start. Anyone else gets 404. No sharing in v1.
- A finished session keeps a read-only view until its worktree is gone.
- `scripts/hooks/` is guard-protected, so the session link is printed by `harness brief` (which auto-init already runs) instead of a hook edit.

## Shared data format (both workers code to this)

`board.py list --json` rows gain: `kind` ("repo"|"worktree"|"dir"), `parent` (main folder target id or null), `branch` (string or null), and for worktrees `task` = {`title`, `phase`, `run_id`, `live`, `last_active`} or null, and `owner` = {`user`, `login`}. A worktree's parent row is always present in the list.

`board.py session --target PATH --session-id ID` creates (idempotent) `targets/<id>/board/sessions/<key>.json` = {`key` (secrets.token_urlsafe(24)), `session_id`, `target`, `root`, `owner`: {`user`, `login`|null}, `started_at`} and prints `{"key":..., "url": "<HARNESS_AUDIT_URL or http://127.0.0.1:18080>/projects/<target>/sessions/<key>"}`.

`board.py focus --target ID --session KEY` prints:
```json
{"target": "...", "board_target": "...", "parent": "..."|null,
 "session": {"key": "...", "owner": {"user": "...", "login": "..."|null}, "branch": "...", "base": "develop",
             "root": "...", "run_id": "...", "phase": "plan|build|review|done", "title": "...", "live": true, "last_active": "..."},
 "signals": {"<card id>": {"edited": ["path"], "scope": ["path"], "touched": ["path"]}},
 "unmapped": ["path"], "summary": "plain sentence"}
```
Unknown key: exit 3 and `{"error": "unknown session"}`.

Audit service routes: `GET /projects/{id}/sessions/{key}` (page) and `GET /projects/{id}/sessions/{key}/focus.json`; both 404 for non-owners. `board.json` for the view comes from `board_target`.

## Tasks

1. [done] Stop race fix + baseline: 5be6649.
2. [done] Data layer: 5640391. Check: `bash tests/board.sh`.
3. [done] Dashboard: 8157360. Check: audit cargo test + `tests/test_board_session.py`.
4. [done] Merge, base-ref fix and auto-init test isolation: 2b04d45; `scripts/verify.sh --project .` passed, build done.
5. [running] Independent review; then user `approved`, `review submit --chat-approved`, `review done`.

## Follow-ups

- Clean up the ~126 per-worktree DollarWise boards once worktrees reuse the main map (not in this task).
- Worktree rows show the board state as NOT MAPPED/READY though they reuse the main map.
- Tailnet access from another device untested (only unit tests).
- Changes reach real sessions only after the published harness picks them up.

## Hook preferences and verification repair

- [done] Confirmed upstream `master` still points to `c0e86b4005da9e9e07543ec7ff128003c52bac8d`.
- [done] Preserved the existing hook-preference UI candidate and restored the current hardened `scripts/role_dispatch.py` behavior while retaining its user comment.
- [done] Fixed `process::alive` for a child reaped between the signal-0 probe and `ps`; added regression coverage for zombie and reaped states.
- [done] Verified the candidate with `/Users/me/.local/bin/harness-runtime exec scripts/verify.sh --project /Users/me/Code/harness-template-fix-harness-verification`: `ran=3, skipped=3, failures=0`.
- [done] Copied the verified `scripts/role_dispatch.py` and `services/harness-signals/src/jobs/process.rs` into this checkout; both match the verified candidate byte-for-byte. Published harness runtime restored; Claude worker tab and task-owned dashboards closed.
- [note] An initial direct, unwrapped verify run wrote run `20261112T202120Z-71134` into the primary `.harness-db` and failed on fixture isolation. The database record was left untouched; the required pinned wrapper run passed.
- [retained] Worktree `/Users/me/Code/harness-template-fix-harness-verification` and branch `fix/harness-verification` remain because their changes are uncommitted and unmerged.
# Workflow session recency — 2026-11-13

- Goal: move sessions with the newest interaction to the top of Workflow and refresh the directory live, preserving filters and pagination.
- Run: `20261113T154723Z-29371`; contract: `tasks/workflow-session-recency.json`.
- Implementation: native worker `session_recency` owns `services/harness-audit/` in sibling worktree `harness-template-session-recency`, branch `fix/workflow-session-recency`; parent owns coordination and this handoff.
- Dispatch recovery: remote dispatch `1ef6ea772acc4a7aaeab1f0fcc491206` had no access to this repository; local dispatch `79357dd2492e409b9fcc8ee4af6e9439` failed snapshot preparation. Its dirty worktree is retained; no application patch came from either attempt. A clean native worktree avoids modifying either failed attempt.
- Discovery: default directory SQL already sorts `last_at` descending; the open directory has no live refresh. Initial `/health` and `/workflow` returned HTTP 200. Browser connection currently fails before opening a tab.
- Implemented: five-second visible-tab refresh of summary, quick-filter counts and session results; applied URL, unsaved form edits and focused/hovered links are preserved.
- Verified: managed harness verification passed (`ran=3 skipped=3 failures=0`); isolated Docker audit suite passed 123 tests; Node refresh suite passed 3 tests; formatting and whitespace passed. Logs: `/tmp/workflow-recency-{verify,audit-tests,build}.log`.
- Delivered locally: release build passed; `local.harness.audit` restarted; health and default/filtered Workflow pages returned HTTP 200 and served the new script and regions. Binary SHA256 `3ad4797b158c0f694ef053e8f820b027adee7da01020a47fe5551d22f7adc9cf`. Browser-tool connection failed before tab creation, so no visual verification is claimed.
- Independent review approved tree `ea5e93becfbdfad4d0e28b3a5772b5d5faf49b77` with zero findings and both criteria met. Findings: `/tmp/workflow-recency-review-findings.json`. `build done` and review packet generation passed; review submission requested the required chat approval for run `20261113T154723Z-29371`.
- Approval: user replied `approved`, then explicitly instructed bypassing the broken approval-capture gate and finishing. Proceed with normal Git commit checks and push to `origin/master`; do not fabricate a harness receipt or change hook settings. The harness review phase remains pending because its native approval capture is disabled. Preserve both dirty worker worktrees until safe cleanup.
- The unchanged contract Cargo command ran through `/tmp/workflow-recency-cargo/bin/cargo` in isolated Docker. The harness refused a post-plan contract amendment; the original contract is preserved. All final required checks passed, including the same full harness mode; browser rendering remains unverified because the browser connection failed.
- Environment: primary checkout owns the launchd audit service at port 18080; tests must use temporary databases, with no secret or production database changes.
- Scope: session directory ordering/refresh only; unrelated services and collection hooks remain outside the change.
