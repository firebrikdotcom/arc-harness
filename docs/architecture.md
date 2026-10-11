# Architecture

## Current State

This repository currently contains AI-agent configuration and harness files. It is an orchestration harness, not necessarily the application repository being changed. Application source code, runtime entrypoints, package manifests, and deployment configuration may live in separate target project directories.

Known repo items:

- `AGENTS.md` (the map), `CLAUDE.md` (imports it), `docs/`, `tasks/`, `scripts/`, and `schemas/`: AI development harness. Run progress lives in each run's log under `.harness-db/`.
- `.github/workflows/`: CI automation.
- `.gitignore` and `SECURITY.md`: repository hygiene and security guidance.

Known local-only items:

- `.codex/`: local Codex/GSD agent configuration and installed helper agents.
- `.agents/`: local agent runtime files when present.
- `.venv/`: local Python virtual environment.
- `.harness-db/` or `harness-db/`: local harness database for project registries, task notes, run state, indexes, and project-specific documents.

## Orchestration Model

Use two explicit roots for cross-project work:

- Harness root: this template repository. It contains portable guides, scripts, conventions, and template files.
- Project root: the target repository or directory being planned, changed, bootstrapped, verified, or reviewed.

Harness scripts accept a target project directory with `--project PATH` or `HARNESS_TARGET_ROOT=PATH`.

Project-related documents are database records, not template source. Store project registries, per-project task files, run progress, generated indexes, and project notes under `.harness-db/` or another configured local database directory that is ignored by git. Do not add those files to the template repository unless a document is intentionally generalized into reusable template guidance.

Target projects do not need to contain this harness. When a target project has its own `AGENTS.md`, `CLAUDE.md`, setup docs, or verification scripts, those project-local instructions take precedence for work inside that target root.

Unknown placeholders:

- Product/domain architecture: unknown.
- Runtime language/framework: unknown.
- Main application entrypoint: unknown.
- Persistence layer: unknown.
- External services: unknown.
- Deployment target: unknown.

Update this document as soon as source modules or runtime boundaries are introduced.

## Module Boundaries

Until application code exists, use these boundaries:

- `docs/`: project documentation and architectural decisions.
- `tasks/`: task definitions and acceptance criteria.
- `scripts/`: local automation and verification sensors.
- `schemas/`: contracts for proposed agent actions. The model writes JSON; `scripts/action.sh validate` accepts or rejects it. This slice does not permit or execute the action.
- `.github/workflows/`: CI automation.
- `.harness-db/`: ignored local database state; never required for a clean template checkout.
- `scripts/task_route.py`: accepts only bounded, enum-based task metadata. It applies hard rules before optional TypeSafe routing, supports stable percentage-based active cohorts with a shadow holdback, and writes private route records under `.harness-db/routes/`. No prompt, source file, or diff is sent to the API.
- `scripts/agent_launch.py`: selects an exact argv command profile from the route. It runs no shell interpreter and leaves existing agent launches unchanged unless the launcher is used.
- `scripts/audit_emit.py`: emits redacted route, outcome, agent-completion, and token-measurement facts to the local Arc service; telemetry failure never blocks the task.
- `services/harness-audit/`: versioned Arc event-sourced audit sink and projection. It stores compact measurements and hashed evidence. Workflow user-prompt capture is separately configurable through the dashboard and disabled by default; tool inputs and outputs remain excluded.
- `scripts/codex_budget.py`: talks only to a connected Codex App Server, mirrors its persisted goal-token counter into harness state, and interrupts the exact active turn at an explicitly chosen cap. A direct CLI-owned thread cannot be interrupted by this separate connection.
- `scripts/jev_delegation.py`: the persisted `harness jev on|off|status` switch, one JSON file under `HARNESS_CONFIG_HOME` (default `~/.config/harness/`) outside every checkout, with `HARNESS_JEV_DELEGATION` as a per-shell override. `task_route.py`, `context_advice.py`, and the session-start route read it: off means shadow comparison, on means the route, launch profile, session route, and advise action follow Jev. It stores the exact model pin and never touches a deterministic gate.
- `scripts/context_advice.py`: accepts a curated decision context and dynamically supplied options during agent work, supports legacy choices and versioned v2 batches with baseline capture, typed validation, deterministic bypasses, outcome joins and cohort reporting, and saves a private record under `.harness-db/advice/`. It cannot execute the chosen option; with the switch on it returns Jev's choice as the `action` (cohort `delegated-1`), with it off the baseline (cohort `shadow-1`).
- `scripts/phase_checkpoint.py`: builds shadow checkpoints from enum-only git and run-state signals at the phase gates, verification, review, session start, and repeated-command hook; stores pending oracles under `.harness-db/advice-pending/` and labels them mechanically from verification exit codes and loop counts. It never changes a gate result.
- `scripts/jg.sh`: the only sanctioned path to jevgrep (`jg`), which is Jev used for source retrieval rather than for a bounded decision. It resolves the target and its private database, refuses the upload-widening flags and targets marked `.harness-no-upload`, streams `jg`'s output, and stores a compact record (question hash, timing, exit, completeness) under `retrieval/`. It never stores questions, paths, excerpts, or source, never runs from a hook or gate, and cannot authorize or verify anything.
- `scripts/harness-target.sh`: machine-local registry of harness targets under `.harness-db/targets/<id>/`. It resolves a session directory to its project root (git toplevel or the directory), registers it without touching the project, and gives every target a private database so runs, gate records, routes, and checkpoints never collide between parallel worktrees. `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`, and the hooks consult it.
- `scripts/hooks/auto-init.sh`, `scripts/hooks/session-route.sh`, and `scripts/hooks/jev-observe.sh`: Claude Code and Codex hooks. Auto-init runs at SessionStart, registers the project, runs `scripts/init.sh --auto`, and chains the session route; the other two are observation-only (SessionStart route, third-repeat progress checkpoint). `scripts/retrieval-reminder.sh` (PreToolUse for Grep and Glob, Claude Code only) adds one context line suggesting `scripts/jg.sh` on a session's first search when the run has no retrieval record. They always exit 0 and are installed by `scripts/install-hooks.sh`, which edits only its own hook groups and backs up the user files.
- `scripts/init.sh`: registers the target, discovers project-owned dependency setup, previews it, and executes it only after confirmation or explicit non-interactive opt-in. `--auto` runs it once per bootstrap-input fingerprint, in the background, recording state beside the registration.
- `scripts/verify.sh` and `scripts/review.sh`: non-mutating verification and review sensors. They write the records used by the build and review gates.
- `scripts/install-guides.sh`: idempotently refreshes the marked harness block in a target's agent guides without changing its surrounding instructions.
- `tools/auditability-planning-wizard.html`: dependency-free browser planner for the first Arc/JEV auditability slice. It keeps draft answers in browser-local storage and exports only the answers the operator supplies; it does not route tasks or call an external service.
- `.claude/settings.json` and `scripts/hooks/require-phase.sh`: Claude Code-only enforcement for the guard version, denylist, knowledge trust, the session's active phase (stale runs refused), build-only project writes, and tool-step accounting.
- `scripts/permit.py`: the denylist engine behind `scripts/permit.sh`. It parses shell commands into segments and judges the files they would write against the `path` rules; inline interpreter code and unparseable commands meet the `inline` rules.
- `scripts/task_contract.py` and `schemas/task.schema.json`: the run's bounded contract. `plan done` requires it, `build done` runs its acceptance commands through the denylist, `review done` checks non-goal paths.
- `scripts/review_findings.py` and `schemas/review-findings.schema.json`: the independent reviewer's verdict, bound to the tree hash it reviewed; `review done` requires an approval with no open blocker or major finding.
- `scripts/failure_budget.py`: post-tool hook that reports each shell outcome to `harness failure`; the same command failing the same way twice pauses the run.
- `scripts/tree-hash.sh`: a git tree of the project's files on disk, used to void verify and review records once project files change.
- `scripts/guard-version`: the guard's version stamp; a checkout older than the installed harness refuses work.

When application code is added, document each module with:

- Responsibility.
- Public interface.
- Dependencies allowed.
- Dependencies forbidden.
- Test strategy.

## Dependency Rules

- Follow existing project patterns first.
- Keep domain logic independent of UI, transport, and storage details where possible.
- Avoid circular dependencies.
- Keep scripts idempotent and safe to rerun.
- Keep harness scripts explicit about whether they operate on the harness root or a target project root.
- Keep task routing separate from permission and verification gates. Shadow mode preserves the default command; active routing requires observed outcomes and explicit opt-in.
- Dynamic TypeSafe advice may inform contextual judgment, but it cannot authorize work, waive required checks, or become an arbitrary command executor.
- Codex token enforcement is opt-in for App Server-owned threads through an explicit numeric cap; direct CLI launches refuse a cap they cannot enforce, and interruption requires both identifiers returned by App Server.
- Do not introduce new runtime dependencies without a clear reason and setup documentation.
- Do not require secrets for local verification or CI.

## Architectural Decision Log

Add dated decisions here as the system takes shape.

- 2026-05-24: Added an AI development harness with guides, verification scripts, progress tracking, and CI defaults. Application architecture remains unknown.
- 2026-05-25: Classified `.codex/`, `.agents/`, and `.venv/` as local-only artifacts because they can contain machine-specific paths, hooks, generated state, or installed dependencies.
- 2026-05-25: Defined the harness as a cross-project orchestrator. Project-specific documents and run state are local database content under ignored harness database directories, not tracked template files.
- 2026-09-01: Added a proposed-action schema and validator. The model proposes `run_command` or `write_file` JSON. The harness validates the shape. Permission checks and execution are not in this slice.
- 2026-09-19: Added dynamic TypeSafe advice as a separate, non-executing path. It takes a small redacted context and per-situation choices instead of a fixed global template; the existing deterministic gates remain authoritative.
- 2026-10-09: Closed the gaps against harness-engineering practice: every rule in AGENTS.md now has a gate. Runs are bound to a contract, verification must produce evidence and expires on project edits, an independent reviewer approves the exact files, two identical failures stop the run, the denylist judges write targets instead of command text, sessions resume from `harness brief`, and the always-loaded guide shrank to a map.
- 2026-09-21: Added opt-in Codex goal-token metering for App Server-owned threads. Existing active goal usage is preserved, direct CLI launches refuse unenforceable caps, and active-turn interruption uses the App Server protocol's required thread and turn identifiers.
- 2026-09-22: Added an offline auditability-planning wizard for choosing the first Arc/JEV event-sourcing slice without collecting raw prompts or making network requests.

