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

The bootstrap script checks required tools, then detects project-owned setup commands (`make init` or `make setup`, `npm`/`pnpm`/`yarn install`, `composer install`, `go mod download`, `cargo fetch`). It prints that list and runs it only after you answer `y`, or when `--yes` or `HARNESS_INIT_YES=1` is given. Without a terminal and without `--yes` it refuses with exit `3`, so nothing from an unfamiliar repository runs by accident. CI passes `--yes`.

To bootstrap a separate target project directory from this harness:

```sh
scripts/init.sh --project /path/to/project
```

Or:

```sh
HARNESS_TARGET_ROOT=/path/to/project scripts/init.sh
```

### Automatic initialisation at session start

`scripts/install-hooks.sh` registers `scripts/hooks/auto-init.sh` as a Claude Code and Codex `SessionStart` hook (`startup`, `resume`, and `clear`). Every session then initialises the project it starts in, whether that is a linked git worktree, a main checkout, or a plain directory:

1. The project root (the git toplevel when inside a repository, otherwise the directory itself) is registered as a harness target with `scripts/harness-target.sh register`. Registration lives under `HARNESS_DB_ROOT/targets/<name>-<hash>/` and never writes inside the project, so tracked files stay clean. The home directory, `/`, and the harness root are never registered.
2. `scripts/init.sh --project ROOT --auto` runs the detected project-owned setup commands non-interactively, but only when the fingerprint of the bootstrap inputs (`Makefile`, `package.json`, lockfiles, `composer.json`, `composer.lock`, `go.mod`, `go.sum`, `Cargo.toml`, `Cargo.lock`) differs from the last successful run for that target. The commands run in the background with their output in `targets/<id>/bootstrap.log`; `targets/<id>/bootstrap.state` records `STATUS` (`running`, `ok`, `failed`), `FINGERPRINT`, `EXIT`, and `PID`. A failed bootstrap is reported on later starts but not retried until the inputs change or someone runs `scripts/init.sh --project ROOT --yes`.
3. The same payload is handed to `scripts/hooks/session-route.sh`, so the shadow task-entry route is recorded for the registered target.

The hook prints one `Harness auto-init:` context line naming the target, its registry directory, and the bootstrap outcome. When it says the bootstrap is running, wait for the log to finish before running project commands that need dependencies. `HARNESS_AUTO_INIT=0` disables the hook; `HARNESS_AUTO_INIT_SYNC=1` runs the bootstrap in the foreground (the regression test uses this).

Once registered, `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`, and the Jev hooks resolve the target from the current directory or `--project` and use its private database at `targets/<id>/db/`: runs, `runs/current`, gate records, routes, advice, and pending oracles are per target, so parallel worktrees never share one active run. `scripts/harness-target.sh list` shows the registered targets. Manual `scripts/init.sh --project PATH` registers the target as well.

Detected bootstrap inputs:

- `Makefile`: runs `make init` or `make setup` when either target exists.
- `package.json`: installs JavaScript/TypeScript dependencies with `pnpm install`, `yarn install` (`--frozen-lockfile` when locked), or `npm ci` (`npm install` without a lockfile).
- `composer.json`: installs PHP dependencies with `composer install --no-interaction`, adding `--prefer-dist` when locked.
- `go.mod`: downloads Go modules with `go mod download`.
- `Cargo.toml`: fetches Rust dependencies with `cargo fetch`, adding `--locked` when `Cargo.lock` exists.

## Environment Variables

No required environment variables are currently known.

Optional harness variables:

