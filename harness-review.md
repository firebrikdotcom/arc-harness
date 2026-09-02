# Harness Review

Review of this harness against four controls: separation of control, feedback and verification, resource budgets, and progressive disclosure.

Confirmed 2026-09-01. Do not treat leftover review bullets as open questions.

## What is already right

- The harness is a control plane for a target project, not the app itself.
- Plan, build, review, and PR are named as separate phases.
- Work is supposed to stay scoped to a task with acceptance criteria.
- `scripts/verify.sh` is a real sensor, not just a prompt.
- Required check categories can be declared and can fail the run.
- Skips and failures are printed, so a silent pass is harder.
- CI runs bootstrap and verify with read-only GitHub permissions.
- `--project` / `HARNESS_TARGET_ROOT` keep harness root and target root distinct.
- Secrets are not required for local verify or CI.
- Project-specific state is supposed to live in ignored `.harness-db/`, not the template.
- An action schema and validator exist. The model can propose JSON and `scripts/action.sh validate` accepts or rejects it.

## Confirmed: keep / build

- Permission: allow unless denied. Harness default denylist if no project file. Project file replaces the default. Rules match commands and paths. Check only; no execute.
- Session budgets (step, time, token, loop): agent rules. The future `harness` CLI counts, pauses, evaluates, and allows continue. The CLI also owns plan → build → review phases.
- Timeouts on `init.sh` / `verify.sh` now.
- Verify run record: write it; completion must cite it.
- `.harness-db/` gets a schema, lock, and retention policy.
- Context: one short core file; other docs on demand. `AGENTS.md` / `CLAUDE.md` collapse to that. `progress.md` is current-run only.
- Threat model: short section in `SECURITY.md`.
- Verify is check-only (no rewrites). Parse shell by shebang. Honor the lockfile or refuse.
- Review continues after verify fails and shows the actual patch.
- Local and CI use the same tool gate. Pin CI actions to commit SHAs. Add fixture tests.

## Confirmed: leave as-is / drop

- No harness executor. The model still does Write, Shell, and git.
- Dry-run: dropped.
- No command manifest. Detection stays the source of truth.
- Make still wins and hides other checks in that category.
- `init.sh` still runs `make init` / `make setup` immediately.
- `knowledge/` hooks still always run.

## Control mapping

| Control | Status after confirm |
|---|---|
| Separation of control | Partial. Schema and validator exist. Permit is a denylist, check only. No execute. |
| Feedback and verification | Partial. Sensors exist. Next: check-only, timeouts, run records, shebang, lockfile, review patch, fixtures. Detection stays. |
| Resource budgets | Deferred to the future `harness` CLI, plus timeouts on init/verify now. |
| Progressive disclosure | Agreed direction: one short core, on-demand docs, slim `progress.md`. |

## Highest-value next builds

- Denylist permit check (commands and paths).
- Per-command timeouts on init and verify.
- Verify run record that completion must cite.
- One short core guide plus on-demand docs.
