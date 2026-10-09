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

Once registered, harness commands use the target database at `targets/<id>/db/`. Run directories and aggregate routing history remain per target, but each native session owns its current pointer at `runs/sessions/<sha256-session-id>/current` and its check records at `records/sessions/<sha256-session-id>/`. Session selection uses `HARNESS_SESSION_ID`, then `CODEX_THREAD_ID`, then `CLAUDE_SESSION_ID`. Commands may instead use `harness --session-id ID COMMAND`. A new session never adopts the legacy directory-wide run. Resume, clear, and compaction with the same native ID preserve the run, phase, budget, and pause. Completing review closes that run; the next `plan start` in the same session creates a fresh run. Genuine pauses within a session still require explicit user continuation.

Without a native identity, manual commands retain `runs/current` and `records/` for compatibility. Set `HARNESS_SESSION_ID` consistently on phase commands, verification, review, and child processes when working manually across separate terminals. Existing legacy runs remain available to manual commands and are neither reset nor aborted. Check records include their run ID; an earlier run's record cannot satisfy a new session run's gate.

The installed SessionStart adapter binds the payload identity before bootstrap and routing, and writes the native environment file independently of audit collection. Terminal commands use their native `CODEX_THREAD_ID`. The installed command observer selects the payload session's run. Run `scripts/install-hooks.sh` after updating to install these adapters; existing unrelated hook groups are preserved. New native sessions load the updated hook configuration; already running sessions keep their loaded hooks. `scripts/harness-target.sh list` shows registered targets.

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
- `HARNESS_SESSION_ID`: explicit run identity, ahead of `CODEX_THREAD_ID`, `CLAUDE_SESSION_ID`, and `CLAUDE_CODE_SESSION_ID`; hashed for on-disk pointer and record paths. Native resumes preserve identity.
- `HARNESS_BUDGET_STEPS`, `HARNESS_BUDGET_TIME_MIN`, `HARNESS_BUDGET_LOOPS`, `HARNESS_BUDGET_TOKENS`: session budget caps read when a `scripts/harness` run is created.
- `HARNESS_BUDGET_TOKENS` is an explicit run cap. A direct `codex` CLI launch with this value is refused because a separate App Server cannot interrupt that CLI-owned turn. Leave it unset or `unknown` for the existing unmetered launch path.
- `HARNESS_BUDGET_CONTINUES`: maximum human continuations allowed for a run; defaults to `3`.
- `HARNESS_BUDGET_REPEAT_FAILURES`: identical failures of one command in a row before the run pauses; default `2`, `0` disables.
- `HARNESS_RUN_IDLE_HOURS`: idle hours before a run is stale and refused; default `24`, `0` disables.
- `HARNESS_RETAIN_RUNS`: runs kept per database before `plan start` archives older finished ones; default `50`.
- `HARNESS_CONFIRM_TTY`: the terminal `harness review submit` reads its confirmation from; default `/dev/tty`. Tests point it at a file; agent commands may not assign it.
- `HARNESS_REVIEW_BASE`: commit the review packet's committed diff starts from; default the run's `CONTRACT_BASE`.
- `HARNESS_CRITERION_TIMEOUT`: seconds each contract acceptance command may run at `build done`; default `900`.
- `HARNESS_HOME`: the installed harness root for the guard-version check; defaults to the path `scripts/install-hooks.sh` records in `~/.config/harness/root`.
- `HARNESS_REQUIRED_CHECKS`: whitespace-separated verification categories (`format`, `lint`, `typecheck`, `test`, `build`); it overrides `.harness-required-checks` for a temporary or CI-specific requirement.
- `HARNESS_VERIFY_SCOPE=full`: disables the docs-only scope and runs every verification category.
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

`scripts/permit.sh` holds the allow-unless-denied rules; with `python3` it runs `scripts/permit.py`. `schemas/denylist.default` is the harness default; a project replaces it entirely by adding `.harness-denylist` at its root. Each line is one of:

