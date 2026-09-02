# Agent Operating Guide

This repository uses the Harness pattern so AI agents can work safely and repeatedly:

1. Guides: read project instructions before changing files.
2. Sensors: run verification after changes.
3. Memory + Bootstrap: keep setup and progress state current.
4. Process isolation: keep planning, building, and reviewing in separate runs or clearly separated phases.

## Required Workflow

Before coding:

- Read `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`, `docs/conventions.md`, `docs/setup.md`, and the active task file.
- Identify the harness root and the target project root. For cross-project work, do not assume they are the same directory.
- Inspect the current tree and relevant files before making assumptions.
- Decompose work into small tasks with clear acceptance criteria.
- Update `progress.md` with the current goal, plan, and first step.
- For cross-project work, keep project registries, task notes, run progress, generated indexes, and project-specific documents in the ignored harness database directory such as `.harness-db/`, not in tracked template files.

During work:

- Before a side-effecting tool call, write an action JSON file and run `scripts/action.sh validate PATH`. Do not treat a rejected proposal as approved.
- Keep changes scoped to the active task.
- Prefer existing project patterns over new abstractions.
- Make one meaningful change at a time and update `progress.md` after each meaningful step.
- Do not delete existing files unless the task explicitly requires it.
- Preserve user changes and unrelated worktree changes.
- When operating on another project, run harness scripts with `--project PATH` or `HARNESS_TARGET_ROOT=PATH`.

After work:

- Run `scripts/verify.sh` before declaring completion. Use `scripts/verify.sh --project PATH` when the target project is outside the harness root.
- Run `scripts/review.sh` for review-oriented passes or before opening a PR.
- Do not declare success unless verification ran and the result is recorded.
- Update `progress.md` with completed steps, decisions, next steps, blockers, and verification history.

## Process Isolation

Use separate runs/processes for distinct responsibilities:

- Planning run: clarify goal, read docs, inspect code, write/update task plan.
- Build run: implement the scoped change and update progress.
- Review run: run verification, inspect diff, ask review questions, and record risks.
- PR run: summarize changes, commands, results, and unresolved risks.

If a single interactive session performs multiple responsibilities, mark the phase transition in `progress.md` and keep the review phase separate from implementation decisions.

## Success Bar

An agent may only mark work complete when:

- Acceptance criteria are satisfied or explicitly documented as not applicable.
- `scripts/verify.sh` has run.
- Failures, skips, or missing project tooling are documented.
- `progress.md` reflects the final state.
