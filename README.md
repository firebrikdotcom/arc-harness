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

With the hooks installed (`scripts/install-hooks.sh`), this happens automatically: every Claude Code or Codex session registers the project it starts in as a harness target and bootstraps it once per lockfile fingerprint. See `docs/setup.md`.

Then read `AGENTS.md`: it is the map, with each rule next to the gate that enforces it. `CLAUDE.md` imports it. Open `docs/setup.md` (every command and variable), `docs/architecture.md`, or `docs/conventions.md` only when a task needs them.

## Daily Workflow

```sh
scripts/harness plan start
# plan the work and write its contract (tasks/task.example.json)
scripts/harness contract set task.json
scripts/harness plan done

scripts/harness build start
# do the work, then produce the evidence
scripts/verify.sh
scripts/harness build done

scripts/harness review start
scripts/review.sh                          # writes the packet for an independent reviewer
scripts/harness review submit findings.json
scripts/harness review done
```

You cannot build before plan is done. You cannot review before build is done. Edits are allowed only in build.

`plan done` needs a task contract (or a recorded waiver). `build done` needs a passing `scripts/verify.sh` on the current files, where at least one check ran, and every acceptance command in the contract. `review done` needs an independent reviewer's approving findings for the current files and no change in a non-goal path. The same command failing the same way twice pauses the run. See "Gates that decide done" in `docs/setup.md`.

If it stops you: `scripts/harness status`. An agent may run `scripts/harness continue "<evaluation note>"` only after the user has explicitly instructed continuation in the current chat; record that authorization in the required evaluation note (for example, `User explicitly requested continuation in chat.`). Only a human may abort a run or approve a `knowledge/` folder. The CLI exits `3` for a pause and `4` for a phase-order or gate violation. A continuation extends the tripped budget by one window and is capped by `HARNESS_BUDGET_CONTINUES` (default: three).

Native sessions keep separate run pointers and check records within each target database. Resuming the same session preserves its budgets; a new session starts independently of older paused runs. Manual commands without session identity keep the legacy directory-wide pointer. See `docs/setup.md` for identity selection and hook installation.

The phase guard is a Claude Code hook only. It checks the denylist and knowledge-trust state before requiring an active phase, then counts an allowed tool call as a harness step. Other agents must follow the written workflow themselves.

## Important Rules