- `command <regex>`: matched against the whole shell command, for destructive actions: destroying root, home, or `.git`, privilege escalation, piping downloads into a shell, force pushes and history rewrites, and publishing. Decisions that are not the agent's are refused on the parsed command, wherever they appear and however they are quoted: `harness abort`, `harness review submit`, `harness failure`, `harness launch`, `knowledge-trust.sh approve`, and setting the guard's own variables by assignment, `env`, or `export` (`HARNESS_HOOK_DISABLE`, `HARNESS_BUDGET_*`, `HARNESS_RUN_IDLE_HOURS`, `HARNESS_RETAIN_RUNS`, `HARNESS_HOME`, `HARNESS_DB_ROOT`, `HARNESS_REQUIRED_CHECKS`, `HARNESS_REVIEWER_CMD`, `HARNESS_REVIEW_BASE`, `HARNESS_CONFIRM_TTY`), whether by assignment, `+=`, `export`, `env`, `read`, `printf -v`, a `for` variable, a nameref, or `${NAME:=...}`. A command word that is a glob, a variable, `{}`, or an indirect runner (`script`, `flock`, `parallel`, ...) carrying a human-only subcommand is refused too. The default rules repeat the plain forms as text patterns.
- `path <regex>`: matched against a write target, relative to the project root when inside it and absolute otherwise. It protects git internals, secrets, the guard itself (`.claude/settings.json`, `scripts/hooks/`, `scripts/permit.sh`, `scripts/permit.py`, `scripts/guard-version`, `schemas/denylist.default`, `.harness-denylist`), and the whole harness database `.harness-db/` (runs, contracts, failure counts, gate records), which only the harness scripts write.
- `inline <regex>`: matched against inline interpreter code (`python -c`, `node -e`, heredocs fed to an interpreter, ...).

Shell commands are parsed, not pattern-matched for paths. `permit.py` strips comments, follows `cd`/`pushd`/`popd`, `env -C`, `sh -c` (and `-ec`, `-xc`, ...), `eval`, command substitutions, and heredocs or here-strings fed to a shell, peels wrappers (`nice`, `timeout`, `env`, `command`, ...), and collects every file the command would write: redirections, `tee`, `cp`/`mv`/`install`/`ln`, `rm`/`rmdir`, `touch`, `mkdir`, `truncate`, `chmod`/`chown`, `dd of=`, `sed -i`, `perl -i`, `find -delete`/`-exec`, `git rm`/`mv`/`checkout`/`restore`, and `rsync`. Each target is resolved against the working directory and symlinks. Deleting, moving, or changing the mode of a directory is judged against every protected path beneath it; a glob or brace list against every protected path it could match; a target built from a variable (`"$D/scripts/hooks/x"`) by its literal suffix. Commands whose writes cannot be known (`xargs` into a writer, `tar`, `patch`, an interpreter run by `find -exec`) and commands that cannot be parsed are refused when any word in them names a protected path. Reading or naming a protected file is allowed; writing it is not. Write and Edit paths are resolved (`..`, `//`, symlinks) before the rules see them.

```sh
scripts/permit.sh check --command "git push --force"                   # exit 1, DENY with the rule
scripts/permit.sh check --command 'cat scripts/hooks/require-phase.sh'  # exit 0: a read
scripts/permit.sh check --command 'echo x > scripts/hooks/a.sh'        # exit 1: a write target
scripts/permit.sh targets --command 'cp a docs/b.md' --cwd .           # project files it would write
scripts/permit.sh check --path .env                                    # exit 1
scripts/permit.sh rules                                                # print the effective rules
sh tests/permit.sh
```

Without `python3`, the Node fallback (used by `scripts/action.sh`) applies only the `command` and `path` regexes and refuses any command that names a guard path; the phase guard itself refuses every shell call without `python3`, since it cannot know what the call writes.

`scripts/action.sh validate` runs this check after schema validation. The phase guard hook runs it on every real Write, Edit, and Bash call, so the check and the action are the same event.

