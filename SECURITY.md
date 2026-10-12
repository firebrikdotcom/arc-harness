# Security

## Repository Hygiene

- Do not commit secrets, credentials, private keys, local tokens, or real `.env` files.
- Keep local runtime artifacts out of git, including `.venv/`, `.codex/`, `.agents/`, caches, and dependency folders.
- Use `.env.example` for documented configuration shape only.
- CI and local verification must not require production credentials.

## Agent Harness Notes

This template keeps portable harness files in the repository: guides, docs, task templates, scripts, and CI.

Local agent installations can contain absolute paths, user-specific hooks, generated manifests, and machine state. Those files are ignored by default and should be regenerated per workstation rather than committed.

## Threat Model

The harness lets an AI agent act with the operator's permissions inside repositories it did not write. Each entry lists what can go wrong, what the harness does about it today, and what it leaves uncovered. These controls catch honest mistakes and shortcuts. They do not sandbox the agent against a determined attacker; `docs/setup.md` ("What the guard can and cannot stop") says how to add an OS sandbox for that.

### Untrusted repositories

- Threat: a cloned target has files that act when the harness touches them, such as build files, agent guides, and a `knowledge/` folder.
- Controls: registration and bootstrap state are written only under `HARNESS_DB_ROOT` (default `.harness-db/` in the harness checkout), never inside the project. A `knowledge/` folder is ignored, and the phase guard refuses to work, until a human runs `scripts/knowledge-trust.sh approve`; a later change to the folder needs a new approval. `scripts/jg.sh` refuses targets marked `.harness-no-upload` and the upload-widening flags `--include-sensitive` and `--no-ignore`.
- Gaps: `scripts/verify.sh` runs the target's own Make, package-manager, Composer, Go, and Cargo commands, so verifying a repository means running its code. `jg` uploads eligible source to its provider unless the target opts out.

### Lifecycle scripts

- Threat: install commands run code the repository chose, including npm, pnpm, and yarn `preinstall` and `postinstall` scripts, Composer scripts, and `make init` or `make setup`.
- Controls: `scripts/init.sh` previews the project-owned commands and runs them only after confirmation, `--yes`, or `HARNESS_INIT_YES=1`. With no terminal and no `--yes`, it refuses.
- Gaps: the SessionStart hook (`scripts/hooks/auto-init.sh`) runs `scripts/init.sh --auto`, which runs the plan in the background without confirmation once per bootstrap-input fingerprint. `HARNESS_AUTO_INIT=0` turns it off. Detection itself calls `make -qp`, which evaluates the Makefile, so a `$(shell ...)` assignment runs before any preview. Installs do not pass `--ignore-scripts`.

### Prompt injection

- Threat: text the agent reads tries to instruct it. Sources include code, docs, diffs, tool and test output, jevgrep excerpts, review comments, and a target's own `AGENTS.md` or `CLAUDE.md`.
- Controls: the guides treat that content as data, not instructions. `knowledge/` needs human approval. The phase guard and the default denylist refuse the human-only commands: `harness abort`, `harness review submit`, `harness failure`, and `knowledge-trust.sh approve`. Review approval needs `yes` typed on a terminal or a reviewer command a person wrote into the harness database. Jev decision calls (`harness route`, `harness advise`, and the automatic checkpoints) send only enum or redacted context. Their answer never runs a command itself: with `harness jev` on it picks among the launch profiles a person configured and names the action the agent should take, while permissions, failed and required checks, and the completion gates still decide. jevgrep source retrieval is different: it uploads source, as described under Untrusted repositories.
- Gaps: a target's `AGENTS.md` and `CLAUDE.md` are followed with no trust gate. The guard cannot read the chat, so it cannot tell whether a `harness continue` was really authorized by the user. Beyond the hooks, resisting injected instructions is up to the model.

### Command authorization

- Threat: the agent runs a destructive, privileged, or outward-facing command, or edits the guard that is supposed to stop it.
- Controls: `scripts/action.sh validate` checks a proposed action against `schemas/action.schema.json` and the denylist (`schemas/denylist.default`, or a project's `.harness-denylist`). In Claude Code, `scripts/hooks/require-phase.sh` applies the same denylist through `scripts/permit.py` to every real Write, Edit, MultiEdit, NotebookEdit, and Bash call, blocks calls outside an active phase, and protects its own files, the denylist, the harness database, secrets, and git internals. `scripts/permit.py` parses shell commands (following `sh -c`, `eval`, heredocs, and `cd`) and judges the files a command would write, not only its text. Agent commands that set `HARNESS_HOOK_DISABLE` or retune the budgets are refused. The repeated-failure stop (`scripts/failure_budget.py`) pauses a run when the same failure repeats, and the todo gate (`scripts/workflow_gate.py`) requires a declared plan and an active todo before covered tools run; `scripts/install-hooks.sh` installs both for Claude Code and Codex.
- Gaps: the parser reads a command but cannot run it, so indirection it has no case for gets through: the hook and `scripts/action.sh validate` both accept `x=/; rm -rf $x`, interpreter code such as `python3 -c 'import shutil; shutil.rmtree("/")'`, and a command decoded at run time. `scripts/action.sh validate` also joins a `run_command` argv with spaces before the check, losing its quoting, so it accepts `["sh", "-c", "sh -c 'rm -rf /'"]`, which the hook refuses as a shell command. A script the agent writes and then runs can reach anything the user can. The phase guard and denylist hook exist only in Claude Code, so Codex and other agents rely on the written rules for them. The todo gate enforces declared plans, not safety. Validation neither runs nor sandboxes a command, and actual approval still comes from the agent runtime's permission prompts.

## Before Commit Or PR

Run:

```sh
scripts/verify.sh
scripts/review.sh
```

Also inspect the staged set:

```sh
git diff --cached --stat
git diff --cached --name-only
```