- Do not declare success without running `scripts/verify.sh`.
- The guard applies the denylist to every real Write, Edit, and Bash call, judged on the files a command would write. `scripts/action.sh validate PATH` checks a proposed action against the same rules before you run it.
- `scripts/init.sh` previews project-owned setup commands and runs them only after you confirm, or with `--yes`. It first fails if a tool in `.harness-required-tools` (or `git`/`sh`) is missing.
- Bootstrap uses the lockfile-aware install command: `npm ci`, `pnpm install --frozen-lockfile`, `yarn install --frozen-lockfile`, `bun install --frozen-lockfile`, `composer install --no-interaction --prefer-dist`, or `cargo fetch --locked` when the matching lockfile exists. `scripts/init.sh` installs a Node lockfile only with the package manager that wrote it; when that manager is missing, or lockfiles from different managers are present, it refuses and runs nothing.
- Every bootstrap command and every verification check runs under a time limit (`HARNESS_BOOTSTRAP_TIMEOUT` and `HARNESS_VERIFY_TIMEOUT`, 1800 seconds each by default). A command that runs longer is stopped along with its child processes and fails with exit `124` and a `TIMEOUT:` line. Discovery probes that run project code (`make -qp`, the `node` script listing) are bounded too. Without a supervisor (`python3` or GNU `timeout`) nothing is run.
- A `knowledge/` folder is followed only after a human runs `scripts/knowledge-trust.sh approve`.
- Keep planning, building, and reviewing as separate phases.
- Do not assume secrets exist locally or in CI.
- Do not delete existing files unless the task explicitly requires it.
- Record steps with `scripts/harness step --note`; `scripts/harness brief` reads them back at the next session start.

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
progress.md               Pointer: run progress lives in each run's log (harness brief)
schemas/action.schema.json  Proposed-action contract
scripts/action.sh           Action validator
scripts/harness             Harness CLI: harness root, session budgets, plan/build/review phases
scripts/task_route.py       Structured task-entry route with deterministic gates and optional TypeSafe call
scripts/agent_launch.py     Launches an argv command profile selected by the task-entry route
scripts/codex_budget.py     App Server-owned Codex goal-token meter and active-turn interrupter
scripts/audit_emit.py       Best-effort route, outcome, and token telemetry bridge
scripts/audit-service.sh    Runs the versioned Arc audit service
scripts/jev-enable.sh       Enables JEV checkpoint collection, the model pin, and Arc telemetry for a shell
scripts/jev_delegation.py   The persisted `harness jev on|off|status` switch: shadow comparison, or follow Jev
scripts/context_advice.py   Jev checkpoints (file or flag form), outcome labels, pending list, and cohort/pilot reports
scripts/phase_checkpoint.py Automatic shadow checkpoints and mechanical labels at phase gates, verify, review, and hooks
scripts/install-hooks.sh    Installs the auto-init and Jev hooks into Claude Code and Codex user settings
scripts/harness-target.sh   Machine-local target registry: project root, register, lookup, per-target database, bootstrap fingerprint
scripts/hooks/require-phase.sh  Claude Code PreToolUse hook: blocks edits and shell calls outside an active phase
scripts/hooks/auto-init.sh      SessionStart hook: registers the session's project as a target, bootstraps it once per lockfile fingerprint, then runs the session route
scripts/hooks/session-route.sh  SessionStart hook: one task-entry route per interactive session in a harness target (shadow, or delegated when the switch is on)
scripts/observe_commands.py    Session-scoped PreToolUse command-repeat observer; never blocks
scripts/session_hook.py        Native SessionStart identity binding before bootstrap/routing
scripts/run_paths.py           Shared session-owned current-pointer and check-record selector
scripts/retrieval-reminder.sh   PreToolUse hook (Grep/Glob, Claude Code): one jg.sh reminder on a session's first search without a retrieval; never blocks
.claude/settings.json       Registers the phase guard hook
scripts/init.sh             Bootstrap: registers the target, previews project-owned commands, runs them after confirmation (or automatically with --auto)
scripts/required-tools.sh   Checks the tools in .harness-required-tools (init.sh) and prints their apt packages (CI)
.harness-required-tools     The one list of tools local and CI verification of this harness need
scripts/install-guides.sh   Adds the harness command block to AGENTS.md and CLAUDE.md
scripts/jg.sh               Semantic retrieval through jevgrep (jg) with refusals, per-target opt-out, and compact private records
scripts/install-jg-skill.sh Install the jevgrep skill plus harness guidance for Claude Code and Codex sessions (skills and global guides)
scripts/sites.sh            `harness sites doctor|publish|list`: publish a Firebrik Sites page with a sha256 receipt and live check; doctor names what is missing
scripts/install-sites-skill.sh Install the firebrik-sites skill for Claude Code and Codex, pointing at this checkout
skills/firebrik-sites/SKILL.md Source of that skill (`@HARNESS@` is replaced with the absolute harness command)
scripts/permit.sh           Denylist check for commands and write paths
scripts/knowledge-trust.sh  Human approval gate for a project's knowledge/ folder
schemas/denylist.default    Default denylist; a project replaces it with .harness-denylist
scripts/verify.sh           Local verification sensor (check-only, never rewrites)
scripts/with-timeout.sh     Runs one bootstrap or verification command under a time limit, stopping its whole process group
scripts/review.sh           Review helper: full patch, continues after a failing verify
tasks/task-template.md      Reusable task template
tasks/auditability-wizard.md  Scope and acceptance criteria for the offline auditability planner
tests/*.sh                  Regression tests; scripts/verify.sh runs them all on the harness root
tools/auditability-planning-wizard.html  Offline ten-decision planner with Markdown and JSON exports
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

It attempts formatter check, lint, typecheck, tests, and build, never running a formatter that rewrites files. Missing checks are reported as explicit skips. Each check, and each harness test file, is stopped after `HARNESS_VERIFY_TIMEOUT` seconds (default 1800) and fails with exit `124`, while the remaining checks still run. On the harness itself it also runs `tests/*.sh`, and every run writes a record to its session-owned records directory (legacy manual runs use `.harness-db/records/verify.state`).

Projects can make a category mandatory with `.harness-required-checks` (or `HARNESS_REQUIRED_CHECKS`) containing `format`, `lint`, `typecheck`, `test`, and/or `build`. A mandatory category that runs no check fails verification. When every file changed since the upstream base (`@{upstream}`, else `origin/HEAD`) is documentation, verification runs only format and lint, skips typecheck, test and build (mandatory ones included), and records `SCOPE=docs-only`. Without a base, with no changes, or with any non-doc path, the full run applies. `scripts/review.sh` runs verification, prints the target patch (and the harness patch for cross-project work), and writes `.harness-db/records/review.state` even when verification fails.

## Agent Entry Points

`make install-guides PROJECT=/path/to/project` refreshes the marked Harness Phases block in a target's `AGENTS.md` and `CLAUDE.md`, preserving the rest of each file. For an external target, that block uses this harness's absolute CLI path.

For scripted starts, `scripts/harness route --state TASK.json --project PATH` accepts only compact enum metadata and applies deterministic gates before optional TypeSafe routing. `scripts/harness launch --state TASK.json --project PATH --agent codex` selects an exact argv profile without invoking a shell. Whether Jev's route is followed is the persisted switch `scripts/harness jev on|off|status` (stored in `~/.config/harness/config.json`, outside every checkout): off, shadow mode preserves the default command and records the comparison; on, `route` and `launch` run in active mode and select the profile Jev recommends, after the deterministic gates. `--mode` on a single call overrides the switch, and `HARNESS_TYPESAFE_ROLLOUT_PERCENT=10`, then `25`, `50`, and `100`, stages the delegation.

`scripts/harness advise --context DECISION.json` is a separate, non-executing TypeSafe choice for a live, context-specific judgment. It rejects sensitive or oversized contexts, returns one supplied option with confidence, and writes a private record under `.harness-db/advice/`; it never authorizes work or runs the option. With the switch on, a v2 checkpoint's `action` is Jev's choice (`delegated: true`) instead of the agent's baseline; the agent takes it unless a deterministic rule decides otherwise.

Open `tools/auditability-planning-wizard.html` directly in a browser to plan the first Arc/JEV auditability slice. It stores answers in that browser, warns against raw prompts and secrets, and exports a Markdown brief plus structured JSON without network requests or a build step.

The implemented Arc audit service lives in `services/harness-audit/`. Start it with `scripts/audit-service.sh setup` once, install its per-user supervisor with `scripts/audit-service.sh install`, and source `scripts/jev-enable.sh` in agent-launching shells to enable the explicit full-rollout operator acknowledgement and best-effort telemetry.

## CI

CI is defined in `.github/workflows/ci.yml`.

It runs:

```sh
scripts/init.sh --yes
scripts/verify.sh
```

CI uses safe defaults and does not assume secrets.

Before those two commands CI installs exactly the apt packages listed in `.harness-required-tools` (`scripts/required-tools.sh packages`), and `scripts/init.sh` then checks the same list, locally and in CI, so a missing tool fails the bootstrap on either side instead of becoming a silent skip. Add a tool by adding its `TOOL APT_PACKAGE` line; `tests/required-tools.sh` fails when a test gates on an unlisted tool or a workflow installs a tool any other way. See `docs/setup.md`, Required tools.

Every action in a workflow is pinned to a full 40-character commit SHA, with the release it came from in a trailing comment (`actions/checkout@<sha> # v4.4.0`), because a tag such as `@v4` can be moved to different code without any change in this repository. `tests/ci-pinned-actions.sh` fails verification on a tag, branch, or short SHA, and on any `uses` it does not parse (a flow mapping such as `- {uses: ...}` or a quoted key), so write steps in the block form `- uses: OWNER/REPO@SHA`. To update an action, resolve the new release's commit with `git ls-remote --tags https://github.com/OWNER/REPO` (for an annotated tag, take the commit on its `^{}` line) and replace both the SHA and the comment.

## Updating This Project

When source code, runtime commands, dependencies, or architecture are added:

- Update `docs/setup.md` with exact setup and run commands.
- Update `docs/architecture.md` with module boundaries and dependency rules.
- Update `docs/conventions.md` if new language/framework conventions are introduced.
- Keep `AGENTS.md` short: it is loaded at every session start, so put detail in `docs/`.

## Local-Only Files

Do not commit local runtime state such as `.venv/`, `.codex/`, `.agents/`, `.harness-db/`, caches, real `.env` files, or private keys. Regenerate local agent tooling per workstation.

Project-related documents are local database records for the harness. Keep project registries, task notes, run progress, generated indexes, and project-specific notes in `.harness-db/` or another ignored database directory instead of tracking them in this template repository.

## Jev decision checkpoints

Consider Jev at meaningful decision points using the [checkpoint workflow](docs/jev-checkpoints.md). `scripts/harness advise` supports v1 choices, v2 batches from a file or from flags (`--family ...`), `--record`/`--label` for outcomes, `--pending` for unlabeled checkpoints, and `--report` for versioned cohorts plus the pilot counter, all without executing anything. `scripts/harness jev on|off|status` is the persisted switch between comparing Jev with the agent's baseline (shadow, the default) and following Jev's choice; shadow and delegated checkpoints report as separate cohorts (`shadow-1`, `delegated-1`). With `HARNESS_JEV_CHECKPOINTS=1` the phase gates, `verify.sh`, `review.sh`, and the installed hooks emit and label checkpoints automatically; those stay shadow observations in both modes because the gates they predict are mechanical. See `docs/setup.md`.

## Semantic retrieval

`scripts/jg.sh [--project PATH] [--root SUBDIR] "question"` asks [jevgrep](https://github.com/dzhng/jevgrep) for the files and excerpts relevant to a question about a target, which is the recommended first discovery step in an unfamiliar project. Because `jg` uploads eligible source to its configured provider, the wrapper refuses `--include-sensitive`, `--no-ignore`, and any target carrying a `.harness-no-upload` marker, and records only a question hash, timing, and exit status under the target database. `scripts/jg.sh --report` summarises past retrievals. Setup and limits are in `docs/setup.md`.

## Publishing to Firebrik Sites

`scripts/harness sites publish FILE ROUTE [NAME]` creates or updates `/{username}/ROUTE` on Firebrik Sites from any directory and prints `OK: <url>` only after the server confirms the exact sha256 it received and stored and the live URL serves it. `scripts/harness sites doctor` lists each missing requirement (token, file permissions, endpoint health, server version, the config switch) with its fix, and `publish` prints the same list instead of failing vaguely. Settings live in the `sites` section of `config.json`; the API token lives only in its own 0600 file. `scripts/install-sites-skill.sh` installs the `firebrik-sites` skill so Claude Code and Codex sessions know the command. See [docs/setup.md](docs/setup.md#publishing-to-firebrik-sites).

Audited todo plans are enforced by additive native hooks, independently of collection switches. See [todo enforcement](docs/setup.md#audited-todo-enforcement) for registration, revisions, evidence, completion gates, and runtime limitations.

## License

This harness is licensed under either of the MIT license ([LICENSE-MIT](LICENSE-MIT)) or the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option. Unless you state otherwise, any contribution you submit for inclusion is dual licensed as above, without any additional terms or conditions.

Runtime toggles share `~/.config/harness/config.json` with brief `_doc` reminders. Run `scripts/harness config init` to preserve and migrate current choices; see [unified runtime configuration](docs/setup.md#unified-runtime-configuration).

Role-scoped native/external workers, decision receipts and the orchestration dashboard are described in [orchestration](docs/orchestration.md). Use `scripts/harness dispatch --help` and `scripts/harness decision --help`.
