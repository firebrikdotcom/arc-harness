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

```sh
scripts/harness plan start
# plan the work
scripts/harness plan done

scripts/harness build start
# do the work; run scripts/verify.sh before you call it done
scripts/harness build done

scripts/harness review start
# review; run scripts/review.sh
scripts/harness review done
```

You cannot build before plan is done. You cannot review before build is done.

`build done` needs a passing `scripts/verify.sh` run after `build start`, and `review done` needs a `scripts/review.sh` run after `review start`.

If it stops you: `scripts/harness status`. Only a human may run `scripts/harness continue "why it is ok to go on"` or `scripts/harness abort "reason"`, in a terminal or with the `!` prefix.

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
Makefile                  Helper targets: make install-guides
CLAUDE.md                 Claude-specific project instructions
SECURITY.md               Security and repository hygiene guidance
docs/architecture.md      Architecture notes and module boundaries
docs/conventions.md       Coding, testing, logging, and security conventions
docs/setup.md             Local setup and command documentation
progress.md               Current goal, decisions, steps, blockers, verification history
schemas/action.schema.json  Proposed-action contract
scripts/action.sh           Action validator
scripts/harness             Harness CLI: harness root, session budgets, plan/build/review phases
scripts/hooks/require-phase.sh  Claude Code PreToolUse hook: blocks edits and shell calls outside an active phase
.claude/settings.json       Registers the phase guard hook
scripts/init.sh             Safe bootstrap script
scripts/install-guides.sh   Adds the harness command block to AGENTS.md and CLAUDE.md
scripts/verify.sh           Local verification sensor
scripts/review.sh           Review helper
tasks/task-template.md      Reusable task template
tests/action-schema.sh      Action schema regression tests
tests/harness-cli.sh        Harness CLI phase-order and budget regression tests
tests/harness-hook.sh       Phase guard hook regression tests
tests/install-guides.sh     Guide block installer regression tests
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

It attempts formatter/check, lint, typecheck, tests, and build. Missing checks are reported as explicit skips. On the harness itself it also runs `tests/*.sh`, and every run writes a record to `.harness-db/records/verify.state`.

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
