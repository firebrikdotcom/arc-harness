# Progress

## Current Goal

Build the `harness` CLI slice on branch `lotharthesavior/feat-cli`: harness-root discovery, session budgets with pause/continue, and plan/build/review phase gates. Complete; awaiting review.

## Decisions

- Keep this file current during each task.
- Record only decisions that affect future implementation, verification, setup, or architecture.
- Move completed task details out of this file when they stop being useful for the next run.
- 2026-08-30: Strategy review decided three directions. (1) Make detection safe first: non-mutating checks, shebang-aware syntax parsing, per-check timeouts. (2) An explicit per-project manifest becomes authoritative for verification commands and required categories; command detection demotes to a labelled fallback. Scope correction 2026-08-30: this applies to command discovery only. Codebase comprehension is a separate, first-class capability that must grow, not shrink. (3) `scripts/verify.sh` emits a machine-readable run record (commands, exit codes, git HEAD, timestamp) that completion claims must cite instead of asserting success.
- 2026-08-30: Added a knowledge layer as a fourth harness pillar beside guides, sensors, and state: pattern scanning, graph indexing, generated code memory, and ingestion of a per-project non-versioned `knowledge/` folder. It feeds planning and review, not verification.
- 2026-08-30: Verified there is no `graphify` skill installed on this machine. The existing implementation of graph indexing and code memory is the `codebase-memory` skill over its MCP server. `knowledge/` currently has no reader; it is referenced only by this file.
- 2026-08-30: Knowledge layer shaped. Indexing runs at bootstrap from `scripts/init.sh` and refreshes on later runs. `knowledge/` carries its own `CLAUDE.md` and `AGENTS.md` that are consulted every run for hooks, automated verifications, extra detail, and direction. Graph trust is freshness-gated: authoritative while the index is verified current, demoted to a lead when stale.
- 2026-08-30: Verified `codebase-memory-mcp` (v0.10.8) exposes a `cli <tool>` mode, so `scripts/init.sh` can index without an MCP client. `index_repository`, `index_status`, and `detect_changes` cover bootstrap indexing, refresh, and the freshness gate.
- 2026-08-30: Verified the current index for this repo holds 63 nodes and 61 edges and excludes `docs/` and `scripts/`, which is where all harness behavior lives. Index scope must be revisited before the graph is useful here.
- 2026-08-30: Open conflict to settle. `knowledge/` is non-versioned yet always consulted and able to declare hooks and automated verifications. That is an unreviewed instruction-and-execution channel (fault line G1) and it reopens the local/CI divergence that committing the manifest was meant to close. Precedence, hook trust, and index scope are unresolved.
- 2026-08-30: Entrypoint decided. Install a real `harness` CLI on PATH that locates the harness root itself, instead of requiring `--project` from the harness directory.
- 2026-08-30: Project workflow conventions (git model, branch and commit format, PR shape, task tool) are detected from the target repo first; the user is asked only about what detection cannot settle. Verified against DollarWise-Prototype: branch promotion chain, conventional-commit format with Linear ticket scope, task tool, existing `.github` templates, and commit size baseline were all derivable from git history and `.github/` with no interview.
- 2026-08-30: Placement rule for conventions. Team facts live in committed target-repo files; machine and personal facts live in non-versioned `knowledge/`. This also resolves the open precedence question by jurisdiction rather than ranking: committed sources own shared workflow and verification, `knowledge/` owns personal context and direction.
- 2026-08-30: Convention detection must stay project-specific. The DollarWise-Prototype findings are one project's standards, not a template. The harness ships probes and no assumed defaults, and low confidence is a valid outcome that produces a question rather than a guess.
- 2026-08-30: Learned project knowledge is stored in a `knowledge/` subfolder inside each target project, hidden through that repo's `.git/info/exclude` rather than `.gitignore`, so the harness leaves no trace in tracked files. Verified the mechanism on a scratch repo. Note: `.git/info/exclude` is not copied by clone, so `harness init` must write the entry idempotently per machine and account for linked worktrees sharing the file.
- 2026-08-30: The convention interview runs at `harness init`, after detection, covering only unresolved gaps, once per project.
- 2026-08-30: Accepted consequence. Conventions learned by the harness are per-machine, so contributors can diverge where a repo declares nothing. Mitigated by treating `knowledge/` as a derived cache while the repo remains the source of truth.
- 2026-08-30: The convention interview is bounded and splits by project age. An existing repo relies on detection and asks only about unresolved gaps, often nothing. A greenfield project has no evidence to read, so it gets the full interview.
- 2026-08-30: `harness init` must check for the mattpocock/skills collection and use `grill-with-docs` for the interview. Verified the collection is in Claude Code's official marketplace (`claude plugins install mattpocock-skills`), that `grill-with-docs` exists, and that nothing from it is installed on this machine.
- 2026-08-30: Open tension recorded. `grill-with-docs` builds on the `grilling` primitive, which interviews intensively until all branches resolve. That conflicts with the requirement for a short interview, so its use must be capped or restricted to greenfield.
- 2026-08-31: Installed `mattpocock-skills@claude-plugins-official` v1.2.3 at user scope. Verified 35 skills on disk including `engineering/grill-with-docs`.
- 2026-08-31: Interview scope cap decided. Seed the grill only with topics detection could not settle; no open exploration. Reading the skill showed `grilling` already requires the agent to find facts itself rather than ask, and asks in batched rounds with recommended answers, so the cap is a seeding discipline rather than a conflict. Note `grill-with-docs` sets `disable-model-invocation: true`, so `harness init` cannot trigger it autonomously and must instruct the user to run it.
- 2026-08-31: Hook trust decided. Hooks declared in `knowledge/` always run. Accepted risk, raised and reaffirmed: `knowledge/` lives inside each project, so a cloned repository could arrive carrying one that executes without review.
- 2026-08-31: Index scope decided. The indexer reads the whole tracked tree including `docs/` and `scripts/`; `knowledge/` is skipped because `.git/info/exclude` hides it from the indexer as well as from git. `index_repository --persistence` is permanently forbidden because it is the only option that writes into the target project (`.codebase-memory/graph.db.zst`).
- 2026-08-31: Confirmed the retrieval model. The graph is retrieval-augmented in shape but not a vector RAG: a full-mode index of this repo produced 277 structural edges (DEFINES, CALLS, USAGE, IMPORTS, CONTAINS_*) against 9 SEMANTICALLY_RELATED similarity edges. Retrieval returns named symbols with exact file and line coordinates, so a stale index yields confidently wrong coordinates rather than merely irrelevant text. This is the justification for freshness-gated trust.
- 2026-08-30: Accepted residual risk for decision 3. Citing a run record is not enforced, so a completion claim can still reference a stale or invented id. This makes false claims detectable, not impossible.
- 2026-09-01: First control-plane slice is schema plus validator only. No allowlist, no executor, and no Cursor tool block. Agents can still skip validation; a rejected proposal is detectable.
- 2026-09-02: The `harness` CLI counts a step for every phase command and for every explicit `harness step`. The CLI cannot observe the model's tool calls, so step counting is a reported, agent-visible rule rather than an enforced measurement.
- 2026-09-02: Token budget stays `unknown` unless the agent reports counts with `harness step --tokens N`. The CLI does not attempt to count tokens.
- 2026-09-02: `harness continue` extends the tripped budget by one more window (`cap + extensions * cap`, with a grant of 1 when the base cap is 0) rather than clearing the counter, so total consumption stays visible across evaluations.
- 2026-09-02: Run state is stored twice on purpose: `state` as `KEY=VALUE` for shell reads without a JSON runtime, and `run.json` as the machine-readable snapshot. Evaluation notes live only in the JSON pause records, so arbitrary text never enters the shell state file.
- 2026-09-02: `tests/harness-cli.sh` follows the existing convention of a standalone script invoked directly. It is not wired into `scripts/verify.sh`, which is owned by another worktree's scope.
- 2026-09-01: Grill confirmed. Full keep/leave list is in `harness-review.md`. Overrides: no executor; dry-run dropped; detection stays (no manifest); Make still wins; init still runs project setup immediately; `knowledge/` hooks still always run. Permit is allow-unless-denied with a harness default denylist that a project file replaces. Session budgets and phase gates wait for a future `harness` CLI.

