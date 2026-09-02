# Setup

## Prerequisites

Required:

- POSIX-compatible shell.
- `git`.

Optional, depending on project files:

- `make` when a `Makefile` exists.
- Node.js and one of `npm`, `pnpm`, or `yarn` when `package.json` exists.
- `composer` and PHP when `composer.json` exists.
- Go when `go.mod` exists.
- Rust and Cargo when `Cargo.toml` exists.
- `shellcheck` for stronger Bash/shell linting.
- `python3` or `node` to validate proposed action JSON with `scripts/action.sh` and to store `scripts/harness` run state.
- Other language runtimes as documented by future project code.

## Bootstrap

From the repository root:

```sh
scripts/init.sh
```

The bootstrap script is safe and idempotent. It checks available tools, installs dependencies when a known lockfile or manifest is present, and prints next steps.

To bootstrap a separate target project directory from this harness:

```sh
scripts/init.sh --project /path/to/project
```

Or:

```sh
HARNESS_TARGET_ROOT=/path/to/project scripts/init.sh
```

Detected bootstrap inputs:

- `Makefile`: runs `make init` or `make setup` when either target exists.
- `package.json`: installs JavaScript/TypeScript dependencies with `pnpm`, `yarn`, or `npm`.
- `composer.json`: installs PHP dependencies with Composer.
- `go.mod`: downloads Go modules with `go mod download`.
- `Cargo.toml`: fetches Rust dependencies with Cargo.

## Environment Variables

No required environment variables are currently known.

Optional harness variables:

- `HARNESS_TARGET_ROOT`: target project directory for `scripts/init.sh`, `scripts/verify.sh`, and `scripts/review.sh` when `--project` is not passed.
- `HARNESS_ROOT`: harness root for `scripts/harness` when automatic discovery should be skipped.
- `HARNESS_DB_ROOT`: harness state directory for `scripts/harness`. Defaults to `HARNESS_ROOT/.harness-db`.
- `HARNESS_BUDGET_STEPS`, `HARNESS_BUDGET_TIME_MIN`, `HARNESS_BUDGET_LOOPS`, `HARNESS_BUDGET_TOKENS`: session budget caps read when a `scripts/harness` run is created.

When environment variables are introduced, document each one here:

- Name.
- Required or optional.
- Example value.
- Whether it is safe for local development.
- Where it is used.

Do not commit real secrets.

Local-only files such as `.env`, `.venv/`, `.codex/`, `.agents/`, `.harness-db/`, caches, and private keys are ignored by default.

Project registries, task notes, generated indexes, and per-project progress documents belong in `.harness-db/` or another ignored local database directory. Treat them as database content owned by the local harness installation, not as tracked template files.

## Run

No application run command is currently known.

When an entrypoint is introduced, document it here. Prefer project-native commands such as:

```sh
make run
```

or:

```sh
npm run dev
```

## Test, Lint, Typecheck, Format, Build

Use the verification sensor:

```sh
scripts/verify.sh
```

To verify a separate target project directory:

```sh
scripts/verify.sh --project /path/to/project
```

To review a separate target project directory:

```sh
scripts/review.sh --project /path/to/project
```

The review script runs verification in the target project root and prints target git changes separately from harness git changes.

To validate a proposed action JSON file:

```sh
scripts/action.sh validate PATH
```

The validator accepts or rejects the file against `schemas/action.schema.json`. It does not execute the action. `python3` is used when available; otherwise `node`. One of those runtimes is required.

To run the harness regression tests:

```sh
sh tests/action-schema.sh
sh tests/harness-cli.sh
```

## Harness CLI

`scripts/harness` owns session budgets and plan/build/review phase order. It is a control plane only: it records and gates, and never runs Write, Shell, or git for the model.

It finds the harness root by walking up from the current directory for a directory containing `AGENTS.md` and `scripts/verify.sh`. Set `HARNESS_ROOT` to skip discovery.

```sh
scripts/harness plan start
scripts/harness plan done
scripts/harness build start
scripts/harness step --note "edit scripts/verify.sh"
scripts/harness build done
scripts/harness review start
scripts/harness review done
scripts/harness status
scripts/harness status --json
scripts/harness continue "<evaluation note>"
```

Phase rules:

- `build start` fails until `plan done` has run.
- `review start` fails until `build done` has run.
- `PHASE done` fails unless that phase is active.
- Re-starting a phase that is already done counts against the loop budget.

Session budgets are counted per run and are agent-visible rules:

| Budget | Default cap | Counted by |
|---|---|---|
| `steps` | 20 | every phase command and every `harness step` |
| `time_min` | 15 | wall-clock minutes since the run was created |
| `loops` | 1 | re-entering a phase that was already marked done |
| `tokens` | unknown | only what `harness step --tokens N` reports |

Set caps with `HARNESS_BUDGET_STEPS`, `HARNESS_BUDGET_TIME_MIN`, `HARNESS_BUDGET_LOOPS`, and `HARNESS_BUDGET_TOKENS`. They are read when the run is created. The CLI cannot count tokens itself, so the token budget stays `unknown` unless the agent reports counts.

When a cap is reached the CLI writes a pause record, refuses further phase and step commands, and exits non-zero. `harness continue` requires an evaluation note, stores it in the pause record, and extends the tripped budget by one more window. There is no way to resume without that evaluation.

Exit codes:

- `0` success.
- `2` usage or environment error, including no harness root found.
- `3` budget pause, or a command refused because the run is paused.
- `4` phase-order violation.

Run state lives under `.harness-db/runs/<run-id>/` in the harness root and is ignored by git:

- `state`: `KEY=VALUE` counters, caps, and phase status.
- `run.json`: machine-readable snapshot of the same run.
- `pauses/NNN.json`: one record per budget pause, including the evaluation note once resolved.
- `log`: append-only record of the commands the run accepted.

Override the state directory with `HARNESS_DB_ROOT`, which is how the regression tests keep runs isolated.

`scripts/verify.sh` automatically detects common Make, JavaScript/TypeScript, PHP, Go, Rust, and Bash commands. It runs available checks and skips missing checks clearly.

Projects can require verification categories by adding `.harness-required-checks` at the target root. Use one or more of `format`, `lint`, `typecheck`, `test`, and `build`, separated by whitespace or lines. A required category fails verification when it runs no checks. `HARNESS_REQUIRED_CHECKS` overrides the file for temporary or CI-specific requirements.

Detection order:

- Make targets override generic detection when targets such as `format`, `lint`, `typecheck`, `test`, or `build` exist.
- JavaScript/TypeScript projects use matching `package.json` scripts through `pnpm`, `yarn`, or `npm`.
- PHP projects use Composer scripts such as `lint`, `analyse`/`analyze`, `phpstan`, `psalm`, `test`, and `build`.
- Go projects use `gofmt`, `go vet`, `go test`, and `go build`.
- Rust projects use `cargo fmt --check`, `cargo clippy`, `cargo check`, `cargo test`, and `cargo build`.
- Bash/shell files use `sh -n`; `shellcheck` runs when available.

Manual command placeholders:

```sh
make test
make lint
make typecheck
make build
```

or:

```sh
npm test
npm run lint
npm run typecheck
npm run build
```

or:

```sh
composer run-script test
go test ./...
cargo test --all-targets --all-features
sh -n scripts/*.sh
```

Replace these placeholders with exact project commands when tooling is added.
