# Audit RAM repair handoff

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
