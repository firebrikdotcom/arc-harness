# QA Review: harness CLI with session budgets

## What changed

`scripts/harness` is a new POSIX shell CLI that acts as the harness control plane. It does three things and nothing else:

1. **Finds the harness root.** It walks up from the current directory looking for a directory that contains both `AGENTS.md` and `scripts/verify.sh`. `HARNESS_ROOT` skips discovery.
2. **Owns session budgets.** Every run counts `steps`, `time_min`, `loops`, and `tokens`. Defaults are `steps=20`, `time_min=15`, `loops=1`, `tokens=unknown`. When a cap is reached, the CLI writes a pause record and exits non-zero. `harness continue` refuses to resume without an evaluation note, and extends the tripped budget by one more window once the note is recorded.
3. **Owns phase order.** `build start` fails until `plan done` has run. `review start` fails until `build done` has run.

There is **no action executor**. The CLI never runs Write, Shell, or git for the model. It records and gates only.

Run state lives in `.harness-db/runs/<run-id>/` (already gitignored): `state` (KEY=VALUE), `run.json` (snapshot), `pauses/NNN.json` (one per pause, with the evaluation note once resolved), and `log`.

The CLI cannot count tokens, so the token budget is recorded as `unknown` unless the agent reports counts with `harness step --tokens N`. A cap set via `HARNESS_BUDGET_TOKENS` is enforced against reported counts only.

## Files

| File | Change |
|---|---|
| `scripts/harness` | New. The CLI. |
| `tests/harness-cli.sh` | New. Regression tests for phase order, pause, and continue. |
| `docs/setup.md` | New "Harness CLI" section, new env vars, test invocation. |
| `README.md` | Two lines added to the project-structure block. |
| `progress.md` | Plan, build, review, and verification entries. |

## Commands

Copy-paste from the worktree root. Both sessions use a throwaway state directory so they are repeatable.

### A. Build is blocked before plan is done

```sh
export HARNESS_DB_ROOT=/tmp/harness-qa-a
rm -rf "$HARNESS_DB_ROOT"

scripts/harness plan start;  echo "exit=$?"
scripts/harness build start; echo "exit=$?"   # blocked
scripts/harness plan done;   echo "exit=$?"
scripts/harness build start; echo "exit=$?"   # now allowed
```

### B. Budget pause, then continue

```sh
export HARNESS_DB_ROOT=/tmp/harness-qa-b
rm -rf "$HARNESS_DB_ROOT"

HARNESS_BUDGET_STEPS=3 scripts/harness plan start; echo "exit=$?"
scripts/harness step --note "read AGENTS.md and docs/architecture.md"; echo "exit=$?"
scripts/harness step --note "draft the implementation plan";           echo "exit=$?"   # pauses
scripts/harness step --note "keep working anyway";                     echo "exit=$?"   # refused
scripts/harness continue;                                              echo "exit=$?"   # no note
scripts/harness continue "Plan was too broad. Narrowed to scripts/harness plus one test file."; echo "exit=$?"
scripts/harness step --note "write the narrowed plan";                 echo "exit=$?"
scripts/harness status
cat /tmp/harness-qa-b/runs/*/pauses/001.json
```

### C. Tests and verification

```sh
sh tests/harness-cli.sh
sh tests/action-schema.sh
scripts/verify.sh
```

## Expected output

- **A**: exits `0`, `4`, `0`, `0`. The blocked call prints `FAIL: cannot start build: plan is 'active', not 'done'.`
- **B**: exits `0`, `0`, `3`, `3`, `2`, `0`, `0`. The pause prints `PAUSE: steps budget reached (used=3 cap=3).` and names a pause record. The bare `continue` prints `FAIL: continue requires an evaluation note.` After the note, the steps cap becomes `6` and the pause record shows `"resolved": true` with the note under `"evaluation"`.
- **C**: both test scripts print `PASS: ...` and exit `0`. `scripts/verify.sh` exits `1` — see "Actual verify paste" for why.

