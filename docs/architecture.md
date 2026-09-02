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
- Do not introduce new runtime dependencies without a clear reason and setup documentation.
- Do not require secrets for local verification or CI.

## Architectural Decision Log

Add dated decisions here as the system takes shape.

- 2026-05-24: Added an AI development harness with guides, verification scripts, progress tracking, and CI defaults. Application architecture remains unknown.
- 2026-05-25: Classified `.codex/`, `.agents/`, and `.venv/` as local-only artifacts because they can contain machine-specific paths, hooks, generated state, or installed dependencies.
- 2026-05-25: Defined the harness as a cross-project orchestrator. Project-specific documents and run state are local database content under ignored harness database directories, not tracked template files.
- 2026-09-01: Added a proposed-action schema and validator. The model proposes `run_command` or `write_file` JSON. The harness validates the shape. Permission checks and execution are not in this slice.