- `HARNESS_TARGET_ROOT`: target project directory for `scripts/init.sh`, `scripts/verify.sh`, and `scripts/review.sh` when `--project` is not passed.
- `HARNESS_INIT_YES`: set to `1` to let `scripts/init.sh` run its previewed project-owned setup commands non-interactively.
- `HARNESS_AUTO_INIT`: set to `0` to make the `SessionStart` auto-init hook exit without registering or bootstrapping; default on.
- `HARNESS_AUTO_INIT_SYNC`: set to `1` to run the automatic bootstrap in the foreground instead of the background; used by tests.
- `HARNESS_ROOT`: harness root for `scripts/harness` when automatic discovery should be skipped.
- `HARNESS_DB_ROOT`: harness database root for `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`, and the hooks. Defaults to `HARNESS_ROOT/.harness-db`. Registered targets keep their own state beneath it at `targets/<id>/db/`.
- `HARNESS_BUDGET_STEPS`, `HARNESS_BUDGET_TIME_MIN`, `HARNESS_BUDGET_LOOPS`, `HARNESS_BUDGET_TOKENS`: session budget caps read when a `scripts/harness` run is created.
- `HARNESS_BUDGET_TOKENS` is an explicit run cap. A direct `codex` CLI launch with this value is refused because a separate App Server cannot interrupt that CLI-owned turn. Leave it unset or `unknown` for the existing unmetered launch path.
- `HARNESS_BUDGET_CONTINUES`: maximum human continuations allowed for a run; defaults to `3`.
- `HARNESS_REQUIRED_CHECKS`: whitespace-separated verification categories (`format`, `lint`, `typecheck`, `test`, `build`); it overrides `.harness-required-checks` for a temporary or CI-specific requirement.
- `HARNESS_TYPESAFE_ROUTER`: path to the TypeSafe router used by `route`, `launch`, and `advise`; it defaults to the installed TypeSafe skill.
- `HARNESS_TYPESAFE_ACTIVE`: set to `1` only after the active-routing outcome gate is satisfied and reviewed.
- `HARNESS_TYPESAFE_ROLLOUT_PERCENT`: optional integer from `0` to `100`; in active mode, only that percentage of eligible tasks follows JEV's live recommendation. The cohort is stable as the percentage increases; default `100`.
- `HARNESS_TYPESAFE_OPERATOR_ACTIVATION`: set to `1` only when the operator explicitly accepts unvalidated full activation. It bypasses the 30-outcome evidence gate but never bypasses deterministic safety gates; every route record marks `operator_activation=true`.
- `HARNESS_AUDIT_ENABLED`, `HARNESS_AUDIT_URL`: enable best-effort Arc audit emission and select its local URL; audit failure never blocks routing or agent execution.
- `HARNESS_JEV_CHECKPOINTS`: set to `1` to let phase gates, `verify.sh`, `review.sh`, and the Jev hooks emit automatic shadow checkpoints and labels. Unset or `0` makes every automatic checkpoint a silent no-op; `verify.sh` forces `0` for nested harness tests.
- `HARNESS_JEV_TIMEOUT`, `HARNESS_JEV_ATTEMPTS`: per-attempt timeout in seconds and attempt count for `advise` calls; automatic checkpoints default to `10` and `1` so gates and hooks stay bounded.
- `HARNESS_JEV_REPEAT_THRESHOLD`: how many identical shell commands in one run trigger the hook's `progress_assessment` checkpoint; default `3`.
- `TYPESAFE_HOME` or `TYPESAFE_LOG_DIR`: optional TypeSafe outcome-log location used when checking eligibility for active routing.

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

The review script runs verification in the target project root, then prints the full patch: status, staged diff, unstaged diff, and every untracked file as a new-file diff, for the target and separately for the harness when they differ. It keeps going when verification fails, so the reviewer still sees the change, and exits with the verification status. Its record carries `VERIFY_EXIT`, and `scripts/harness review done` accepts it only when that is `0`.

To validate a proposed action JSON file:

```sh
scripts/action.sh validate PATH
```

The validator accepts or rejects the file against `schemas/action.schema.json`, then checks it against the denylist (see below) and exits `3` when denied. It does not execute the action. `python3` is used when available; otherwise `node`. One of those runtimes is required.

To run the harness regression tests:

```sh
for t in tests/*.sh; do sh "$t"; done
```

`scripts/verify.sh` runs the same set under its `harness:tests` check.

## Denylist