Exit codes: `0` ok, `2` usage/environment, `3` budget pause or refused-while-paused, `4` phase-order violation.

## Manual QA

- [ ] `scripts/harness status` from a subdirectory (`cd docs && ../scripts/harness status`) still reports the repo root as the harness root.
- [ ] `scripts/harness status` from outside any harness root (`cd /tmp && /path/to/worktree/scripts/harness status`) exits `2` with `FAIL: no harness root found`.
- [ ] Session A: `build start` before `plan done` exits `4` and the message names the phase that is blocking.
- [ ] Session A: after `plan done`, `build start` succeeds and `status` shows `plan done` / `build active`.
- [ ] `scripts/harness review start` before `build done` exits `4`.
- [ ] `scripts/harness plan done` without a prior `plan start` exits `4`.
- [ ] Session B: the third step exits `3` and writes `pauses/001.json`.
- [ ] Session B: while paused, both `step` and `build start` are refused with exit `3`.
- [ ] Session B: `status` still works while paused and shows `Paused on: steps`.
- [ ] Session B: `continue` with no argument and with an empty argument both exit `2`.
- [ ] Session B: `continue "<note>"` exits `0`, the note appears in `pauses/001.json`, and the steps cap doubles.
- [ ] Loop budget: `plan start` / `plan done` / `build start` / `build done` / `plan start` pauses on `loops` with `used=1 cap=1`.
- [ ] Time budget: `HARNESS_BUDGET_TIME_MIN=0 scripts/harness plan start` pauses immediately on `time_min`.
- [ ] `scripts/harness status --json` prints a JSON snapshot and does not mutate `run.json`.
- [ ] `git status --short` shows no `.harness-db/` entries after running the CLI with the default state directory.

## Out of scope

Deliberately not implemented in this worktree, per the task:

- Harness action executor. The model still performs Write, Shell, and git itself.
- Dry-run mode.
- Command manifest.
- Running Make and native checks together.
- `init.sh` preview / trust boundary.
- `knowledge/` hook gating.
- Wiring `tests/harness-cli.sh` into `scripts/verify.sh`. The repo's existing convention is to invoke `tests/*.sh` directly, and `scripts/verify.sh` belongs to another worktree's scope.

## Actual verify paste

All commands were run from `/Users/savior/orca/workspaces/harness-template/feat-cli` on 2026-09-02.

```
$ sh tests/harness-cli.sh
PASS: harness CLI phase order, budgets, pause, and continue
(exit 0)

$ sh tests/action-schema.sh
PASS: action schema validation
(exit 0)
```

```
$ scripts/harness plan start
Run created: 20260902T035654Z-64409
State: /tmp/harness-qa-a/runs/20260902T035654Z-64409
OK: plan started in run 20260902T035654Z-64409.
exit=0

$ scripts/harness build start
FAIL: cannot start build: plan is 'active', not 'done'.
Close the earlier phase first: harness plan done
exit=4

$ scripts/harness plan done
OK: plan marked done in run 20260902T035654Z-64409.
Next: harness build start
exit=0

$ scripts/harness build start
OK: build started in run 20260902T035654Z-64409.
exit=0
```

