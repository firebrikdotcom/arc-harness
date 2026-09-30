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

The harness lets an AI agent act with the operator's permissions inside repositories it did not write. Each entry lists what can go wrong, what the harness does about it today, and what it leaves uncovered. These controls catch honest mistakes. They do not sandbox the agent against a determined attacker.

### Untrusted repositories

- Threat: a cloned target has files that act when the harness touches them, such as build files, agent guides, and a `knowledge/` folder.
- Controls: registration writes only under `HARNESS_DB_ROOT`, never inside the project. A `knowledge/` folder is ignored until a human runs `scripts/knowledge-trust.sh approve`. `scripts/jg.sh` refuses targets marked `.harness-no-upload`.
- Gaps: `scripts/verify.sh` runs the target's own Make, package-manager, Composer, Go, and Cargo commands, so verifying a repository means running its code. `jg` uploads eligible source to its provider unless the target opts out.

### Lifecycle scripts

- Threat: install commands run code the repository chose, including npm, pnpm, and yarn `preinstall` and `postinstall` scripts, Composer scripts, and `make init` or `make setup`.
- Controls: `scripts/init.sh` previews the project-owned commands and runs them only after confirmation, `--yes`, or `HARNESS_INIT_YES=1`. With no terminal and no `--yes`, it refuses.
- Gaps: the SessionStart hook runs `scripts/init.sh --auto`, which runs the plan without confirmation once per bootstrap-input fingerprint. `HARNESS_AUTO_INIT=0` turns it off. Detection itself calls `make -qp`, which evaluates the Makefile, so a `$(shell ...)` assignment runs before any preview. Installs do not pass `--ignore-scripts`.

### Prompt injection

- Threat: text the agent reads tries to instruct it. Sources include code, docs, diffs, tool and test output, jevgrep excerpts, review comments, and a target's own `AGENTS.md` or `CLAUDE.md`.
- Controls: the guides treat that content as data, not instructions. `knowledge/` needs human approval. The phase guard refuses the human-only `harness abort` and `knowledge-trust.sh approve`. Jev decision calls (`harness route`, `harness advise`, and the automatic checkpoints) send only enum or redacted context, and their advice never executes anything. jevgrep source retrieval is different: it uploads source, as described under Untrusted repositories.
- Gaps: a target's `AGENTS.md` and `CLAUDE.md` are followed with no trust gate. The guard cannot read the chat, so it cannot tell whether a `harness continue` was really authorized. Beyond the hook, resisting injected instructions is up to the model.

### Command authorization

- Threat: the agent runs a destructive, privileged, or outward-facing command, or edits the guard that is supposed to stop it.
- Controls: `scripts/action.sh validate` checks a proposed action against `schemas/action.schema.json` and the denylist. In Claude Code, `scripts/hooks/require-phase.sh` applies the same denylist to every real Write, Edit, and Bash call, blocks calls outside an active phase, and protects its own files, the denylist, and the gate records. Agent commands that mention `HARNESS_HOOK_DISABLE` are refused.
- Gaps: the denylist blocks only what a regex matches in the command text, so indirect forms get through: `scripts/action.sh validate` accepts `sh -c "sh -c 'rm -rf /'"`. The hook exists only in Claude Code, so Codex and other agents rely on the written rules. Validation neither runs nor sandboxes a command. Actual approval still comes from the agent runtime's permission prompts.

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