`scripts/permit.sh` holds the allow-unless-denied rules. `schemas/denylist.default` is the harness default; a project replaces it entirely by adding `.harness-denylist` at its root. Each line is `command <regex>` (matched against the whole shell command) or `path <regex>` (matched against a write target, relative to the project root when inside it). The default denies destroying root, home, or `.git`, privilege escalation, piping downloads into a shell, force pushes and history rewrites, publishing, and any edit to the guard itself: `.claude/settings.json`, `scripts/hooks/`, `scripts/permit.sh`, `schemas/denylist.default`, `.harness-denylist`, and the trust and gate records.

```sh
scripts/permit.sh check --command "git push --force"     # exit 1, DENY with the rule
scripts/permit.sh check --path .env                      # exit 1
scripts/permit.sh rules                                  # print the effective rules
sh tests/permit.sh
```

`scripts/action.sh validate` runs this check after schema validation. The phase guard hook runs it on every real Write, Edit, and Bash call, so the check and the action are the same event.

## Knowledge Trust

A `knowledge/` folder inside a project can carry instructions and hooks, so nothing in it is followed until a human approves its exact content once per machine. `scripts/knowledge-trust.sh check` exits `1` while the folder is unapproved or changed since approval; the phase guard hook blocks every tool call in that state. Approval is human-only: the hook refuses `approve` from the agent.

```sh
scripts/knowledge-trust.sh status  --project /path/to/project
scripts/knowledge-trust.sh approve --project /path/to/project   # run by a person, after reading the folder
sh tests/knowledge-trust.sh
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
scripts/harness route --state /path/to/task-metadata.json --project /path/to/project
scripts/harness launch --state /path/to/task-metadata.json --project /path/to/project --agent codex
scripts/harness advise --context /path/to/decision-context.json
scripts/harness budget --thread THREAD_ID --tokens 40000 --watch
scripts/harness continue "<evaluation note>"
scripts/harness abort "<reason>"
```

### Task-entry routing

`harness route` and `harness launch` run before an agent starts. They require Python 3. The route input is a compact metadata object with fixed enum fields, not the user's prompt or repository content. Example:

```json
{
  "version": 1,
  "task_kind": "change",
  "area": "mobile",
  "proposed_action": "start_routine_agent",
  "reversibility": "reversible",
  "uncertainty_reason": "test_gap",
  "diff_size": "small",
  "changed_file_count": 2,
  "known_failures": 0,
  "required_checks_pending": false
}
```

Allowed `task_kind`: `change`, `bug`, `review`, `research`, `question`, `ops`. Allowed `area`: `mobile`, `frontend`, `backend`, `infrastructure`, `docs`, `other`. Allowed `proposed_action`: `start_routine_agent`, `run_targeted_check`, `start_deep_agent`, `ask_for_missing_input`. Allowed `uncertainty_reason`: `none`, `scope_unclear`, `test_gap`, `unknown_dependency`, `conflicting_evidence`, `other`. `reversibility` is `reversible` or `irreversible`; `diff_size` is `none`, `small`, `medium`, or `large`. Optional booleans are `approval_required` and `user_choice_explicit`. Unknown fields, including `prompt`, are rejected.

For a direct dry run, save that object to a JSON file and run:

```sh
scripts/harness route --state /tmp/task-metadata.json --project /path/to/project
scripts/harness launch --state /tmp/task-metadata.json --project /path/to/project --agent codex --dry-run
```

`launch` can also take `--default-command`, `--routine-command`, `--targeted-command`, and `--deep-command`. Each command file contains one JSON argv array, such as `["codex", "--model", "<configured-routine-model>"]`. Without a matching profile, it uses the default command. It executes the chosen argv directly, never through a shell. Use command files to call an existing Herdr or other launcher when that is the established start path. Sessions started directly in a terminal or Herdr bypass this intake route; the launcher must be the entrypoint for token savings at task start.

