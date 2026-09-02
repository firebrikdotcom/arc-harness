# AI Development Harness Template

This template provides an AI development harness so work can be planned, implemented, verified, reviewed, and resumed safely.

The harness can operate on this repository or on a separate target project directory. Treat this repo as the control plane and the target project as the workspace being changed.

## Start Here

From the project root, run:

```sh
scripts/init.sh
```

For a separate target project:

```sh
scripts/init.sh --project /path/to/project
```

Then read:

- `AGENTS.md`
- `CLAUDE.md`
- `docs/architecture.md`
- `docs/conventions.md`
- `docs/setup.md`
- `progress.md`

These files explain how agents and humans should work in this repo.

## Daily Workflow

1. Define the task.
   - Copy `tasks/task-template.md` into a new task note, or use it as a checklist.
   - Write the goal, context, acceptance criteria, constraints, implementation plan, verification plan, and rollback notes.
   - For cross-project work, keep project-specific task notes and progress in `.harness-db/` or another ignored harness database path.

2. Plan before coding.
   - Read the relevant docs and files.
   - Identify both the harness root and target project root.
   - Break work into small steps.
   - Record the current goal and plan in `progress.md`.

3. Build in small steps.
   - Keep changes scoped to the task.
   - Follow existing patterns first.
   - Update tests and docs when behavior changes.
   - Update `progress.md` after each meaningful step.

4. Verify before calling work complete.
   - Run:

```sh
scripts/verify.sh
```

For a separate target project:

```sh
scripts/verify.sh --project /path/to/project
```

5. Review before handoff or PR.
   - Run:

```sh
scripts/review.sh
```

For a separate target project:

```sh
scripts/review.sh --project /path/to/project
```

## Important Rules

- Do not declare success without running `scripts/verify.sh`.
- Before a side-effecting change, write action JSON and run `scripts/action.sh validate PATH`.
- Keep planning, building, and reviewing as separate phases.
- Do not assume secrets exist locally or in CI.
- Do not delete existing files unless the task explicitly requires it.
- Record commands run and results in `progress.md` and handoff notes.

## Project Structure

```text
.github/workflows/ci.yml  CI verification
.gitignore                Repo hygiene for local artifacts, secrets, and generated output
AGENTS.md                 Agent operating guide
CLAUDE.md                 Claude-specific project instructions
SECURITY.md               Security and repository hygiene guidance
docs/architecture.md      Architecture notes and module boundaries
docs/conventions.md       Coding, testing, logging, and security conventions
docs/setup.md             Local setup and command documentation
progress.md               Current goal, decisions, steps, blockers, verification history
schemas/action.schema.json  Proposed-action contract
scripts/action.sh           Action validator
scripts/init.sh             Safe bootstrap script
scripts/verify.sh           Local verification sensor
scripts/review.sh           Review helper
tasks/task-template.md      Reusable task template
tests/action-schema.sh      Action schema regression tests
```

Ignored local database content:

```text
.harness-db/              Local project registry, run state, task notes, indexes, and project documents
```

## Verification

Use:

```sh
scripts/verify.sh
```

Or, for a target project outside this repo:

```sh
scripts/verify.sh --project /path/to/project
```

The script detects common project tooling:

- `Makefile` targets when available.
- `package.json` with `npm`, `pnpm`, or `yarn` when available.
- `composer.json` for PHP projects.
- `go.mod` for Go projects.
- `Cargo.toml` for Rust projects.
- Bash/shell files, including `scripts/*.sh`.

It attempts formatter/check, lint, typecheck, tests, and build. Missing checks are reported as explicit skips.

## CI

CI is defined in `.github/workflows/ci.yml`.

It runs:

```sh
scripts/init.sh
scripts/verify.sh
```

CI uses safe defaults and does not assume secrets.

## Updating This Project

When source code, runtime commands, dependencies, or architecture are added:

- Update `docs/setup.md` with exact setup and run commands.
- Update `docs/architecture.md` with module boundaries and dependency rules.
- Update `docs/conventions.md` if new language/framework conventions are introduced.
- Keep `progress.md` current as work proceeds.

## Local-Only Files

Do not commit local runtime state such as `.venv/`, `.codex/`, `.agents/`, `.harness-db/`, caches, real `.env` files, or private keys. Regenerate local agent tooling per workstation.

Project-related documents are local database records for the harness. Keep project registries, task notes, run progress, generated indexes, and project-specific notes in `.harness-db/` or another ignored database directory instead of tracking them in this template repository.