## Active Plan

Phase: Build and review complete on `lotharthesavior/feat-cli`. Not merged, not pushed.

Goal: add the `harness` CLI from the confirmed keep/build list in `harness-review.md` — harness-root discovery, session budgets, and plan/build/review phase ownership.

Non-goals: action executor, dry-run, command manifest, Make plus native checks together, `init.sh` preview, `knowledge/` hook gating. Those belong to other worktrees.

Harness root: this repository checkout

Target project root: same as the harness root.

Acceptance criteria:

- `scripts/harness` finds the harness root from the current directory or an ancestor by looking for `AGENTS.md` and `scripts/verify.sh`.
- Commands `plan|build|review start|done`, `status`, and `continue` exist, plus `step` so the step budget can be counted.
- Run state lives under `.harness-db/runs/<id>/` with budget counters and the current phase.
- Default caps are `steps=20`, `time_min=15`, `loops=1`; the token cap is optional and recorded as `unknown`.
- Reaching a cap writes a pause record and exits non-zero; `continue` requires an evaluation note.
- `build start` is refused until `plan done`; `review start` is refused until `build done`.
- Tests cover phase order and pause/continue.
- `docs/setup.md` documents the CLI.

Implementation plan:

1. Add `scripts/harness`.
2. Add `tests/harness-cli.sh`.
3. Document the CLI in `docs/setup.md` and list the new files in `README.md`.
4. Write `QA-REVIEW.md` with copy-paste commands and real output.