Deterministic conditions bypass TypeSafe: required checks, known failures, explicit user choices, authorization, irreversible actions, and tasks with no route ambiguity. Ambiguous reversible tasks use the TypeSafe routing skill at `~/.agents/skills/typesafe-routing/scripts/route.py` (override with `HARNESS_TYPESAFE_ROUTER`). The call uses `--strict`, so any detected secret cancels the API request. Missing credentials or service errors fall back to the default command and are recorded.

The default mode is `shadow`: TypeSafe answers and logs its judgment while `launch` keeps the existing default command. Route records are private files under `.harness-db/routes/` (or `HARNESS_DB_ROOT/routes/`). The TypeSafe skill records its own calls under `~/.typesafe-routing/logs/`. Record each real outcome using the skill's `route.py record --call-id ...` command and inspect `route.py report` for accuracy, token usage, and latency. The harness cannot measure avoided reasoning tokens itself.

Active mode requires an exact `TYPESAFE_MODEL` pin and current-cohort evidence (matching requested/returned model and the fingerprint of the router plus task adapter). Unversioned records, model probes, advice, and other policy/model cohorts do not qualify. Active mode requires `--mode active`, `HARNESS_TYPESAFE_ACTIVE=1`, at least 30 distinct correct shadow outcomes (including five correct `proceed` routes), and zero `under_escalated` outcomes. The route and outcome logs are joined by call ID; model checks and fabricated unpaired outcomes do not count. After activation, raise `HARNESS_TYPESAFE_ROLLOUT_PERCENT` in deliberate stages such as `10`, `25`, `50`, and `100`; a stable metadata hash keeps tasks in the same holdback or delegated cohort as the percentage rises. Holdback tasks still call JEV in shadow mode and keep the normal command, so each route record exposes `routing_mode`, `rollout_percent`, `rollout_bucket`, and `rollout_selected` for comparison. Review correctness and paired agent-token measurements before each increase. Required verification and permission gates are unchanged.

Recommended staged activation:

```sh
export TYPESAFE_MODEL=jev-1.13.0
export HARNESS_TYPESAFE_ACTIVE=1
export HARNESS_TYPESAFE_ROLLOUT_PERCENT=10
scripts/harness launch --mode active --state TASK.json --agent codex \
  --routine-command routine.json --targeted-command targeted.json --deep-command deep.json
```

Increase the percentage only after the route report shows no under-escalation and paired measurements show that JEV plus the selected agent costs less than the normal path. Set the percentage to `0` for an immediate active-mode holdback, or unset `HARNESS_TYPESAFE_ACTIVE` to disable active mode entirely.

### Arc audit and full operator activation

The prepared Arc project is now implemented and versioned at `services/harness-audit/`. It records immutable `jev.route`, `jev.outcome`, `agent.completed`, and `agent.token_usage` events and exposes a projection-backed summary:

```sh
scripts/audit-service.sh setup       # first time only
scripts/audit-service.sh install     # systemd user service or macOS launchd
curl http://127.0.0.1:18080/health
curl http://127.0.0.1:18080/api/audit/summary
curl http://127.0.0.1:18080/api/audit/summary/checkpoints   # per family/version/model; add ?include_fixtures=true to keep test fixtures
```

Each machine keeps its own SQLite database; to pool telemetry, point another host's `HARNESS_AUDIT_URL` at one service over the tailnet. See `services/harness-audit/README.md`.

`scripts/jev-enable.sh` is sourced from the login shell and now enables **shadow** collection only: the model pin, `HARNESS_JEV_CHECKPOINTS=1`, and Arc telemetry. It deliberately unsets `HARNESS_TYPESAFE_ACTIVE` and `HARNESS_TYPESAFE_OPERATOR_ACTIVATION` left over from earlier sessions (`JEV_KEEP_ACTIVATION=1` preserves them for a deliberate active run). Activation is earned: export those variables by hand only after `scripts/harness advise --report` shows the labeled pilot batch and the task-entry gate above is satisfied.

```sh
. scripts/jev-enable.sh
scripts/harness advise --report | python3 -m json.tool | sed -n '/"pilot"/,/}/p'
```