- 2026-09-22: Broad Jev consideration uses guidance and explicit shadow checkpoints, not runtime interception. Baselines are persisted before evaluation; outcomes are local exclusive records. Task activation evidence is scoped to exact model and router/adapter fingerprint. No automatic promotion is introduced.
- 2026-09-22: Active task-entry delegation uses operator-controlled stable rollout percentages after the existing evidence gate. Holdback tasks remain shadow comparisons; no automatic percentage promotion is introduced.
- 2026-09-23: Added the Arc audit aggregate/projection and harness telemetry bridge. Full active rollout is available only through the explicit operator acknowledgement `HARNESS_TYPESAFE_OPERATOR_ACTIVATION=1`; deterministic safety gates remain authoritative, and automatic outcome labels remain `unknown` until independently reviewed.
- 2026-09-25: Made target initialisation automatic and per target. A SessionStart hook registers whatever project a session starts in and bootstraps it once per lockfile fingerprint; registration is machine-local state, never a tracked file in the project. Each registered target owns its database beneath `HARNESS_DB_ROOT`, replacing the single shared `runs/current` for cross-project work.
- 2026-09-24: Moved checkpoint generation from agent discipline to harness seams. Phase gates, verification, review, and two observation-only hooks emit shadow checkpoints from enum-only signals and label them from deterministic oracles; `jev-enable.sh` returns to shadow until the pilot batch exists. Automatic labels are mechanical rules recorded in `docs/jev-checkpoints.md`, not judgments; no activation, promotion, or gate behaviour changed.
- 2026-10-10: Made following Jev a persisted, reversible switch rather than a per-shell export. `harness jev on|off|status` writes one file outside the checkouts; the task-entry route, launcher, session-start route, and `advise` read it, so one decision covers every worktree and shell, and `off` returns everything to shadow comparison. The phase-gate checkpoints stay observations in both modes because the gates they predict are mechanical, and the deterministic gates run before any call as before. The switch counts as the operator's acknowledgement after reading the pilot report; it is not accuracy evidence, and shadow and delegated checkpoints report as separate cohorts.
- 2026-09-28: Adopted jevgrep as the harness's semantic retrieval step. Retrieval is the one Jev path that sends source to the provider, accepted by the operator for now; it is opt-in per target (`.harness-no-upload` refuses), goes only through `scripts/jg.sh`, and leaves only compact private records. The default denylist rejects the two upload-widening `jg` flags, and `plan done` checkpoints report whether retrieval was used so the pilot can compare runs. No gate, permission, or routing behaviour changed.
- 2026-09-29: Made the Jev pilot counter and cohort report machine-wide: `advise --report` and the `review done` counter sum every registered target's checkpoints (`--scope target|machine`, per-target `by_target` breakdown); cohorts still never pool across question, policy or model versions. Shadow only; no promotion or gate behaviour changed.


- 2026-10-02: Split audit collection into independently configurable Jev and Workflow categories. The Arc API gates ingestion using atomic local persisted settings shared with collectors. `scripts/audit_transport.py` queues stable-ID events in SQLite, and a service worker retries them without blocking harness gates. `scripts/workflow_audit.py` maintains exact native session/task bindings, collects selected lifecycle metadata and phase/sensor outcomes, and exposes explicit task/decision/outcome recording. Workflow rejects raw-prompt fields and does not infer final completion. `/workflow` presents source-time session timelines. This local collection layer does not change routing policy or shadow mode.

- `scripts/workflow_todos.py`: deterministic, machine-local plan state and completion criteria independent of collection switches.
- `scripts/workflow_gate.py`: additive native prompt/tool/Stop gate; resets prompt confirmation, requires an active todo for covered execution, and blocks completion without resolved required items and fresh passing checks. Workflow events preserve revision reasons and correlate tool/check facts to todo IDs.

- 2026-10-03: Run selection and verification records are scoped by native session identity within each target database. `scripts/run_paths.py` and its shell/Node adapter own hashed pointers and record paths; native hook adapters propagate identity independently of collection settings. Runs retain shared target history, while resumes preserve budgets and new sessions never adopt legacy paused runs. Sensor records carry run ownership, and pending checkpoint resolution does not consume another session's oracles.