```
$ HARNESS_BUDGET_STEPS=3 scripts/harness plan start
Run created: 20260902T035756Z-83551
State: /tmp/harness-qa-b/runs/20260902T035756Z-83551
OK: plan started in run 20260902T035756Z-83551.
exit=0

$ scripts/harness step --note "read AGENTS.md and docs/architecture.md"
OK: step 2/3 recorded in phase plan.
exit=0

$ scripts/harness step --note "draft the implementation plan"
OK: step 3/3 recorded in phase plan.
PAUSE: steps budget reached (used=3 cap=3).
Run: 20260902T035756Z-83551
Pause record: /tmp/harness-qa-b/runs/20260902T035756Z-83551/pauses/001.json
Evaluate the run, then resume with: harness continue "<evaluation note>"
exit=3

$ scripts/harness step --note "keep working anyway"
FAIL: run 20260902T035756Z-83551 is paused on the steps budget.
Resume with: harness continue "<evaluation note>"
exit=3

$ scripts/harness continue
FAIL: continue requires an evaluation note.
Usage: harness continue "<evaluation note>"
exit=2

$ scripts/harness continue "Plan was too broad. Narrowed to scripts/harness plus one test file."
OK: evaluation recorded, run 20260902T035756Z-83551 resumed.
Budget steps extended to 6.
Pause record: /tmp/harness-qa-b/runs/20260902T035756Z-83551/pauses/001.json
exit=0

$ scripts/harness step --note "write the narrowed plan"
OK: step 4/6 recorded in phase plan.
exit=0

$ scripts/harness status
Harness root: /Users/savior/orca/workspaces/harness-template/feat-cli
State root:   /tmp/harness-qa-b
Run:          20260902T035756Z-83551 (active)
Run state:    /tmp/harness-qa-b/runs/20260902T035756Z-83551
Phase:        plan
  plan   active
  build  pending
  review pending
Budgets (used/cap):
  steps     4/6
  time_min  0/15
  loops     0/1
  tokens    unknown/unknown
exit=0

$ cat /tmp/harness-qa-b/runs/*/pauses/001.json
{
  "version": 1,
  "run_id": "20260902T035756Z-83551",
  "sequence": 1,
  "budget": "steps",
  "used": 3,
  "cap": 3,
  "phase": "plan",
  "paused_at": "2026-09-02T03:57:57Z",
  "resolved": true,
  "resolved_at": "2026-09-02T03:57:57Z",
  "evaluation": "Plan was too broad. Narrowed to scripts/harness plus one test file."
}
```

```
$ scripts/verify.sh
Verification started
Project root: /Users/savior/orca/workspaces/harness-template/feat-cli
Required checks (.harness-required-checks):  lint test
SKIP: no formatter/check target detected

==> bash:shellcheck
... (warnings listed below)
FAIL: bash:shellcheck (exit 1)
SKIP: no typecheck/static-analysis target detected

==> bash:syntax
PASS: bash:syntax
REQUIRED: test satisfied
SKIP: no build target detected

Verification summary: ran=2 skipped=3 failures=1
Verification failed.
(exit 1)
```

**`scripts/verify.sh` fails, and the failure is pre-existing.** The required `lint` category runs ShellCheck across the whole tree. Every warning comes from files this feature does not touch:

```
$ scripts/verify.sh 2>&1 | grep -E '^In \./' | sed 's/ line.*//' | sort -u
In ./scripts/action.sh
In ./scripts/review.sh
In ./scripts/verify.sh
In ./tests/action-schema.sh
In ./tests/verify-required-checks.sh
```

The same warnings reproduce against the base commit, so none of them were introduced here:

```
$ for f in scripts/verify.sh scripts/review.sh scripts/action.sh \
           tests/action-schema.sh tests/verify-required-checks.sh; do
    git show 3e34bf0:"$f" > /tmp/head-check.sh
    printf '%s: %s\n' "$f" \
      "$(shellcheck -s sh /tmp/head-check.sh 2>&1 | grep -oE 'SC[0-9]+' | sort -u | tr '\n' ' ')"
  done
scripts/verify.sh: SC2164
scripts/review.sh: SC1007
scripts/action.sh: SC1007
tests/action-schema.sh: SC1007
tests/verify-required-checks.sh: SC1007 SC2209
```

The two new files are ShellCheck-clean:

```
$ shellcheck scripts/harness tests/harness-cli.sh
(no output, exit 0)
```

Fixing the pre-existing warnings would mean editing `scripts/verify.sh`, `scripts/action.sh`, `scripts/review.sh`, and two existing test files, which belong to other worktrees. That is left as a follow-up.