The pilot summary is machine-wide by default (every registered target's checkpoints, with a per-target `by_target` breakdown); pass `--scope target` to see only the current worktree's database. `harness review done` prints the same machine-wide counter.

The explicit operator flag is an activation acknowledgement, not accuracy evidence. The launcher automatically records an `unknown` outcome after completion; review can later replace that unresolved label with `correct`, `over_escalated`, or `under_escalated` using the TypeSafe recorder. Agent token counts are emitted when supplied by `HARNESS_AGENT_*_TOKENS` or by the App Server `harness budget --watch` meter; missing measurements remain explicitly marked `missing`.

### Codex token-budget meter

`harness budget` uses the local Codex App Server goal APIs to read the persisted `tokensUsed` counter. With `--watch`, each increase is recorded through `harness step --tokens`; when usage reaches the chosen cap, the controller looks up the active turn and calls `turn/interrupt` with both its thread and turn IDs. The command exits `3` when the cap is reached, matching the harness budget-pause exit code.

Pass an existing thread directly:

```sh
scripts/harness budget --thread THREAD_ID --tokens 40000 --watch
```

This command requires a running Codex App Server daemon and a thread owned by that server; it cannot interrupt a thread running inside an independent `codex` terminal process. On this machine, `codex app-server proxy` currently has no daemon to connect to, and `codex app-server daemon start` requires a managed standalone Codex installation that is not present. Until that dependency and an App Server-owned launch path are available, use the command only with an already managed thread. `harness launch --agent codex` refuses an explicit token cap instead of silently running without enforcement.

Attaching to a thread with an active goal updates its cap without replacing its objective or resetting its usage. A terminal goal is rejected so the caller must deliberately create a new goal instead of losing the old accounting.

### Auditability planning wizard

Open `tools/auditability-planning-wizard.html` directly in a browser. The standalone page asks ten required questions covering the first pilot, Arc event design, prompt privacy, JEV shadow routing, review projections, cross-machine delivery, operations, and the evidence required before rollout. It has no network dependency or build step.

Draft answers stay in browser `localStorage` until cleared. Do not enter raw prompts, source, credentials, or personal data. After generating the brief, download either the human-readable Markdown plan or the structured JSON answers.

### Dynamic advice during work

`harness advise` is for a genuine decision that arises while an agent is working. Unlike task-entry routing, the agent supplies a small, situation-specific set of options. TypeSafe selects one option and returns a confidence score; the harness writes a private record under `.harness-db/advice/` and does not execute the selection. This is advisory only: permissions, destructive-action safeguards, known failures, required checks, and verification remain deterministic and cannot be waived.

The context is deliberately concise and must not contain raw prompts, source files, diffs, credentials, personal data, or secrets. It is rejected before the API request if the strict redaction scan finds sensitive material. The JSON format is:

```json
{
  "version": 1,
  "decision": {
    "question": "For this ledger feature, which persistence approach is the better fit?"
  },
  "context": {
    "goal": "Keep banking transfers, reversals, balances, and audit history correct.",
    "facts": ["The ledger is append-only."],
    "constraints": ["Financial correctness is mandatory."],
    "risks": ["An incorrect design can weaken auditability."]
  },
  "options": [
    {"id": "crud", "description": "CRUD tables with an explicit audit-log design."},
    {"id": "event_sourcing", "description": "Events are the ledger source of truth."}
  ]
}
```

Run it with:

```sh
scripts/harness advise --context /path/to/decision-context.json
```

The output contains `choice`, `confidence`, `call_id`, and `record_path`. Treat `choice` as evidence for the ongoing judgment, not an instruction to bypass a rule or automatically change code.

Five unrelated v1 fixtures and one v2 Choice/Score/Boolean batch exercise this path (banking ledger, clinic appointments, offline field work, storefront search, and staff authentication). They are opt-in because they require the local TypeSafe credential and network access:

```sh
HARNESS_TYPESAFE_LIVE=1 sh tests/live-context-advice.sh
```

The ordinary harness test run invokes the wrapper without that variable and reports a skip, so CI never requires the credential. The fixtures are test-only; their database and TypeSafe logs are isolated so they cannot count as pilot evidence. `scripts/context_advice.py` has no domain-specific options or terms.

Phase rules:

- `build start` fails until `plan done` has run.
- `review start` fails until `build done` has run.
- `PHASE done` fails unless that phase is active.
- `build done` fails unless `.harness-db/records/verify.state` exists, was written after `build start`, and reports `EXIT=0`. `scripts/verify.sh` writes that record on every run, pass or fail.
- `review done` fails unless `.harness-db/records/review.state` was written after `review start`. `scripts/review.sh` writes it when it reaches the end.
- Re-starting a phase that is already done counts against the loop budget.
- After `review done` or `abort`, only `plan start` is accepted; it opens a new run.

Session budgets are counted per run and are agent-visible rules:

| Budget | Default cap | Counted by |
|---|---|---|
| `steps` | 200 | every phase command, every `harness step`, and every tool call the hook counts |
| `time_min` | disabled | Optional wall-clock cap; set `HARNESS_BUDGET_TIME_MIN` to a non-negative minute value only for a deliberately time-boxed run |
| `loops` | 1 | re-entering a phase that was already marked done |
| `tokens` | unknown | Only what `harness step --tokens N` reports; the harness does not receive model usage automatically |
| `continues` | 3 | every accepted `harness continue` in the run |

Set caps with `HARNESS_BUDGET_STEPS`, `HARNESS_BUDGET_TIME_MIN`, `HARNESS_BUDGET_LOOPS`, and `HARNESS_BUDGET_TOKENS`. They are read when the run is created. The CLI cannot count tokens itself, so the token budget stays `unknown` unless the agent reports counts. A token cap is therefore enforced only when the agent launcher or runtime reports its measured token use through `harness step --tokens N`; the current Claude hook records tool steps but receives no token-usage field.

When a cap is reached the CLI writes a pause record, refuses further phase and step commands, and exits non-zero. `harness continue` requires an evaluation note, stores it in the pause record, and extends the tripped budget by one more window. There is no way to resume without that evaluation. An agent may invoke it only after the user explicitly instructs continuation in the current conversation; the note must record that authorization concisely, such as `User explicitly requested continuation in chat.` Once the `continues` cap is reached, `continue` is refused and the only way forward is `harness abort "<reason>"` followed by `harness plan start`.

A `continue` on the time budget restarts the clock as well as adding a window, so a run left overnight resumes cleanly. The phase guard hook allows `continue`; it cannot inspect chat authorization, so agents must follow the current-conversation policy above. `abort` and `scripts/knowledge-trust.sh approve` remain human-only and the hook refuses them when an agent issues them through the Bash tool. Set `HARNESS_BUDGET_CONTINUES` to change the cap.

Exit codes:

- `0` success.
- `2` usage or environment error, including no harness root found.
- `3` budget pause, or a command refused because the run is paused.
- `4` phase-order or gate violation.

Run state lives under `.harness-db/runs/<run-id>/` in the harness root and is ignored by git:

- `state`: `KEY=VALUE` counters, caps, and phase status.
- `run.json`: machine-readable snapshot of the same run.
- `pauses/NNN.json`: one record per budget pause, including the evaluation note once resolved.
- `log`: append-only record of the commands the run accepted.
- `abort.note`: the reason given to `harness abort`, when the run was aborted.

Gate records live under `.harness-db/records/` (or `targets/<id>/db/records/` for a registered target):

- `verify.state`: `KEY=VALUE` written by `scripts/verify.sh` with `RECORD_EPOCH`, `GIT_HEAD`, `GIT_DIRTY_FILES`, `RAN`, `SKIPPED`, `FAILURES`, and `EXIT`.
- `review.state`: written by `scripts/review.sh` at the end of a review.

Override the state directory with `HARNESS_DB_ROOT`, which is how the regression tests keep runs isolated.

### Semantic retrieval with jevgrep

[jevgrep](https://github.com/dzhng/jevgrep) (`jg`) answers a natural-language question about a repository with a summary, a ranked file list, and verbatim excerpts with line references, evaluated by Jev. It is Jev used for retrieval, where the rest of the harness uses Jev for bounded decisions. Unlike every other Jev path in the harness, `jg` uploads eligible source of the searched tree to the provider chosen with `jg auth` (Vercel AI Gateway, TypeSafe, OpenRouter, or OpenCode Zen), so it is opt-in per target and always goes through the wrapper:

```sh
npm install --global @dzhng/jevgrep@latest   # Node 22+
jg auth                                       # interactive; the user runs this, never the agent
jg doctor                                     # confirms the saved provider and connectivity
scripts/jg.sh --project /path/to/project "Where is authentication checked before a request reaches a handler?"
scripts/jg.sh --project /path/to/project --root src "Which tests cover retry behaviour?" --no-cache
scripts/jg.sh --project /path/to/project --report
```

`scripts/jg.sh` resolves the target root (default: the current directory, or `--project`), searches it or the `--root SUBDIR` beneath it, streams `jg`'s output unchanged, and exits with `jg`'s status (`0` complete, `1` failed, `2` incomplete). It refuses, with exit `4`, the upload-widening options `--include-sensitive` and `--no-ignore` (the default denylist rejects them for a direct `jg` call as well) and any target whose root contains a `.harness-no-upload` marker. Exit `2` means usage or a missing `jg`. Every completed search writes `retrieval/<stamp>.state` under the target's private database with the time, run id, current phase, a sha256 of the question, duration, exit code, output size, and a completeness flag; the question text, paths, excerpts, and source are never stored. `--report` summarises those records, and `harness plan done` includes "semantic retrieval used in this run" among its checkpoint facts. Credentials live in `~/.config/jevgrep/credentials.json`; environment overrides are ignored by `jg`, and there is no per-search provider switch.

Interactive sessions learn about the wrapper from `scripts/install-jg-skill.sh`, which is what makes retrieval reachable outside a project that carries the guide block:

```sh
scripts/install-jg-skill.sh                 # refresh both skills and the two global guides
scripts/install-jg-skill.sh --no-upstream   # refresh only the harness block in existing skills
scripts/install-jg-skill.sh --home /tmp/h --source /path/to/SKILL.md --skip-global
```

It copies the upstream skill (`skills/jevgrep/SKILL.md` from the package next to the installed `jg`, or `--source`) verbatim to `~/.claude/skills/jevgrep/SKILL.md` and `~/.agents/skills/jevgrep/SKILL.md` and appends a marked `harness-jg` block that tells the agent to search through `scripts/jg.sh` instead of a bare `jg` inside any harness target. When `~/.claude/CLAUDE.md` or `~/.codex/AGENTS.md` carries the `global-harness` block, the same marked paragraph is inserted before that block's end marker, so the instruction reaches every session on the machine without editing project-owned guides. Reruns are idempotent; run it again after `jg skill --global` or a package upgrade, since those overwrite the skill files.

## Agent Guide Block

The guide block is optional: automatic initialisation recognises a target through the registry, not through this block. `make install-guides` (or `scripts/install-guides.sh --project PATH`) adds a marked "Harness Phases" block to `AGENTS.md` and `CLAUDE.md` in the target project, creating the files when missing. Rerunning refreshes the block in place between `<!-- harness-cli:start -->` and `<!-- harness-cli:end -->` and leaves everything else untouched. For a project outside the harness root the block carries `HARNESS_ROOT=... /path/to/harness/scripts/harness` so the CLI can find its state.

```sh
make install-guides
make install-guides PROJECT=/path/to/project
sh tests/install-guides.sh
```

The `Makefile` deliberately has no `format`, `lint`, `typecheck`, `test`, or `build` targets, so `scripts/verify.sh` detection is unchanged.

## Phase Guard Hook

`.claude/settings.json` registers `scripts/hooks/require-phase.sh` as a Claude Code `PreToolUse` hook for `Write`, `Edit`, `MultiEdit`, `NotebookEdit`, and `Bash`. The hook asks `scripts/harness status` for the run state and blocks the tool call (exit 2) unless a phase is active and the run is not paused, complete, or aborted. Bash calls whose whole command is `scripts/harness ...` or `scripts/action.sh validate ...` are allowed so the agent can open a phase; a chained command such as `scripts/harness plan start; rm -rf build` is not. The hook permits `scripts/harness continue` because it cannot inspect chat context; agents may use it only after an explicit current-conversation user instruction and must retain the required evaluation note. It continues to refuse agent-issued `scripts/harness abort` and `scripts/knowledge-trust.sh approve`.

Before the phase check, the hook applies the denylist to the actual command or write path and refuses to work while a `knowledge/` folder is unapproved. Every allowed call is then recorded with `scripts/harness step --note "tool:NAME"`, so the step budget counts real tool calls instead of self-reports. Use `HARNESS_BUDGET_TIME_MIN=<n>` only for a deliberately time-boxed run. Tokens stay `unknown` because the hook payload carries no token counts.

A person can switch the hook off for one session by exporting `HARNESS_HOOK_DISABLE=1` in the environment Claude Code starts from. The denylist refuses that string inside agent commands, so the agent cannot do it for itself.

The hook is enforcement for Claude Code only. Other agents still rely on the written rules. Run its regression test with:

```sh
sh tests/harness-hook.sh
```

`scripts/verify.sh` automatically detects common Make, JavaScript/TypeScript, PHP, Go, Rust, and Bash commands. It runs available checks and skips missing checks clearly, and it never runs a command that rewrites files: only `format-check`, `fmt-check`, `check-format` Make targets and `format:check` or `prettier:check` scripts are used, and a plain `format` target or script is reported as a skip. When the project being verified is this harness itself (it has `scripts/harness` and `tests/*.sh`), the `harness:tests` check runs every script in `tests/`. Each run ends by writing `.harness-db/records/verify.state`, which `scripts/harness build done` requires.

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

### Broad Jev checkpoints

See [Jev decision checkpoints](jev-checkpoints.md) for version 2 batched Choice/Score/Boolean evaluation, baseline capture, deterministic bypasses, outcome recording, and cohort reports. Version 1 successful single-choice output remains compatible. Invalid or unavailable evaluations now return a structured fallback without executing a recommendation. Use `scripts/harness advise --report` without credentials; use `--record OUTCOME.json` or `--label CALL_ID ...` to attach independent labels, `--pending` to list what still needs one (`--scope machine` covers every target; `--report` defaults to machine scope), and `--family ...` for the flag form. New families are shadow-only.

With `HARNESS_JEV_CHECKPOINTS=1`, `harness plan done`, `harness build start`, `scripts/verify.sh`, and `scripts/review.sh` emit their own shadow checkpoints and label them from the verification exit code and the run's loop count; `harness review done` prints the pilot counter. The checkpoints add one bounded API call per seam and never change a gate result.

### Jev hooks for interactive sessions

Interactive Claude Code and Codex sessions bypass `harness launch`, so two additive hooks cover them. `scripts/hooks/session-route.sh` (SessionStart) records one shadow task-entry route when the working directory is a harness target, using only enum metadata derived from git and harness state, and prints one context line. `scripts/hooks/jev-observe.sh` (PreToolUse for Bash) keeps a checksum count of commands in the active run and emits one `progress_assessment` checkpoint on the third identical command; it stores no command text and always exits 0. Install or remove both entries with:

```sh
scripts/install-hooks.sh            # ~/.claude/settings.json and ~/.codex/hooks.json, with backups
scripts/install-hooks.sh --dry-run
scripts/install-hooks.sh --uninstall
```

The installer only touches hook groups whose command points at these two scripts. Neither hook replaces `scripts/hooks/require-phase.sh`, which remains the only enforcement hook.
