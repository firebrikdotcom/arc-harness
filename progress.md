# Audit RAM repair handoff

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
