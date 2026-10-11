# Agent Operating Guide

This repository is a harness: the environment that makes an AI agent's work bounded, checked, and resumable. This file is the map. Read it, then open only the files a task needs.

## Map

| Path | What it holds |
| --- | --- |
| `scripts/harness` | The CLI: phases, contract, budgets, pauses, brief, retention. `scripts/harness --help` lists every command. |
| `scripts/verify.sh`, `scripts/review.sh` | The sensors: checks with evidence, and the independent review packet. |
| `scripts/hooks/require-phase.sh`, `scripts/permit.py` | The guard: phase rules and the denylist, judged on the files a command would write. |
| `scripts/failure_budget.py`, `scripts/workflow_gate.py` | The repeated-failure stop and the todo gate. |
| `schemas/` | Task contract, review findings, action proposals, the default denylist. |
| `tasks/task.example.json` | A filled-in task contract to copy. |
| `docs/setup.md` | Every command and environment variable in detail. |
| `docs/architecture.md`, `docs/conventions.md` | How the pieces fit; coding conventions. |
| `docs/jev-checkpoints.md` | Optional Jev (TypeSafe) advice, and the `harness jev` switch that decides whether it is followed. |
| `tests/` | One script per behaviour; `scripts/verify.sh` runs them all. |

The harness root and the target project may differ. Run the scripts with `--project PATH` for another project; its runs, records, and map live in the ignored `.harness-db/`, never in the project.

## Rules and the gates that enforce them

Each rule is written here so you know the goal, and enforced by a gate so it holds under load.

| Rule | Gate |
| --- | --- |
| Work inside a phase; edit only in build. | The guard blocks Write, Edit, and shell writes outside build (plan and review may write `progress.md`, `tasks/`, `task.json`, `review-findings.json`). |
| Every run has a bounded contract: deliverables, non-goals, acceptance commands. | `plan done` needs `harness contract set` (or a recorded `contract waive`). |
| Done means evidence, not a claim. | `build done` needs a passing `scripts/verify.sh` on the current files (a run with no checks fails) and every acceptance command to pass. |
| The author does not approve their own work. | `review done` needs an independent reviewer's approving findings for the current files, and no non-goal path touched. Findings come from the reviewer a person configured (`.harness-db/reviewer`) or are submitted by a person, who types `yes` on a terminal. |
| Evidence is about this project. | Gates accept verify and review records only for the run's own project, and the guard refuses calls in a project other than the run's. |
| Stop when the same failure repeats. | Two identical failures in a row pause the run; resuming needs the user and a new approach. |
| Stay inside budgets. | Steps, loops, and continues are counted; a pause waits for the user. `abort` is the user's. |
| Do not touch the guard, its state, secrets, or git internals. | The denylist refuses writes to them (including all of `.harness-db/`), however the command reaches them; reading them is fine. |
| Resume from durable state, not memory. | Session start prints `harness brief`; runs left idle for a day expire. |

If a gate blocks you, read its message: it says what to do next. Do not work around a gate; if it is wrong, say so. The gates stop mistakes and shortcuts; they are not a sandbox (docs/setup.md, "What the guard can and cannot stop").

## Working style

- Follow existing patterns; keep the change to the contract's scope.
- Preserve user changes and unrelated files.
- Report exactly what ran and its result; say what was skipped.

<!-- harness-cli:start -->
## Harness

Work runs in phases, and a guard enforces each rule below. Edits are allowed only in build.

```sh
scripts/harness plan start                  # read; write the task contract (tasks/task.example.json)
scripts/harness contract set task.json      # or: scripts/harness contract waive "<why there is nothing to accept>"
scripts/harness plan done                   # needs the contract
scripts/harness build start                 # make the change
scripts/verify.sh --project .              # evidence: fails when no check ran
scripts/harness build done                  # needs that verify on the current files and every acceptance command
scripts/harness review start
scripts/review.sh --project .              # writes a packet for an independent reviewer (fresh context)
scripts/harness review submit findings.json # a person submits a fresh-context reviewer's verdict
scripts/harness review done                 # needs an approval of the current files; no non-goal path touched
```

- Session start prints `scripts/harness brief`: this session's run (or the latest), its contract, any pause, recent steps, and the project map. Record steps with `scripts/harness step --note "..."`.
- A pause (budget, or the same failure twice) waits for the user. Resume only on their instruction: `scripts/harness continue "<new approach>"`. Abort is theirs.
- Jev (TypeSafe) advice and the task launcher: docs/jev-checkpoints.md, docs/setup.md. `scripts/harness jev status` says whether Jev is followed: off, every call is a shadow comparison and your own baseline runs; on, the session-start route is the route to take and `scripts/harness advise` returns Jev's choice as `action` with `delegated: true`, so take that action unless a deterministic rule (permissions, required checks, failures, the user's choice) decides otherwise, then label the call. Semantic search: `scripts/jg.sh --project PATH "question"`.
<!-- harness-cli:end -->

<!-- shared-rule:lavish-sequential-review:start -->
# Lavish Review: One Viewport, One Approval

- Apply this approach to every Lavish session from now on unless the user explicitly changes it.
- Present only one small review part at a time. All content and decision controls for that part must fit within the actual available viewport, accounting for Lavish chrome and feedback panels, without vertical or horizontal scrolling or nested scroll areas.
- Split oversized material into smaller review parts. Do not hide overflow, clip content, or shrink text to unreadable sizes to simulate fitting. Check viewport fit before presenting each part and after layout or viewport changes.
- Wait for the user's explicit approval of the current part before presenting or unlocking the next part. Silence, elapsed time, feedback, requested edits, or navigation are not approval. Revise the current part and obtain approval when changes are requested.
- Record which part and revision was approved, preserve decisions, and keep later parts gated across reloads and agent handoffs. Approval of a plan part authorizes only that review progression, not implementation or deployment.
<!-- shared-rule:lavish-sequential-review:end -->