Verification plan:

- Run `sh tests/harness-cli.sh` and `sh tests/action-schema.sh`.
- Run `scripts/verify.sh` and record the pre-existing ShellCheck failures.

Known risks:

- `scripts/verify.sh` fails on ShellCheck warnings that predate this branch, in files this feature does not touch.
- Step and token counts are self-reported by the agent; the CLI records and gates but cannot measure them.
- The earlier action-validator risks still stand: agents can skip the validator, schema and script rules can drift, and the validator needs `python3` or `node`.

Goal: create a progressive harness strategy guide, render it for a 10.3-inch grayscale screen, and transfer it to the NoteAir.

Acceptance criteria:

- The first section is a one-page complete overview.
- The second and third sections progressively deepen the explanation.
- Flowcharts and component descriptions are included.
- Markdown is stored under locally excluded `knowledge/` state.
- The PDF is visually verified and submitted to the NoteAir.

Goal: make verification fail when a project-declared required category runs no checks.

Acceptance criteria:

- Projects can declare required categories through configuration or an environment override.
- A required category passes only when at least one check in that category runs successfully.
- Missing required categories produce a non-zero exit status and an actionable summary.
- Regression tests cover successful, missing, and overridden requirements.
- Harness verification and review results are recorded.

Implementation plan:

1. Add required-category configuration and validation.
2. Configure this harness to require lint and test checks.
3. Add regression coverage and CI tooling.
4. Run verification, review, and inspect the diff.

Verification plan:

- Run `scripts/verify.sh`.
- Run `scripts/review.sh`.
- Inspect `git diff`.

Known risks:

- Local verification now intentionally fails until ShellCheck is installed.
- Undeclared categories remain optional for backward compatibility.

## Completed Steps

