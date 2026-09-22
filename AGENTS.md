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
- On a budget pause, an agent may run `harness continue "<evaluation note>"` only after an explicit user instruction to continue in the current conversation. Keep the required evaluation note and record the authorization concisely (for example, `User explicitly requested continuation in chat.`). `harness abort` and `scripts/knowledge-trust.sh approve` remain human-only; do not invoke them yourself.
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

## Task-entry Routing

When a task is started through an agent launcher, use `scripts/harness launch --state TASK.json` with compact metadata prepared by the launcher. The default route is shadow mode: TypeSafe records its judgment, while the existing agent command runs. Never put a raw prompt, source, diff, credential, or personal data in the task metadata. Required checks, known failures, user choices, permissions, and irreversible work follow deterministic rules before any API call. See `docs/setup.md` for the schema, command profiles, and active-mode gate. Direct interactive sessions do not pass through this launch step.

## Dynamic TypeSafe Advice

During work, use `scripts/harness advise --context DECISION.json` only when a real context-specific judgment remains. The context contains a concise goal, relevant facts and constraints, plus the options that fit this situation; it is not a raw prompt, source file, diff, credential, or personal data. The result is advisory: it never executes an option or overrides permissions, failed checks, destructive-action rules, or required verification.

## Success Bar

An agent may only mark work complete when:

- Acceptance criteria are satisfied or explicitly documented as not applicable.
- `scripts/verify.sh` has run.
- Failures, skips, or missing project tooling are documented.
- `progress.md` reflects the final state.

Verification detects non-rewriting format checks, lint, typecheck, tests, and builds. A target can require any of those categories through `.harness-required-checks` or `HARNESS_REQUIRED_CHECKS`; a required category with no runnable check fails. `scripts/verify.sh` and `scripts/review.sh` write gate records beneath `.harness-db/records/` for `build done` and `review done`.

<!-- harness-cli:start -->
## Harness Phases

Every session runs inside a harness phase. Open one before editing files or running commands. Where the phase guard hook is installed, Write, Edit, and Bash are blocked until a phase is active.

```sh
scripts/harness plan start      # read, scope the task, record the plan in progress.md
scripts/harness plan done
scripts/harness build start     # implement; run scripts/verify.sh before finishing
scripts/harness build done
scripts/harness review start    # run scripts/review.sh and inspect the diff
scripts/harness review done
```

Record work with `scripts/harness step --note "..."`. When blocked, run `scripts/harness status`. After a budget pause, wait for an explicit user instruction to continue in this conversation, then evaluate and run `scripts/harness continue "<evaluation note that records that authorization>"`.
<!-- harness-cli:end -->

<!-- shared-rule:lavish-sequential-review:start -->
# Lavish Review: One Viewport, One Approval

- Apply this approach to every Lavish session from now on unless the user explicitly changes it.
- Present only one small review part at a time. All content and decision controls for that part must fit within the actual available viewport, accounting for Lavish chrome and feedback panels, without vertical or horizontal scrolling or nested scroll areas.
- Split oversized material into smaller review parts. Do not hide overflow, clip content, or shrink text to unreadable sizes to simulate fitting. Check viewport fit before presenting each part and after layout or viewport changes.
- Wait for the user's explicit approval of the current part before presenting or unlocking the next part. Silence, elapsed time, feedback, requested edits, or navigation are not approval. Revise the current part and obtain approval when changes are requested.
- Record which part and revision was approved, preserve decisions, and keep later parts gated across reloads and agent handoffs. Approval of a plan part authorizes only that review progression, not implementation or deployment.
<!-- shared-rule:lavish-sequential-review:end -->

## Jev consideration during work

Consider Jev at every meaningful decision: tool selection, reasoning allocation, identification, prioritization, evidence selection/assessment, progress, handoff, context selection, and clarification. Apply explicit rules and user choices first; consideration does not require an API call. Use version 2 `scripts/harness advise --context CHECKPOINT.json` for remaining bounded judgments, capturing the baseline action before evaluation and keeping the checkpoint in shadow mode. Record the actual action and independently supported outcome with `advise --record OUTCOME.json`; inspect `advise --report`. Batch independent questions and revisit only after material evidence changes. Jev never overrides authorization, required verification, failures, or completion gates. See [Jev checkpoints](docs/jev-checkpoints.md) for input formats and the 30-decision pilot. This is guidance plus observable checkpoints, not interception of private reasoning.
