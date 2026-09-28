# Architecture

## Current State

This repository currently contains AI-agent configuration and harness files. It is an orchestration harness, not necessarily the application repository being changed. Application source code, runtime entrypoints, package manifests, and deployment configuration may live in separate target project directories.

Known repo items:

- `AGENTS.md`, `CLAUDE.md`, `docs/`, `tasks/`, `scripts/`, `schemas/`, and `progress.md`: AI development harness.
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
- `services/harness-audit/`: versioned Arc event-sourced audit sink and projection. It stores only compact measurements and hashed evidence, not prompts, source, diffs, credentials, or raw evidence text.
- `scripts/codex_budget.py`: talks only to a connected Codex App Server, mirrors its persisted goal-token counter into harness state, and interrupts the exact active turn at an explicitly chosen cap. A direct CLI-owned thread cannot be interrupted by this separate connection.
- `scripts/context_advice.py`: accepts a curated decision context and dynamically supplied options during agent work, supports legacy choices and versioned shadow batches with baseline capture, typed validation, deterministic bypasses, outcome joins and cohort reporting, and saves a private record under `.harness-db/advice/`. It cannot execute the chosen option.
- `scripts/phase_checkpoint.py`: builds shadow checkpoints from enum-only git and run-state signals at the phase gates, verification, review, session start, and repeated-command hook; stores pending oracles under `.harness-db/advice-pending/` and labels them mechanically from verification exit codes and loop counts. It never changes a gate result.
- `scripts/jg.sh`: the only sanctioned path to jevgrep (`jg`), which is Jev used for source retrieval rather than for a bounded decision. It resolves the target and its private database, refuses the upload-widening flags and targets marked `.harness-no-upload`, streams `jg`'s output, and stores a compact record (question hash, timing, exit, completeness) under `retrieval/`. It never stores questions, paths, excerpts, or source, never runs from a hook or gate, and cannot authorize or verify anything.
- `scripts/harness-target.sh`: machine-local registry of harness targets under `.harness-db/targets/<id>/`. It resolves a session directory to its project root (git toplevel or the directory), registers it without touching the project, and gives every target a private database so runs, gate records, routes, and checkpoints never collide between parallel worktrees. `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`, and the hooks consult it.
- `scripts/hooks/auto-init.sh`, `scripts/hooks/session-route.sh`, and `scripts/hooks/jev-observe.sh`: Claude Code and Codex hooks. Auto-init runs at SessionStart, registers the project, runs `scripts/init.sh --auto`, and chains the session route; the other two are observation-only (SessionStart route, third-repeat progress checkpoint). They always exit 0 and are installed by `scripts/install-hooks.sh`, which edits only its own hook groups and backs up the user files.
- `scripts/init.sh`: registers the target, discovers project-owned dependency setup, previews it, and executes it only after confirmation or explicit non-interactive opt-in. `--auto` runs it once per bootstrap-input fingerprint, in the background, recording state beside the registration.
- `scripts/verify.sh` and `scripts/review.sh`: non-mutating verification and review sensors. They write the records used by the build and review gates.
- `scripts/install-guides.sh`: idempotently refreshes the marked harness block in a target's agent guides without changing its surrounding instructions.
- `tools/auditability-planning-wizard.html`: dependency-free browser planner for the first Arc/JEV auditability slice. It keeps draft answers in browser-local storage and exports only the answers the operator supplies; it does not route tasks or call an external service.
- `.claude/settings.json` and `scripts/hooks/require-phase.sh`: Claude Code-only enforcement for denylist, knowledge trust, active phase, and tool-step accounting.

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
- 2026-09-21: Added opt-in Codex goal-token metering for App Server-owned threads. Existing active goal usage is preserved, direct CLI launches refuse unenforceable caps, and active-turn interruption uses the App Server protocol's required thread and turn identifiers.
- 2026-09-22: Added an offline auditability-planning wizard for choosing the first Arc/JEV event-sourcing slice without collecting raw prompts or making network requests.

- 2026-09-22: Broad Jev consideration uses guidance and explicit shadow checkpoints, not runtime interception. Baselines are persisted before evaluation; outcomes are local exclusive records. Task activation evidence is scoped to exact model and router/adapter fingerprint. No automatic promotion is introduced.
- 2026-09-22: Active task-entry delegation uses operator-controlled stable rollout percentages after the existing evidence gate. Holdback tasks remain shadow comparisons; no automatic percentage promotion is introduced.
- 2026-09-23: Added the Arc audit aggregate/projection and harness telemetry bridge. Full active rollout is available only through the explicit operator acknowledgement `HARNESS_TYPESAFE_OPERATOR_ACTIVATION=1`; deterministic safety gates remain authoritative, and automatic outcome labels remain `unknown` until independently reviewed.
- 2026-09-25: Made target initialisation automatic and per target. A SessionStart hook registers whatever project a session starts in and bootstraps it once per lockfile fingerprint; registration is machine-local state, never a tracked file in the project. Each registered target owns its database beneath `HARNESS_DB_ROOT`, replacing the single shared `runs/current` for cross-project work.
- 2026-09-24: Moved checkpoint generation from agent discipline to harness seams. Phase gates, verification, review, and two observation-only hooks emit shadow checkpoints from enum-only signals and label them from deterministic oracles; `jev-enable.sh` returns to shadow until the pilot batch exists. Automatic labels are mechanical rules recorded in `docs/jev-checkpoints.md`, not judgments; no activation, promotion, or gate behaviour changed.
- 2026-09-28: Adopted jevgrep as the harness's semantic retrieval step. Retrieval is the one Jev path that sends source to the provider, accepted by the operator for now; it is opt-in per target (`.harness-no-upload` refuses), goes only through `scripts/jg.sh`, and leaves only compact private records. The default denylist rejects the two upload-widening `jg` flags, and `plan done` checkpoints report whether retrieval was used so the pilot can compare runs. No gate, permission, or routing behaviour changed.