- Build: 2026-09-03 critical items 6, 7, 8, 9, 27, 39, 40 and reopened 14: denylist (`schemas/denylist.default`, `scripts/permit.sh`) applied by `scripts/action.sh` and by the hook to every real tool call; `scripts/init.sh` previews and confirms project-owned commands (`--yes` in CI); `scripts/knowledge-trust.sh` human approval gate enforced by the hook; `scripts/verify.sh` runs only check-style formatters; `scripts/review.sh` prints the full patch and continues after a failing verify; budget defaults raised to 200 steps and 120 minutes; a time-budget continue restarts the clock. New tests: permit, knowledge-trust, review, init-preview, verify-no-rewrite; hook, CLI, and action tests extended.
- Cleanup: 2026-09-03 pruned `todo.md` to 19 open items: merged duplicates (progress, guides, tooling, review, timeouts) and dropped six nitpicks, listed at the end of the file.
- Cleanup: 2026-09-03 removed done items and empty sections from `todo.md` (numbers stay stable); moved `QA-REVIEW.md` into ignored `.harness-db/reviews/`; ignored `.claude/settings.local.json`; removed a machine-local path from this file.
- Build: 2026-09-03 items 3, 4, 5, 10, 12 of `todo.md`: verify/review run records under `.harness-db/records/` gate `build done` and `review done`; `harness abort` and a per-run continue cap; the hook refuses `continue`/`abort` from the agent and records one step per allowed tool call; ShellCheck warnings fixed; `scripts/verify.sh` runs `tests/*.sh` on the harness root. Live check: the hook blocked this session when the stale 2026-09-02 run tripped its time budget, and a human aborted it from a terminal.
- Build: 2026-09-03 item 2 of `todo.md`: added `Makefile` target `install-guides` and `scripts/install-guides.sh`, which add or refresh a marked Harness Phases block in `AGENTS.md` and `CLAUDE.md`; ran it on this repo; added `tests/install-guides.sh`; documented in `docs/setup.md` and `README.md`.
- Build: 2026-09-03 item 1 of `todo.md`: added `.claude/settings.json` PreToolUse hook and `scripts/hooks/require-phase.sh` that block Write/Edit/Bash unless a harness phase is active; added `tests/harness-hook.sh`; documented in `docs/setup.md` and `README.md`.
- Review: 2026-09-03 harness review found 19 concerns; recorded with why and fix in `todo.md` under "Review Findings 2026-09-03".
- Planning: read `AGENTS.md`, `CLAUDE.md`, `docs/conventions.md`, `docs/setup.md`, `harness-review.md`, `todo.md`, and the existing scripts and tests before writing the CLI.
- Build: added `scripts/harness` with harness-root discovery, `KEY=VALUE` plus JSON run state under `.harness-db/runs/<id>/`, session budgets, pause records, and plan/build/review gates.
- Build: added `tests/harness-cli.sh` covering root discovery, every phase-order refusal, the step/time/loop/token budgets, pause records, refusal while paused, and continue with and without an evaluation note.
- Build: documented the CLI, its budgets, its exit codes, and its state layout in `docs/setup.md`, and listed the new files in `README.md`.
- Review: ran both test scripts, replayed the blocked-build and pause/continue sessions by hand, and confirmed the two new files are ShellCheck-clean.
- Review: wrote `QA-REVIEW.md` with copy-paste commands, expected output, a manual checklist, and the real verification paste.
- Review: 2026-09-01 grill confirmed. Keep/leave recorded in `harness-review.md`. No build started.
- Build: added `schemas/action.schema.json`, `scripts/action.sh validate`, and `tests/action-schema.sh`.
- Build: documented propose-then-validate in setup, architecture, agent guides, README, and `harness-review.md`.
- Review: wrote `harness-review.md` covering what is right, missing, and needs update against the four harness controls.
- Planning: selected a three-step progressive document structure and a 10.3-inch grayscale e-ink layout.
- Build: created `knowledge/harness-strategy-guide.md` with overview, operational, and implementation-depth sections plus flowcharts.
- Build: excluded `knowledge/` and the generated PDF locally through `.git/info/exclude` without changing `.gitignore`.
- Review: rendered a seven-page PDF, inspected every page, corrected first-page clipping, and confirmed the one-page overview fits completely.
- Delivery: submitted `harness-strategy-guide.pdf` to the paired NoteAir through Bluetooth File Exchange.
- Planning: reviewed the existing harness findings and selected false-green verification as the first discussion topic.
- Build: created `todo.md` with the concerns organized as actionable improvements.
- Review: ran verification and confirmed the current false-green behavior: one check ran, four categories skipped, and the command still passed.
- Build: added `.harness-required-checks`, category enforcement, regression tests, documentation, and CI ShellCheck installation.
- Review: regression tests passed; local verification and review correctly failed because the required lint category could not run without ShellCheck.
- Template initialized with agent guides, docs, task template, bootstrap script, verification script, review script, CI workflow, security guidance, and repository hygiene defaults.
- Planning: read required harness docs, active task template, `progress.md`, and inspected `scripts/verify.sh`.
- Build: replaced the inline `sh -c` Go format check with a named `check_go_format` function to avoid ShellCheck SC2016 while preserving behavior.
- Review: ran verification and review scripts, inspected the diff, and confirmed the change is limited to `scripts/verify.sh` and `progress.md`.
- Planning: read required harness docs, task template, README, scripts, `.gitignore`, and current local diffs for cross-directory orchestration support.
- Build: added target-project support to `scripts/init.sh`, `scripts/verify.sh`, and `scripts/review.sh` with `--project PATH` and `HARNESS_TARGET_ROOT`.
- Build: documented `.harness-db/` as ignored local database state for project registries, task notes, progress, indexes, and project documents.
- Review: ran syntax checks, verification, review, and target-option smoke checks.

