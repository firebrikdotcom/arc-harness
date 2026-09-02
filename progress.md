# Progress

## Current Goal

Confirmed harness decisions are recorded. Wait for a build request; do not start implementation from this confirm.

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
- 2026-09-01: Grill confirmed. Full keep/leave list is in `harness-review.md`. Overrides: no executor; dry-run dropped; detection stays (no manifest); Make still wins; init still runs project setup immediately; `knowledge/` hooks still always run. Permit is allow-unless-denied with a harness default denylist that a project file replaces. Session budgets and phase gates wait for a future `harness` CLI.

## Active Plan

Phase: Review complete — 2026-09-01 grill confirmed. Build not started.

Goal: implement only the confirmed keep/build list in `harness-review.md` when a build is requested.

Non-goals: permission, execution, dry-run, harness CLI, JSON Schema library.

Harness root: `/Users/savior/Code/harness-template`

Target project root: `/Users/savior/Code/harness-template`

Acceptance criteria:

- `schemas/action.schema.json` defines versioned `run_command` and `write_file` actions.
- `scripts/action.sh validate PATH` exits 0 for valid actions and 2 for invalid ones.
- Regression tests cover valid and invalid proposals.
- Docs state propose-then-validate. Execute is out of scope.

Implementation plan:

1. Add the schema contract.
2. Add the validator script.
3. Add regression tests.
4. Update setup, architecture, agent guides, README, and the review.

Verification plan:

- Run `sh tests/action-schema.sh`.
- Run `scripts/verify.sh` and record the known local ShellCheck gap.

Known risks:

- Agents can skip the validator and still call Write or Shell.
- Schema file and script rules can drift.
- Validator needs `python3` or `node`.

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

- Accept the incoming `harness-strategy-guide.pdf` transfer on the NoteAir.
- Install ShellCheck locally or rely on the configured CI environment for the complete lint-and-test gate.

## Blockers

- Local ShellCheck is unavailable, so the newly required lint category cannot pass on this machine.

## Verification History

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
