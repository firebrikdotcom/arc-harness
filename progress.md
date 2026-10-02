<!-- harness-run: 20260930T033737Z-676628 -->
# Progress: current run

Run `20260930T033737Z-676628`. This file holds only this run's goal, plan, decisions, steps, blockers, and verification. The next `harness plan start` archives it as `runs/20260930T033737Z-676628/progress.md` in the harness database and starts a fresh page. Earlier runs are in their own `runs/<id>/` directories and in git history.

## Goal

TODO #13: `progress.md` held many goals and months of history. Keep it to the current run and archive finished runs under `.harness-db/runs/<id>/`.

- Harness root and target: `/home/savior/Code/harness-template-todo-13-20260930` (worktree, branch `feat/todo-13-20260930`, base `feat/todo-15-20260930` = `b7ee486`, same directory). Docs read: `README.md`, `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`, `docs/conventions.md`, `docs/setup.md`, `todo.md`, `scripts/harness`, `tests/harness-lock.sh`, `tests/harness-cli.sh`.
- Diagnosis: not resolved. No script reads or writes `progress.md`; it is a 567-line diary of many runs appended by hand, and every guide tells agents to append to it.
- Non-goals: no change to gates, budgets, the lock, hooks, or verify/review; no `progress.md` created in a target that does not have one; no deletion of history.

## Plan

1. `scripts/harness`: when `plan start` creates a run and the target root has a regular `progress.md`, copy it into the harness database, check the copy, then replace `progress.md` with a short page for the new run. The first line `<!-- harness-run: ID -->` names the owning run: a page whose marker names an existing run is archived as `runs/<that id>/progress.md`; unmarked or unknown content goes to `runs/<new id>/progress.previous.md`. An existing archive is never overwritten (numbered suffix). A symlink is left alone, and a failed copy leaves `progress.md` unchanged with a warning; the run still starts.
2. `tests/harness-progress.sh`: rotation after a completed and an aborted run, marker ownership, unmarked legacy content, no overwrite of an existing archive, no file created when absent, symlink left alone, failed archive keeps the file, and every archived byte retained.
3. Docs: `docs/setup.md`, `docs/architecture.md` (decision), `README.md`, `AGENTS.md`/`CLAUDE.md`/`docs/conventions.md` wording ("current run"), guide block in `scripts/install-guides.sh`.
4. This repo: the legacy diary (sha256 `c2aae14a…881b`, 567 lines) was copied to `.harness-db/runs/20260930T033737Z-676628/progress.previous.md` and checked byte-identical with `HEAD:progress.md` before this page replaced it.
5. Remove TODO #13 from `todo.md` before final verification.

## Acceptance

- After `plan start`, `progress.md` names only the new run; the previous page is byte-identical in the archive.
- Archives never overwrite each other and no content is dropped on any failure path.
- Targets without `progress.md` are untouched.
- Existing tests, the new test, `scripts/init.sh --yes`, `scripts/verify.sh`, `scripts/review.sh`, and `git diff --check` pass.

## Decisions

- Rotate at the next `plan start`, not at `review done`: the finished run's notes stay in the working tree until the change is committed, and the next run starts clean.
- `HARNESS_PROGRESS_ROTATE=0` opts a target out (a project may keep its own `progress.md` format).
- Incident during build: `tests/harness-hook.sh` starts runs from the caller's directory, so under `scripts/verify.sh` its root is the real checkout, and the new rotation rewrote this tracked page twice (archives went to the test's temporary database and were deleted with it). The page was restored from the session's copy; the legacy diary archive was not affected. `tests/harness-hook.sh` and `tests/jev-checkpoints.sh` (same shape) now set `HARNESS_PROGRESS_ROTATE=0`, `docs/setup.md` tells test authors to do the same, and a sweep of every `tests/*.sh` and `tests/test_*.py` left the page byte-identical.
- Guide wording: only `AGENTS.md`, `CLAUDE.md`, and `README.md` gained a sentence; `docs/conventions.md` and the `scripts/install-guides.sh` block already say "record the plan" and did not need changes (TODO #17 owns guide slimming).

## Build

- `scripts/harness`: `rotate_progress` (called by `create_run` under the run-state lock), `progress_owner`, `free_archive_path`, `write_progress_page`; `HARNESS_PROGRESS_ROTATE` in the usage text.
- New `tests/harness-progress.sh`; `tests/harness-hook.sh` and `tests/jev-checkpoints.sh` opt out; docs in `docs/setup.md`, `docs/architecture.md`, `README.md`, `AGENTS.md`, `CLAUDE.md`; TODO #13 removed from `todo.md`.

## Verification

- Red: `sh tests/harness-progress.sh` against `HEAD`'s `scripts/harness` (scratch `git archive` copy) fails ("expected output to match: Progress: archived ...").
- Mutants (scratch copies): no overwrite guard, no `cmp`, no partial-copy cleanup, no symlink check, no opt-out, no empty-page branch, no owner-directory check, no `..` rejection, no rotation, `mv` instead of in-place write, no unreadable-page guard: all 11 killed.
- `sh tests/harness-progress.sh`, `sh tests/harness-cli.sh`, `sh tests/harness-lock.sh`, `sh tests/harness-hook.sh`, `sh tests/jev-checkpoints.sh`: PASS. `shellcheck` on the changed shell files: clean.
- `scripts/init.sh --yes` exit 0; `scripts/verify.sh --project /home/savior/Code/harness-template-todo-13-20260930` exit 0 (ran=3 skipped=3 failures=0, all `tests/*.sh` PASS); `progress.md` unchanged by the run.