For an authorized rebase of a task branch, the default permits
`git push --force-with-lease=<ref>:<expected-commit> origin <branch>` when the expected
commit is a full 40- or 64-character hexadecimal object ID. Capture the remote branch
head before the rebase; if it changes, Git rejects the push instead of overwriting
someone else's work. Plain `--force`, `-f`, forced refspecs, and leases without an
explicit expected commit remain denied, including when combined with a valid lease.
This permission does not authorize rewriting shared or long-lived branches; those
still require explicit user authorization. Harness policy maintenance itself remains
protected and requires an explicit user request to change that policy.

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
scripts/harness contract set task.json      # or: contract waive "<reason>", contract show
scripts/harness review submit findings.json
scripts/harness brief
scripts/harness prune --keep 20 --dry-run
```

## Gates that decide done

The model creates, the sensors produce evidence, and the harness decides. Each rule below is written in AGENTS.md, so the agent knows the goal, and enforced here, so it holds under load.

### Task contract

A run is bounded by a contract (`schemas/task.schema.json`, example `tasks/task.example.json`): the goal, the exact deliverables, constraints, non-goals (with optional path globs), and acceptance criteria. At least one criterion carries a `command` (an argv list), so a machine decides when the task is done. During planning, `harness contract set FILE` validates and stores it in the run (`runs/<id>/task.json`); `plan done` refuses without it. A run with nothing to accept records why with `harness contract waive "<reason>"`, and the waiver travels to the reviewer.

- `build done` runs every acceptance command in the target root, after the denylist approves it, and refuses if one fails. Criteria without a command are listed for the reviewer.
- `review done` lists every file changed since `plan done` (committed, uncommitted, untracked) and refuses if one falls under a non-goal path.

### Evidence

`scripts/verify.sh` fails when no check ran at all: an empty run is not evidence. A project that truly has nothing to check declares `allow-empty` in `.harness-required-checks`, and the run says so. `scripts/init.sh` records the categories a target has tooling for (`test`, `lint`) in the target's database (`targets/<id>/db/required-checks`), never in the project; a project file or `HARNESS_REQUIRED_CHECKS` wins.

Verify and review records carry `PROJECT_ROOT` and `TREE_HASH`, a git tree of the project's files as they were on disk (`scripts/tree-hash.sh`; tracked edits plus untracked files that are not ignored, less the phase notes `progress.md`, `tasks/`, `task.json`, and `review-findings.json` at the project root; outside a git work tree it is a content hash of the project's files). `build done` and `review done` accept a record only for the run's own project (`RUN_TARGET`, fixed at `plan start`), and they and the todo gate's completion check compare the tree with the current files: any project edit after the check voids it, and writes outside the project (notes, memory, scratch files) do not.

### Independent review

`scripts/review.sh` runs verify, prints the diff, and writes `review-packet.md` into the records: a brief for a reviewer who did not write the change, the tree hash, a findings template, the task contract, the full verification output, and the diff. The packet holds no conversation, so the reviewer judges the change rather than the narrative.

- When a person has written a reviewer command into `.harness-db/reviewer` (one line; agents cannot write the harness database), review.sh runs it: `sh -c "$(cat .harness-db/reviewer)" reviewer PACKET FINDINGS_OUT`, for example a `claude -p` or `codex exec` wrapper that reads the packet and writes JSON. Valid findings for the current files are stored.
- Otherwise the agent hands the packet to a fresh-context reviewer (a subagent), and a person stores its JSON from their own terminal: `harness review submit FILE`. Submit asks for `yes` typed on the terminal (`/dev/tty`), which an agent's shell does not have, so no spelling of the command lets the author approve its own work; the guard also refuses it from tool calls. `submit` refuses findings for files that have since changed.

The diff in the packet includes the commits made since the run's plan (`CONTRACT_BASE`), or since `HARNESS_REVIEW_BASE` when a person sets it, as well as uncommitted and untracked changes.

Findings follow `schemas/review-findings.schema.json`: `tree_hash`, `reviewer`, a `verdict` (`approve` or `block`), and findings with severity, file, line, claim, evidence, and status. `review done` requires an `approve` verdict for the current tree with no open blocker or major finding; `scripts/review_findings.py check` prints each open one.

### Repeated-failure stop

`scripts/failure_budget.py` runs on every shell call's result (PostToolUse, and PostToolUseFailure in Claude Code; installed by `scripts/install-hooks.sh`). It keys a failure by the command and signs it by the normalized error (numbers, hashes, and temporary paths removed) and reports it to `harness failure`. The same command failing with the same error twice in a row (`HARNESS_BUDGET_REPEAT_FAILURES`, default 2; 0 disables) pauses the run on the `repeat_failure` budget, and the notice goes back to the agent. A different error restarts the count; a success clears it. Resuming needs the user and a `harness continue` note that states the new approach, which clears the count. Only digests and the error's first line are stored. The guard refuses `harness failure` from a tool call, so the agent cannot reset it.

### Sessions, stale runs, and the guard version

The guard binds to the session in each hook payload, so every session uses its own run and a new session must open its own phase. `CLAUDE_CODE_SESSION_ID` is recognised alongside `HARNESS_SESSION_ID`, `CODEX_THREAD_ID`, and `CLAUDE_SESSION_ID`. A run idle longer than `HARNESS_RUN_IDLE_HOURS` (default 24; 0 disables) is stale: `status` shows it, the guard refuses it, and `plan start` marks it expired and opens a new run.

`scripts/guard-version` stamps the guard. `scripts/install-hooks.sh` records the installed checkout in `~/.config/harness/root` (or `HARNESS_HOME`); a checkout whose guard is older, such as a stale worktree of the harness, refuses work except `git` until it is updated.

### What the guard can and cannot stop

Two gates do not depend on how a command is spelled. Review approval needs `yes` typed on a terminal, and the reviewer command lives in the harness database, which the agent cannot write through the guard. Everything else the guard enforces by reading the command. That stops the mistakes and shortcuts an agent actually makes: editing outside build, skipping verification, resetting a failure count, retuning a budget, or touching the guard, its state, secrets, or git internals through the many shell spellings tested in `tests/permit.sh` and `tests/harness-hook.sh`. Four independent reviews probed for bypasses, and every reproduction is a case there.

A parser reads a command; it cannot run it. Shell indirection it has no case for, and above all code the agent writes and then runs (a script file, or interpreter code that builds a path at run time), can still reach what the guard protects, because the agent runs as your user. For a hard barrier against a deliberately adversarial agent, run it in an OS sandbox that denies writes to `scripts/hooks/`, `scripts/permit.*`, `scripts/guard-version`, `schemas/`, `.claude/settings*.json`, and `.harness-db/` (for example Claude Code's sandbox filesystem rules), and run `abort` and `review submit` in a terminal the agent does not control.

### Brief, map, and retention

At session start the hook prints `harness brief`: this session's run or the latest one, its contract, any pause and its reason, the last ten recorded steps (per-tool noise hidden), and the project map. `scripts/init.sh` regenerates the map (`targets/<id>/map.md`) at every init: top-level paths with file counts, the checks verify can run, and the docs to start from.

`plan start` archives finished runs beyond the newest `HARNESS_RETAIN_RUNS` (default 50) into `runs/archive/<id>.tar.gz`; a run some session still has active or paused is never archived. `harness prune --keep N [--dry-run]` does the same on demand.

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

#### Target history in the routed state

Every call that reaches TypeSafe also carries `signals.history`, seven bucketed enums that `scripts/task_route.py` computes from the target's own database (the 20 most recent runs under `runs/`, their `state` and `log`, and the selected session's `verify.state`):

| Fact | Meaning | Values |
| --- | --- | --- |
| `prior_runs` | recent runs, including one still active | `none`, `one`, `few` (2-5), `many` (6+) |
| `verified_builds` | runs whose `build done` gate accepted a passing `scripts/verify.sh` record | same buckets |
| `unverified_builds` | runs that started a build but never passed that gate | same buckets |
| `runs_with_loops` | runs that re-entered an earlier phase | same buckets |
| `total_loops` | phase re-entries summed across those runs | same buckets |
| `aborted_runs` | runs ended with `harness abort` | same buckets |
| `last_verify` | exit of the most recent verification record | `none`, `passed`, `failed` |

Individual failed verification attempts are not counted: `scripts/verify.sh` keeps only the latest `records/verify.state`, so a run that failed several times before passing looks like one verified build. The facts therefore combine each run's gate outcome with the last exit. Only these enums leave the machine: no paths, run ids, commit hashes, step notes, or commands. The route record under `routes/` stores the same `history` object. Deterministic routes do not read history.

Why facts rather than making the session-start route opt-in: over 76 session-start routes the answer was `reasoning_model` every time, because every field the hook could fill (`task_kind`, `area`, `diff_size`, `reversibility`, `uncertainty_reason`, `known_failures`) is constant or nearly constant for an interactive session with no prompt. The harness already keeps per-target run outcomes, so reading them costs a directory scan and no new state, and a target with a record of clean verified builds is distinguishable from one that loops or leaves builds unverified. The facts are added inside the task adapter, so `harness launch` and the SessionStart hook both get them without new metadata fields; when `--project` names a registered target, its own database is read even if the command runs elsewhere. If `harness advise --report` still shows a constant answer once targets have history, making the SessionStart route opt-in is the next step.

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
open http://127.0.0.1:18080/                                # browser workbench (Arc UI host): totals, families, routes, recent events; /events for the log
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
- `build done` fails unless the selected session's `verify.state` exists, was written after `build start`, and reports `EXIT=0`. `scripts/verify.sh` writes that record on every run, pass or fail.
- `review done` fails unless the selected session's `review.state` was written after `review start`, belongs to this run, and reports a passing verification. `scripts/review.sh` writes it when it reaches the end.
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

The guide block is optional: automatic initialisation recognises a target through the registry, not through this block. `make install-guides` (or `scripts/install-guides.sh --project PATH`) adds a marked "Harness" block to `AGENTS.md` and `CLAUDE.md` in the target project, creating the files when missing; a `CLAUDE.md` that imports `@AGENTS.md` is left alone. Rerunning refreshes the block in place between `<!-- harness-cli:start -->` and `<!-- harness-cli:end -->` and leaves everything else untouched. The block is the short gated workflow (under 350 words, which `tests/install-guides.sh` enforces) and points at these docs; it is read at every session start, so detail lives here instead. The three optional Jev trigger commands are in [Jev decision checkpoints](jev-checkpoints.md), and the test runs them against the offline fake router. For a project outside the harness root the block uses the absolute CLI path.

```sh
make install-guides
make install-guides PROJECT=/path/to/project
sh tests/install-guides.sh
```

The `Makefile` deliberately has no `format`, `lint`, `typecheck`, `test`, or `build` targets, so `scripts/verify.sh` detection is unchanged.

## Phase Guard Hook

`.claude/settings.json` registers `scripts/hooks/require-phase.sh` as a Claude Code `PreToolUse` hook for `Write`, `Edit`, `MultiEdit`, `NotebookEdit`, and `Bash`. The hook binds to the session in its payload, asks `scripts/harness status` for that session's run, and blocks the tool call (exit 2) unless a phase is active and the run is not paused, stale, complete, aborted, or expired. A Bash call that is one plain bookkeeping invocation of this harness's own CLI (`plan`, `build`, `review start|done`, `status`, `step`, `continue`, `contract`, `brief`, `workflow`, `prune`, `advise`, `route`, or `scripts/action.sh validate ...`; optionally after `cd DIR &&`; the executable resolving to this checkout; no `--session-id`) skips the other checks so the agent can open a phase. Any other subcommand, such as `budget`, is judged like every command; a chained command such as `scripts/harness plan start; rm -rf build`, a newline, a redirect, a substitution, or another binary named `scripts/harness` is judged like any other command. The hook permits `scripts/harness continue` because it cannot inspect chat context; agents may use it only after an explicit current-conversation user instruction and must retain the required evaluation note. It refuses agent-issued `scripts/harness abort`, `review submit`, `failure`, and `scripts/knowledge-trust.sh approve`, alone or chained. Once a phase is active, it also refuses calls in a project other than the run's own (`Target:` in `harness status`).

Before the phase check, the hook refuses a guard older than the installed harness, applies the denylist to the files the command would write or the write path, and refuses to work while a `knowledge/` folder is unapproved. Plan and review do not change the project: outside build, project writes are blocked except their own artifacts (`progress.md`, `tasks/`, `task.json`, `review-findings.json`); files outside the project stay writable. Every allowed call is then recorded with `scripts/harness step --note "tool:NAME"`, so the step budget counts real tool calls instead of self-reports. Use `HARNESS_BUDGET_TIME_MIN=<n>` only for a deliberately time-boxed run. Tokens stay `unknown` because the hook payload carries no token counts.

A person can switch the hook off for one session by exporting `HARNESS_HOOK_DISABLE=1` in the environment Claude Code starts from. The denylist refuses setting that variable inside agent commands, so the agent cannot do it for itself; mentioning it (for example in a grep) is fine.

The hook is enforcement for Claude Code only. Other agents still rely on the written rules. Run its regression test with:

```sh
sh tests/harness-hook.sh
```

`scripts/verify.sh` automatically detects common Make, JavaScript/TypeScript, PHP, Go, Rust, and Bash commands. It runs available checks and skips missing checks clearly, and it never runs a command that rewrites files: only `format-check`, `fmt-check`, `check-format` Make targets and `format:check` or `prettier:check` scripts are used, and a plain `format` target or script is reported as a skip. When the project being verified is this harness itself (it has `scripts/harness` and `tests/*.sh`), the `harness:tests` check runs every script in `tests/`. Each run ends by writing its selected session's `verify.state` (legacy manual runs use `.harness-db/records/verify.state`), which `scripts/harness build done` requires.

Projects can require verification categories by adding `.harness-required-checks` at the target root. Use one or more of `format`, `lint`, `typecheck`, `test`, and `build`, separated by whitespace or lines. A required category fails verification when it runs no checks. `HARNESS_REQUIRED_CHECKS` overrides the file for temporary or CI-specific requirements, and a registered target without either uses the categories `scripts/init.sh` recorded in its database. A run where no check ran fails unless the list includes `allow-empty`.

Verification narrows itself for documentation-only work. It lists every file changed since the merge base with `@{upstream}` (falling back to `origin/HEAD`): committed, uncommitted, and untracked, with renames split into old and new paths. If every path matches the docs patterns, only format and lint run; typecheck, test, and build are skipped, required or not, and the run record carries `SCOPE=docs-only`. Any doubt runs the full set: no git, no base, no changed files, or a single non-doc path.

The default docs patterns are `*.md`, `*.mdx`, and `*.markdown`, excluding `AGENTS.md`, `CLAUDE.md`, and `SKILL.md` at any depth because tests often assert on agent instructions. A target can replace them with `.harness-docs-paths`: one `case` pattern per line, `!` to exclude, `#` for comments. List only paths no test reads; CI still runs the full suite.

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

With `HARNESS_JEV_CHECKPOINTS=1`, `harness plan done`, `harness build start`, `scripts/verify.sh`, and `scripts/review.sh` emit their own shadow checkpoints and label them from the verification exit code and the run's loop count; `harness review done` prints the pilot counter. The checkpoints add one bounded API call per seam and never change a gate result. Their facts include the target's verification history, which `verify.sh` appends to the private `records/verify-history.jsonl` while checkpoints are enabled (last 100 results with a local tree fingerprint), how previous runs ended, the changed-line bucket, and whether tests changed together with source. Each oracle is labeled only from the run that created it; oracles a run left behind are labeled from that run's own state (or `unknown`) at the next checkpoint event. The current cohorts are the `-2` question versions; see [Jev decision checkpoints](jev-checkpoints.md) for the facts, the reason the `progress.md` questions were dropped, and every labeling rule.

### Jev hooks for interactive sessions

Interactive Claude Code and Codex sessions bypass `harness launch`, so additive hooks cover them. `scripts/hooks/session-route.sh` (SessionStart) records one shadow task-entry route when the working directory is a harness target, using only enum metadata derived from git and harness state, and prints one context line. `scripts/observe_commands.py` (installed PreToolUse observer for Bash) keeps a checksum count of commands in the active run and emits one `progress_assessment` checkpoint on the third identical command; it stores no command text and always exits 0. That checkpoint is labeled mechanically at a later checkpoint event: stuck when the same command recurs or the run loops afterwards, not stuck when its phase and run end without either. `scripts/retrieval-reminder.sh` (PreToolUse for Grep and Glob, Claude Code only) acts on the first Grep or Glob of a session inside a registered target: when the active run has no `scripts/jg.sh` retrieval record it adds one line of context naming the exact wrapper command, and every later Grep or Glob in that session is silent. It is silent without `HARNESS_JEV_CHECKPOINTS=1`, for targets carrying `.harness-no-upload`, and outside registered targets; it stores only a checksum of the session id under `retrieval-reminders/` in the target database, never blocks, and always exits 0. It lives outside `scripts/hooks/` because the default denylist reserves that directory for human edits. Install or remove all entries with:

```sh
scripts/install-hooks.sh            # ~/.claude/settings.json and ~/.codex/hooks.json, with backups
scripts/install-hooks.sh --dry-run
scripts/install-hooks.sh --uninstall
```

The installer only touches hook groups whose command points at these scripts, and writes the Grep/Glob reminder only to the Claude Code settings file. It also installs the enforcing `scripts/failure_budget.py` (repeated-failure stop) and `scripts/workflow_gate.py` (todo gate) hooks and records the installed harness root for the guard-version check. None of them replaces `scripts/hooks/require-phase.sh`, the phase guard.


### Audit collection controls and Workflow

The audit workbench at `http://127.0.0.1:18080/` provides persisted, independent Jev and Workflow collection switches. Install the additive collector with `scripts/install-hooks.sh`, then start or restart the audit service. `/workflow` groups newly collected events by native session and task. See `services/harness-audit/README.md` for lifecycle coverage, metadata commands, API contracts, and limitations.

- `HARNESS_AUDIT_SETTINGS`: shared local JSON switches (default harness `.harness-db/audit-settings.json`); use the same path for the service and collectors.
- `HARNESS_AUDIT_OUTBOX`: durable SQLite delivery queue (default harness `.harness-db/audit-outbox.sqlite`); queued records retry while the service runs and are dropped when their collection category is disabled.
- `HARNESS_WORKFLOW_STATE`: local session/task bindings (default harness `.harness-db/workflow-state.sqlite`).
- `HARNESS_SESSION_ID`: exact native session binding for harness child commands; `CODEX_THREAD_ID` and `CLAUDE_SESSION_ID` are also recognized. Hooks use the session ID in their payload. Missing IDs are not guessed.
- `HARNESS_AUDIT_ENABLED=0`: hard process opt-out. A persisted settings file otherwise controls both categories; before it exists, legacy `=1` enables collection.
- `HARNESS_AUDIT_URL`: emission destination, now defaulting to `http://127.0.0.1:18080`.

Collection switches do not change Jev's evaluations or routing. The dashboard's **Store user prompts** switch controls `workflow_prompts` (default false). When both it and Workflow are enabled, UserPromptSubmit hooks store complete prompts in UTF-8 chunks and the Workflow page shows them, labeling the first captured prompt as the initial goal. Turning it off stops capture and discards queued prompt records; saved history remains. Older settings files default to prompt capture off. Hook guidance reports the current configuration at session start; hooks re-read settings on every submission. Tool content is excluded; curated task/decision descriptions are explicitly supplied through `harness workflow`. Disabling a category retains stored history. The service delivery worker uses `python3` and the companion harness scripts. Telemetry delivery fails open. The separate todo gate blocks covered local execution tools and premature turn completion; it remains active even when collection is disabled. Existing authorization and phase checks still apply.

The installed audit service sets `APP_URL=0.0.0.0` in launchd/systemd and listens on every IPv4 interface at port 18080. Open it through localhost or a machine IP address; telemetry can continue using `127.0.0.1`. The environment example uses the same binding. Existing `.env` files are preserved; installed service configuration overrides their host setting.

Workflow session selectors show a label of at most five words, preferring the first named task, then the native session title, then the first captured prompt. IDs appear in Session metadata alongside the agent, model, and status. Native hooks identify their runtime explicitly and read model/title metadata from the exact matching local transcript or native session metadata database when available; unavailable fields show Unknown. Runtime changes remain in the timeline. `harness workflow refresh-metadata` recovers metadata for already observed sessions without importing prompt or tool content.

Session metadata also displays the full working-directory path (`cwd`). The native hook directory takes precedence over recovered metadata. Directory changes are retained in the timeline; paths preserve case and spaces. Unavailable directories show Unknown.


## Audited todo enforcement

Install or update native hooks with `scripts/install-hooks.sh` on each machine, then restart existing native sessions to load them. The additive `scripts/workflow_gate.py` hook requires a structured plan, reconfirmation or revision for every prompt, and one active todo before covered local execution. Reads need no todo: Read, Grep, and Glob, plus shell commands that only read (`ls`, `cat`, `grep`, `git status`/`log`/`diff`, `find` without `-delete`/`-exec`, `sed -n`, ...), alone or joined with `&&`, `;`, or pipes, as long as nothing is redirected to a file and nothing is substituted with `$(...)`. Harness bookkeeping and the `scripts/verify.sh` and `scripts/review.sh` sensors also need none, so the checks completion depends on can always run. Inside an agent (`CLAUDECODE=1` or `CODEX_SANDBOX`), `harness workflow gate` without a session ID fails closed; a human at a terminal has no plan to check. `scripts/workflow_todos.py` keeps policy state in the local Workflow database even when audit collection is disabled. Collection settings control visibility and storage of emitted events, not this policy. `todo plan`, `update`, `confirm`, and `exempt` print one summary line; `todo show` prints the full list.

```sh
scripts/harness workflow task --name "Repair session navigation" --description "Fix navigation and verify the served dashboard"
scripts/harness workflow todo plan --items '[{"id":"repair","description":"Repair navigation","criterion":"Navigation regression passes"},{"id":"checks","description":"Verify and review","criterion":"Fresh full verification and review pass"}]' --reason "Complete initial plan"
scripts/harness workflow todo update --id repair --status in_progress --reason "Start repair"
scripts/harness workflow todo update --id repair --status completed --evidence "Navigation regression passes" --reason "Repair verified"
scripts/harness workflow todo update --id checks --status in_progress --reason "Run required checks"
scripts/verify.sh --project /absolute/path/to/project
scripts/review.sh --project /absolute/path/to/project
scripts/harness workflow todo update --id checks --status completed --evidence "Full verification and review passed" --reason "Checks complete"
scripts/harness workflow outcome --status completed --description "Navigation repaired and reviewed"
```

Add `--session-id ACTUAL_SESSION_ID` to Workflow commands outside a native session. `todo show` displays IDs, criteria, state and confirmation. For a new prompt, use `todo confirm --reason SUMMARY` or submit the complete revised list with `todo plan`. Keep IDs for retained items; omissions are audited removals with a reason. Changed criteria reset the item and require fresh checks. Completion requires evidence on each completed item, all required items resolved, and passing verify/review records made after the latest scope revision whose tree hash still matches the project's files (records without one fall back to the latest execution). Switching to a new task cannot silently discard unfinished required todos. Questions that require no execution use `todo exempt --reason SUMMARY`; unfinished execution plans must first be resolved or explicitly reported blocked.

The task card displays criteria, status, evidence, and revision history. Select a todo to filter its timeline, including correlated tools and checks. Historical tasks have no fabricated todo list.

The gate uses UserPromptSubmit, PreToolUse and Stop hooks; policy errors return exit 2 with guidance. Telemetry outages do not bypass it. This enforces declared plans on covered native tools, not semantic completeness or a security sandbox: specialized tools outside native hook coverage, hook timeouts, and runtime configuration can bypass enforcement. Codex hosted WebSearch has no PreToolUse; a returned process handle is not tool completion. Stop may request another continuation rather than cancel a session. Existing phase/denylist gates and permissions remain authoritative. Review phase completion may precede deployment todos; explicit completed outcomes and Stop enforce the full task criteria.
