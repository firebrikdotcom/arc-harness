# Claude Harness Instructions

This template is prepared for AI-assisted development using a harness of guides, sensors, persistent memory, and isolated work phases.

## Template Operating Rules

- Read `AGENTS.md`, `docs/architecture.md`, `docs/conventions.md`, and `docs/setup.md` before coding.
- Identify the harness root and target project root before planning or editing. They may be different directories.
- Follow existing patterns first. If no pattern exists, choose the smallest clear implementation and document the decision.
- Keep task scope narrow. Split large requests into small steps before editing.
- Before a side-effecting tool call, write an action JSON file and run `scripts/action.sh validate PATH`. Do not treat a rejected proposal as approved.
- Treat a budget pause, `harness continue`, and `harness abort` as human-only decisions. Report the state instead of attempting to resume or abort the run.
- Update `progress.md` after every meaningful step: planning, implementation, verification, review, blockers, and decisions.
- Do not claim success without running `scripts/verify.sh`.
- Cite exact commands run and their results in the final response.
- For cross-project work, store project-specific registries, task notes, progress, and generated indexes in ignored harness database state such as `.harness-db/`; do not track those records in this template repo.

## Task-entry Routing

For tasks launched with structured metadata, use `scripts/harness launch --state TASK.json` before starting the agent. It defaults to shadow mode and preserves the existing launch command. Never send raw prompts, code, diffs, credentials, or personal data to TypeSafe; deterministic rules own permissions and required verification. See `docs/setup.md`. Direct interactive sessions do not pass through this launch step.

For a real judgment that arises during work, use `scripts/harness advise --context DECISION.json`. Its context must be a concise, redacted summary of the goal, facts, constraints, risks, and situation-specific options. Treat the returned choice as advice only: deterministic permission, safety, failure, and verification rules still decide whether work may proceed.

## Planning Behavior

Planning should be a separate run or clearly separated phase.

Planning output must include:

- Goal and non-goals.
- Relevant files and docs read.
- Harness root and target project root.
- Acceptance criteria.
- Implementation plan.
- Verification plan.
- Known risks or unknowns.

Record the plan in `progress.md` before building.

## Build Behavior

Build work should be a separate run or clearly separated phase.

During build:

- Implement only the agreed scope.
- Prefer small commits/patches when practical.
- Keep unrelated files untouched.
- Add or update tests when behavior changes.
- Update docs when behavior or setup changes.
- Update `progress.md` after each meaningful step.

## Review Behavior

Review should be a separate run or clearly separated phase.

Run:

```sh
scripts/review.sh
```

Review must consider:

- Does the change satisfy acceptance criteria?
- Are tests meaningful?
- Did the work avoid scope creep?
- Are docs and `progress.md` updated?
- Are there security or performance risks?

## PR Behavior

Before opening or preparing a PR:

- Run `scripts/verify.sh`, or `scripts/verify.sh --project PATH` when the target project is outside the harness root.
- Inspect `git diff`.
- Summarize what changed.
- Include exact commands run and results.
- Include known skips, failures, risks, or follow-ups.
- Do not assume secrets are available in CI.

## Completion Requirements

A task is complete only when:

- The requested change is implemented.
- `scripts/verify.sh` has run.
- Verification result is recorded in `progress.md`.
- The final response cites exact commands and outcomes.

`scripts/verify.sh` detects non-rewriting format checks, lint, typecheck, tests, and builds. `.harness-required-checks` or `HARNESS_REQUIRED_CHECKS` can require any category; a required category that runs no check fails. The verify and review scripts write their gate records under `.harness-db/records/`.

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

Record work with `scripts/harness step --note "..."`. When blocked, run `scripts/harness status`. After a budget pause, evaluate and run `scripts/harness continue "<evaluation note>"`.
<!-- harness-cli:end -->