## Next Steps

- Review and merge `lotharthesavior/feat-cli`. It is committed on the branch only; nothing was merged or pushed.
- Clear the pre-existing ShellCheck warnings in `scripts/verify.sh`, `scripts/action.sh`, `scripts/review.sh`, `tests/action-schema.sh`, and `tests/verify-required-checks.sh` so the required `lint` category can pass.
- Accept the incoming `harness-strategy-guide.pdf` transfer on the NoteAir.
- Install ShellCheck locally or rely on the configured CI environment for the complete lint-and-test gate.

## Blockers

- None for this feature. `scripts/verify.sh` fails on ShellCheck warnings that predate this branch in files owned by other work; the two new files are clean.

## Verification History

- 2026-09-03: `scripts/verify.sh` passed after the critical items. Result: ran=3 skipped=3 failures=0; `bash:shellcheck`, `harness:tests` (ten test scripts), and `bash:syntax` passed; run record written. A human fixed one ShellCheck nit in `scripts/permit.sh` because the denylist forbids the agent from editing the guard.
- 2026-09-03: `scripts/verify.sh` passed for the first time since required checks were added. Result: ran=3 skipped=3 failures=0; `bash:shellcheck`, `harness:tests` (five test scripts), and `bash:syntax` all passed; run record written to `.harness-db/records/verify.state`.
- 2026-09-03: `sh tests/install-guides.sh` passed; `make install-guides` twice on this repo produced identical files; `scripts/verify.sh` still ran no make targets and fails only on the pre-existing ShellCheck warnings (todo #10).
- 2026-09-03: `sh tests/harness-hook.sh` passed. `shellcheck scripts/hooks/require-phase.sh tests/harness-hook.sh` clean. `scripts/verify.sh` still fails only on the pre-existing ShellCheck warnings (todo #10).
- 2026-09-02: `sh tests/harness-cli.sh` passed. Result: `PASS: harness CLI phase order, budgets, pause, and continue`.
- 2026-09-02: `sh tests/action-schema.sh` passed. Result: `PASS: action schema validation`.
- 2026-09-02: `shellcheck scripts/harness tests/harness-cli.sh` passed with no output.
- 2026-09-02: `scripts/verify.sh` failed. Result: ran=2 skipped=3 failures=1. The only failure is required `lint`; every ShellCheck warning comes from `scripts/verify.sh`, `scripts/action.sh`, `scripts/review.sh`, `tests/action-schema.sh`, and `tests/verify-required-checks.sh`, and each reproduces against base commit `3e34bf0`. Required `test` was satisfied via `bash:syntax`.
- 2026-09-02: Manual CLI QA passed: subdirectory root discovery, exit 2 outside any harness root, blocked `build start` before `plan done` (exit 4), step-budget pause (exit 3) and continue with an evaluation note, refusal while paused (exit 3), loop-budget pause, time-budget pause, `status --json` leaving `run.json` unchanged, and `.harness-db/` staying untracked.
- 2026-09-01: `scripts/verify.sh` after recording the grill confirm. Result: ran=1 skipped=4 failures=1; required `lint` unavailable without ShellCheck; required `test` satisfied via `bash:syntax`.
- 2026-09-01: `sh tests/action-schema.sh` passed. Result: `PASS: action schema validation`.
- 2026-09-01: `scripts/verify.sh` failed as intended after adding the action schema. Result: ran=1 skipped=4 failures=1; required `lint` was unavailable without ShellCheck; required `test` was satisfied via `bash:syntax`.
- 2026-09-01: `scripts/verify.sh` failed as intended. Result: ran=1 skipped=4 failures=1; required `lint` was unavailable without ShellCheck; required `test` was satisfied via `bash:syntax`.
- 2026-08-27: PDF QA passed. Result: 7 pages, 444.96 x 593.04 points, grayscale-safe layout, complete one-page overview, and no visible clipping or overlap.
- 2026-08-27: `git check-ignore -v --no-index` confirmed the Markdown and PDF are locally excluded through `.git/info/exclude`.
- 2026-08-27: Bluetooth submission succeeded; macOS handed `harness-strategy-guide.pdf` to Bluetooth File Exchange for NoteAir acceptance.
- 2026-08-27: Harness regression tests passed; `scripts/verify.sh` still fails intentionally because required local ShellCheck is unavailable.
- 2026-08-27: `sh tests/verify-required-checks.sh` passed all required-category regression cases.
- 2026-08-27: `scripts/verify.sh` and `scripts/review.sh` failed as intended. Result: ran=1 skipped=4 failures=1; required `lint` was unavailable and required `test` was satisfied.
- 2026-08-27: `scripts/verify.sh` passed. Result: ran=1 skipped=4 failures=0. This demonstrates the false-green verification concern recorded in `todo.md`.
- 2026-05-25: `scripts/verify.sh` passed locally. Result: ran=1 skipped=4 failures=0. Note: local `shellcheck` is unavailable, so the ShellCheck path was skipped locally.
- 2026-05-25: `scripts/review.sh` passed locally. It reran `scripts/verify.sh` with the same result and printed a diff summary for `progress.md` and `scripts/verify.sh`.
- 2026-05-25: `sh -n scripts/init.sh`, `sh -n scripts/verify.sh`, and `sh -n scripts/review.sh` passed.
- 2026-05-25: `scripts/verify.sh` passed locally. Result: ran=1 skipped=4 failures=0. Note: local `shellcheck` is unavailable, so the ShellCheck path was skipped locally.
- 2026-05-25: `scripts/review.sh` passed locally. It reran `scripts/verify.sh` with the same result and printed target git diff summary.
- 2026-05-25: `scripts/init.sh --project .`, `scripts/verify.sh --project .`, `scripts/review.sh --project .`, and `HARNESS_TARGET_ROOT=. scripts/verify.sh` passed as target-root smoke checks.
- 2026-05-25: Final `scripts/verify.sh` passed locally after updating `progress.md`. Result: ran=1 skipped=4 failures=0. Note: local `shellcheck` is unavailable.
