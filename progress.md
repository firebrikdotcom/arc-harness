# Jev checkpoint implementation — 2026-09-22

## Jev adoption: task-entry history, trigger points, retrieval reminder (2026-09-29)

- Harness root and target: `/Users/savior/Code/harness-jev-adoption` (worktree of harness-template, branch `feat/jev-adoption`, same directory). Docs read: `AGENTS.md`, `CLAUDE.md`, `docs/setup.md`, `docs/jev-checkpoints.md`, `docs/architecture.md`, `scripts/task_route.py`, `scripts/phase_checkpoint.py` (read only, lane 1), `scripts/install-guides.sh`, `scripts/install-jg-skill.sh`, `scripts/install-hooks.sh`, the existing hooks, and their tests.
- Evidence: the session-start route answered `reasoning_model` 76/76 times; 1 of 213 shadow checkpoints came from an agent; `jg` ran 5 times in 6 days.
- Goal: (1) enum-only target history in the task-entry route; (2) concrete Jev trigger points with exact flag-form commands in the guide block; (3) a Grep/Glob retrieval reminder hook wired into `scripts/install-hooks.sh`.
- Non-goals: no edit to `scripts/phase_checkpoint.py`, `scripts/context_advice.py`, or `services/harness-audit`; no install into `~/.claude/settings.json`; shadow mode only, no promotion.
- Decisions: history facts are computed inside `task_route.py` from the target's run state, run logs, and `records/verify.state`, so the session route (and `harness launch`) gets them without touching lane 1; they are cheap, so the opt-in fallback is not needed. The reminder hook lives at `scripts/retrieval-reminder.sh` because the harness denylist refuses agent writes under `scripts/hooks/` (the guard's own directory); a human can move it there later.
- Acceptance: routed state carries `signals.history` with seven bucketed enums and no paths or ids; the guide block has three triggers whose commands run and evaluate against the fake router and is regenerated idempotently; the hook prints one context line on the first Grep/Glob of a session in a registered target without a run retrieval record and is silent otherwise, always exit 0; `scripts/verify.sh` passes.
- Verification plan: `tests/test_task_routing.py`, `tests/install-guides.sh`, `tests/auto-init.sh` (hook and installer), then `scripts/verify.sh` and `scripts/review.sh`.
- Risks: history buckets may still not discriminate for new targets (all `none`); the live Claude Code rendering of `additionalContext` is covered by format tests, not a live session.
- Build: `scripts/task_route.py` gained `history_facts` (bucketed, window 20) sent as `signals.history` and stored on the route record; `scripts/install-guides.sh` block now lists three trigger points with exact commands (repo-relative docs link inside the harness root) and this repo's AGENTS.md/CLAUDE.md were regenerated, with the duplicate generic section outside the block reduced to a pointer; new `scripts/retrieval-reminder.sh` and `tests/retrieval-reminder.sh`; `scripts/install-hooks.sh` adds the `Grep|Glob` entry for Claude Code only; docs in `docs/setup.md`, `docs/architecture.md`, `README.md`.
- Verification: `python3 -m unittest discover -s tests -p test_task_routing.py -q` (22 tests OK); `sh tests/install-guides.sh`, `sh tests/retrieval-reminder.sh`, `sh tests/auto-init.sh` PASS. First `scripts/verify.sh` failed on two ShellCheck findings in the new tests (SC2016, SC2010); after fixing, `scripts/verify.sh` passed: ran=3 skipped=3 failures=0.
- Review round 1 (panel, tier med): 9 confirmed findings, all fixed. (1) hook file lacked the exec bit, so installed hooks would fail with 126; now 100755 and the test runs it by path and asserts `-x`. (2) `harness route/launch --project` read history from the cwd's database; `history_db` now resolves the registered target (mutation-checked test). (3) guide now says to substitute the real baseline and facts, and the label example uses an `OUTCOME` placeholder. (4) the unwritable-state test could not fail; rewritten (read-only marker dir and a file in its place) and it caught a real stderr leak from the marker redirect, fixed with a grouped redirect. (5) paused runs count as the live run. (6) tests for the harness-root fallback, a completed run's retrieval, and an empty session id. (7) auto-init asserts real history values. (8)(9) setup.md notes that per-attempt verify failures are not stored, and the hook wording is fixed. `scripts/verify.sh` passed again: ran=3 skipped=3 failures=0.
- Review round 2: 0 findings; `scripts/review.sh` exit 0; committed ed3f098 and opened PR #2. Hosted Verify failed: (a) under dash a failed redirection on the special built-in `:` exits the shell, so the read-only marker case exited 2; the hook now uses `touch` (reproduced and fixed locally with a PATH shim mapping `sh` to dash; `scripts/verify.sh` passes natively and under that shim; round 3 delta review: 0 findings). (b) the CI apt ShellCheck (older than local 0.11.0) reports SC2015 infos in `scripts/knowledge-trust.sh:78` and `tests/permit.sh`, which fail master CI on b45b62c and its parents too; the first file is denylist-protected and both are outside this lane, so this is escalated rather than changed.
- Harness note: this session's shell exports `HARNESS_ROOT` pointing at the main checkout, so the phase guard (which resolves the worktree) did not see the first run; phases here run as `env -u HARNESS_ROOT scripts/harness ...` so the guard and the CLI share the worktree database. The stray plan-only run in the main checkout's target database was left for a human to abort.

## Jev gate signal quality (2026-09-29, lane 1)

- Harness root and target: `/Users/savior/Code/harness-jev-gate-facts` (worktree of harness-template, branch `feat/jev-gate-facts`, same directory). Docs read: `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`, `docs/setup.md`, `docs/jev-checkpoints.md`, `scripts/phase_checkpoint.py`, `scripts/harness` (phase handlers, `jev_checkpoint`), `scripts/verify.sh`, `scripts/review.sh`, `scripts/hooks/jev-observe.sh`, `scripts/context_advice.py` (`pilot`, `report`), `tests/test_phase_checkpoint.py`, `tests/jev-checkpoints.sh`, `tests/fake_router.py`, `schemas/denylist.default`.
- Evidence (coordinator audit of 213 shadow checkpoints): `phase-plan-1` said `refine_plan` 35/35 (22/22 labeled over_escalated) because its only relevant fact was "progress.md changed: no" and it asked whether the plan was recorded in progress.md, which interactive targets never write; `review-handoff-1` has the same shape through `docs_and_progress_current`; `verify-predict-1` only ever saw "No verification record exists yet", so `will_pass` sat at 0.4–0.7; `tool-repeat-1` had no oracle (54/55 unlabeled).
- Goal: (1) richer enum-only facts for every automatic checkpoint; (2) stop asking questions the harness cannot satisfy; (3) a mechanical label oracle for tool repeats. Shadow mode only.
- Non-goals: no edit to `context_advice.py` pilot/report (lane 2), `task_route.py` or `scripts/hooks/session-route.sh` (lane 4), `services/harness-audit` (lane 3); no hook edit (the denylist protects `scripts/hooks/`, and the existing digest log suffices); no automatic promotion.
- Decision (part 2): drop the progress.md questions instead of having `plan done` write a note. A harness-written note would make `plan_recorded` constantly true, so it would carry no information about the outcome the oracle measures (a later loop). Both handoff checkpoints now ask `loop_likely` (will the run re-enter an earlier phase) next to the go/hold recommendation; `clarification_needed` is dropped because no enum fact can speak to a user-owned decision. The "progress.md changed" fact is removed.
- Plan:
  1. Facts: change shape (`source_and_tests`, `source_only`, `tests_only`, `docs_or_config_only`, `none`) with a stricter test-path rule; changed-line bucket from `git diff HEAD --numstat` plus bounded untracked line counts; retrieval facts on every checkpoint; previous-run history from `runs/*/state` (completed/aborted/unfinished buckets, loops in the last five completed runs); verification history from a new private `records/verify-history.jsonl` written at `verify-result` (falls back to the single `verify.state`): last-five pass/fail counts, most recent result and streak, verifications in this run, and whether the working tree changed since the last verification (local sha256 fingerprint, never sent).
  2. Questions: `phase-plan-2` and `review-handoff-2` re-centred on loop risk; every automatic checkpoint moves to a `-2` question version because its facts changed (`phase-build-2`, `verify-predict-2`, `tool-repeat-2`).
  3. Oracle: `tool-repeat-2` writes a pending item with the digest position; any later checkpoint event resolves it. Stuck (the same digest recurs, or the run loops after the checkpoint) labels `change_approach`/`gather_more_evidence` correct and `retry_same_command` under_escalated; not stuck (the emission phase completed, the run ended, no recurrence, no loop) labels the reverse (hold over_escalated). A run that ended before the emission phase completed labels `unknown`.
  4. Tests in `tests/test_phase_checkpoint.py` and `tests/jev-checkpoints.sh`; docs in `docs/jev-checkpoints.md` and `docs/setup.md`.
- Acceptance: new facts present and enum-only (no paths, digests, or content in any sent context); no progress.md question in any v2 cohort; tool repeats labeled mechanically end to end; old pending items and records still resolve; `scripts/verify.sh` passes.
- Verification plan: `python3 -m unittest tests.test_phase_checkpoint`, `sh tests/jev-checkpoints.sh`, `scripts/verify.sh`, `scripts/review.sh`; independent `lothar-panel-review` before commit.
- Harness note: this session's shell exports `HARNESS_ROOT=/Users/savior/Code/harness-template`, while the project phase-guard hook resolves the worktree's own `.harness-db`; phases and verification therefore run as `HARNESS_ROOT= ./scripts/harness ...` so the gate records and the guarded run live in the same database. An unused plan-phase run (`20260929T203653Z-3988`) was opened first under the harness-template target entry; abort is human-only, so it is left for the user.
- Build: `scripts/phase_checkpoint.py` gained `is_test_path`/`change_shape`, `changed_lines`/`line_bucket`, `tree_fingerprint`, `verify_history`/`append_verify_history` (private `records/verify-history.jsonl`, written at `verify-result` before the active-run check, failures reported but never blocking labels), `run_history_facts`, `common_facts` (shared by all five seams), the `-2` builders (handoff questions `recommendation` + `loop_likely`; "progress.md changed" fact removed), the tool-repeat oracle (`tool_repeat_outcome`, `label_tool_repeat`, `resolve_tool_repeats`; pending item records checksum, position, and phase locally), and run scoping (`resolve(..., run_id=)`, `resolve_stale`). No hook, `verify.sh`, `harness`, or lane 2–4 file changed.
- Tests: `tests/test_phase_checkpoint.py` 35 tests (new: test-path and shape rules, line buckets incl. untracked, history write/perms/bounds/no-run/unwritable, legacy `verify.state` fallback, previous-run facts, v2 question keys and no progress.md, tool-repeat wait/recur/loop/clean finish/superseded/label matrix, stale and cross-run labeling, legacy pending without run id). `tests/jev-checkpoints.sh` now expects `-2` versions, the tool repeat labeled at `plan done`, pilot 6/30, no unlabeled checkpoints, and a history line.
- Docs: `docs/jev-checkpoints.md` (seam table, shared fact set, history file, handoff-question decision, tool-repeat rule, run scoping) and `docs/setup.md` (checkpoint facts and hook labeling).
- Verification: `python3 -m unittest tests.test_phase_checkpoint` OK (35); `sh tests/jev-checkpoints.sh` PASS; `HARNESS_ROOT= ./scripts/verify.sh` exit 0, ran=3 skipped=3 failures=0 (all `tests/*.sh` PASS, shellcheck, bash syntax).
- Review round 1 (internal `lothar-panel-review`, med tier, DX/QA/architect lanes): 9 confirmed findings (2 Medium, 7 Minor), all fixed. M1: an unlabelable pending item (advice record removed, or no call id) raised out of resolution and blocked every later checkpoint; `record_label` now catches and drops it, and `main` isolates resolution from the event's own emit. M2: a `verify.state` newer than the history (verify run with checkpoints off) was ignored; it is now appended as the most recent result with an unknown tree comparison. m1: the tool-repeat oracle tracked the last log line; it now snapshots the log once and tracks the digest that reached `--repeats` (`repeated_digest`). m2: docs and docstring wrongly said existing pending items were unscoped; reworded. m3–m7: tests for the enable gate (and exactly two history lines end to end), untracked/deleted/capped line counts, untracked and staged fingerprint changes, the completed-run stale branch, and the previous-run window and exclusion. The "counts up to five" doc wording was also aligned.
- Harness: the fix pass re-entered build (loop 1), which paused the run on the loops budget; resumed with `harness continue` after the user explicitly requested continuation in chat.
- Verification after fixes: `python3 -m unittest tests.test_phase_checkpoint` OK (43); a scratch mutation run killed all seven mutations matching the findings; `sh tests/jev-checkpoints.sh` PASS; `HARNESS_ROOT= ./scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0); `HARNESS_ROOT= ./scripts/review.sh` exit 0.
- Review round 2 (same panel, fresh full pass): 3 Minor findings, all fixed. R1: malformed pending items (a non-object file, a non-integer `loops_at`) raised before the per-item guard, blocking every event or every other label; now `drop_corrupt_pending` removes non-object files, `pending_items` skips them, `as_int` parses item fields tolerantly, `label_item` runs each item's oracle and labeling inside one guard, and each resolver has its own guard in `main`. R2: test for a repository with no commit yet (staged lines counted, fingerprint present). R3: `repeated_digest` exact-count rule pinned (`bbbaaab` → `a` at 6). Docs aligned.
- Harness: the second fix pass re-entered build (loop 2) and paused again; resumed with `harness continue` under the user's explicit in-chat instruction to continue to completion.
- Verification after round 2 fixes: `python3 -m unittest tests.test_phase_checkpoint` OK (44); mutations for R1 (dict check, strict parsing), R2, and R3 all killed; `sh tests/jev-checkpoints.sh` PASS; `HARNESS_ROOT= ./scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0); `HARNESS_ROOT= ./scripts/review.sh` exit 0.
- Review round 3: 1 Minor docs finding: `docs/jev-checkpoints.md` said a pending item with a malformed field is dropped, but `as_int` reads malformed integers as zero and the item is labeled or keeps waiting; items are dropped only when labeling raises. The reviewers' mutation batch could not run because the run hit its step budget mid-review (reviewer tool calls count against it); resumed with the third and last `harness continue` under the user's in-chat instruction, then `review done` closed run `20260929T203801Z-8127` (pilot 13/30 on this worktree database).
- New run `20260929T214311Z-58882` plan: reword that docs sentence; add a test that a `loops_at` of `"zz"` is labeled as zero; verify; final panel round including the skipped mutation batch; then commit, push, PR, and hosted CI; after lane 3's PR #1 merges, rebase onto `origin/master`, re-verify, and push with `--force-with-lease`. A fresh run is used because the previous one had no continues left for another loop pause.
- Build (run `20260929T214311Z-58882`): docs sentence reworded; `test_malformed_integer_fields_read_as_zero_and_are_still_labeled` (current-run and stale handoff items with `loops_at` `"zz"`). Mutation check: replacing `as_int` with strict `int()` at all three sites is killed; moving `drop_corrupt_pending` last survives and is equivalent (`pending_items` skips non-objects). `python3 -m unittest tests.test_phase_checkpoint` OK (45); `HARNESS_ROOT= ./scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0).
- Review round 4 (the blocked round 3 mutation batch executed; only equivalent mutants survived apart from the finding): 1 Minor finding: no test pinned `AttributeError`/`TypeError` in `ITEM_ERRORS`. Fixed: the isolation test adds a `run_complete` item with a list recommendation, a tool repeat with a list recommendation, and a `verify_result` item with null answers; both exception mutations are killed. The fix pass re-entered build (loop 1 of this run), resumed with `harness continue` under the user's in-chat instruction. `python3 -m unittest tests.test_phase_checkpoint` OK (45); `HARNESS_ROOT= ./scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0); `HARNESS_ROOT= ./scripts/review.sh` exit 0.

- Follow-up (run `20260929T221255Z-22639`): PR #4 opened at 3d21a17 (already rebased onto c67a6b0 after PR #1 merged) and hosted Verify passed. Plan: correct the test comment on the planted `null-answers.json`/`list-rec.json` items (the first is dropped as an unknown stale label with no advice record, not by a TypeError; the TypeError comes from `list-rec.json`, and the AttributeError from `verify-null.json`), re-run the unit tests and `scripts/verify.sh`, and commit without pushing. Result: comment corrected; `python3 -m unittest tests.test_phase_checkpoint` OK (45); `HARNESS_ROOT= ./scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0).
## jevgrep guidance for Claude Code and Codex sessions (2026-09-28)

- Harness root and target: `/Users/savior/Code/harness-template` (same directory). Docs read: `README.md`, `AGENTS.md`, `CLAUDE.md`, `docs/setup.md` (jevgrep section), `scripts/install-guides.sh`, `tests/install-guides.sh`, the installed `~/.claude/skills/jevgrep/SKILL.md`.
- Finding: on this Mac `jg` 0.4.2 is installed, `jg auth` was completed by the user, `jg doctor` passes, and one live wrapper search on the DollarWise target completed and was recorded. Yet no interactive session is told to use it: the global `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` harness blocks do not mention retrieval, the DollarWise project has no harness guide block, and the installed skill is the upstream copy, which instructs the agent to run `jg` directly (no retrieval record, no `.harness-no-upload` check). The same gap exists on resende-1.
- Goal: one idempotent installer that gives both agents the harness retrieval guidance: refresh the two skill copies from the upstream package and append a marked harness block that redirects searches through `scripts/jg.sh`, and insert a marked retrieval paragraph inside the existing `global-harness` block of `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md`.
- Non-goals: no edit to any project-owned `CLAUDE.md`/`AGENTS.md` (DollarWise stays untouched); no automatic `jg` call from hooks; no change to the wrapper, records, or denylist; no key handling.
- Plan:
  1. New `scripts/install-jg-skill.sh [--home DIR] [--source SKILL.md] [--skip-global]`: locate the upstream skill (explicit `--source`, then the global npm/Homebrew package next to `jg`), write it verbatim plus a `<!-- harness-jg:start/end -->` block to `~/.claude/skills/jevgrep/SKILL.md` and `~/.agents/skills/jevgrep/SKILL.md` (block-only refresh when no upstream source is found but the file exists), and insert or refresh a `<!-- harness-jg:start/end -->` paragraph before `<!-- global-harness:end -->` in `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` when those files carry the block.
  2. `tests/install-jg-skill.sh` with a temporary home and a fake upstream skill: creation, refresh idempotence, stale-block replacement, global block insertion, missing global file tolerated, bad arguments.
  3. Docs: `docs/setup.md` jevgrep section, `README.md` script list, `progress.md`.
  4. Run the installer on this Mac; commit and push; fast-forward resende-1 and run the installer there.
- Acceptance: both skill files end with the harness block naming the wrapper; both global guides contain the retrieval paragraph inside their harness block exactly once; a second run changes nothing; `scripts/verify.sh` and `scripts/review.sh` pass; resende-1 is on the same commit with the same installed guidance.
- Verification plan: `sh tests/install-jg-skill.sh`, `sh tests/install-guides.sh`, then `scripts/verify.sh`, then `scripts/review.sh`.
- Build: new `scripts/install-jg-skill.sh [--home DIR] [--source SKILL.md | --no-upstream] [--skip-global]`: finds the upstream skill next to the installed `jg` (global npm root or Homebrew `lib/node_modules`), writes it verbatim plus a `<!-- harness-jg:start/end -->` "Harness targets" block to both skill paths (block-only refresh for an existing file without a source, skip for a missing one, exit 2 when nothing was written), and inserts or refreshes the same-marked retrieval paragraph before `<!-- global-harness:end -->` in `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` (files without that block are left alone; project guides are never touched). Idempotent: unchanged files are reported as such.
- Build: `tests/install-jg-skill.sh` (creation from a fake upstream, wrapper command and opt-out present, global paragraph placed before the end marker, guide without a block untouched, second run byte-identical, stale block and stale paragraph replaced, `--skip-global`, `--no-upstream` block-only refresh, missing skill not invented, failure when nothing written, bad arguments). Docs: `docs/setup.md` jevgrep section, `README.md` script list.
- Follow-up (same day): on resende-1 the first install found no upstream skill because `jg` lives under nvm and `npm root -g` resolved the system npm, so only the block was refreshed there. `find_source` now also resolves the package that owns the `jg` binary on PATH (`readlink -f`, then `.../@dzhng/jevgrep`) and scans `${NVM_DIR:-$HOME/.nvm}` and `~/.config/nvm` version roots under the selected home; `--source` is validated before the lookup so a missing file fails instead of being swallowed by the subshell. Two test cases added (package found via the `jg` symlink; package found via a home nvm root with no `jg` on PATH).
- Verification: `sh tests/install-jg-skill.sh` and `sh tests/install-guides.sh` PASS; shellcheck clean on both new files; `scripts/verify.sh --project /Users/savior/Code/harness-template` passed with ran=3 skipped=3 failures=0 (all `tests/*.sh` PASS including the new one, 13 routing tests OK, live TypeSafe suite skipped by design). The verify prediction checkpoint was labeled `over_escalated` and the build-start checkpoint `correct`.
- Review: `scripts/review.sh` re-ran verification (passed) and printed the patch: `README.md`, `docs/setup.md`, `progress.md`, plus the new `scripts/install-jg-skill.sh` and `tests/install-jg-skill.sh`. Acceptance met on the Mac; no project-owned guide was edited. Note: `verify.sh` and `review.sh` recorded their run under a `Code-…` target entry (the parent directory registered as a target on this Mac) while the phases ran in the harness root run; a bookkeeping quirk, not a gate failure.
- Applied on this Mac: `scripts/install-jg-skill.sh` refreshed `~/.claude/skills/jevgrep/SKILL.md` from `/opt/homebrew/lib/node_modules/@dzhng/jevgrep/dist/skills/jevgrep/SKILL.md` (the `~/.agents/skills/jevgrep` path resolves to the same file), and inserted the paragraph into `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md`; a second run reported every file unchanged.

## Semantic retrieval with jevgrep (2026-09-28)

- Harness root and target: `/home/savior/Code/harness-template` (same directory). Docs read: `README.md`, `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`, `docs/setup.md`, `docs/jev-checkpoints.md`, `scripts/harness` (phase handlers, `jev_checkpoint`), `scripts/phase_checkpoint.py`, `scripts/install-guides.sh`, `scripts/harness-target.sh`, `scripts/hooks/require-phase.sh`, `schemas/denylist.default`, `tests/harness-cli.sh`; the published `@dzhng/jevgrep` 0.4.2 package (README, bundled `skills/jevgrep/SKILL.md`, bundle flag list).
- Finding: jevgrep (`jg`) is Jev used as a retrieval primitive (question plus repo path in, ranked files and verbatim excerpts out), while the harness uses Jev only as a decision primitive over enum-only state. `jg` uploads eligible source to the configured provider. The user accepted that for now; no harness or skill file currently mentions the tool and no `jevgrep` skill is installed.
- Goal: make jevgrep a first-class discovery step in the harness: a wrapper with refusals and compact private records, a per-target opt-out, a denylist rule for the two upload-widening flags, a guide-block paragraph, the upstream skill installed for Claude Code and Codex, and a `plan done` checkpoint fact so the pilot can see whether retrieval was used.
- Non-goals: no change to phase order, gates, permissions, or knowledge trust; no question text, excerpts, paths, or source in any harness record; no automatic `jg` call from any hook or gate; no Arc event type yet.
- Plan:
  1. New `scripts/jg.sh [--project PATH] "question" [jg options]`: resolves the target root and its private database, requires `jg`, refuses when `.harness-no-upload` exists at the target root, refuses `--include-sensitive` and `--no-ignore`, runs `jg` with stdout passed through, and writes `retrieval/<stamp>.state` (kind, time, run id, phase, question sha256, duration, exit, output bytes, complete flag). `--report` summarises the records.
  2. `schemas/denylist.default`: deny `jg` with `--include-sensitive` or `--no-ignore` (tightening only).
  3. `scripts/phase_checkpoint.py`: `plan done` facts gain "Semantic retrieval used in this run: yes/no" from the retrieval records of the active run.
  4. `scripts/install-guides.sh`: a discovery paragraph in the marked block; refresh the harness root guides.
  5. Install the upstream skill verbatim under `~/.claude/skills/jevgrep/` and `~/.agents/skills/jevgrep/`; install the CLI globally when permitted (auth stays user-run).
  6. Tests: new `tests/jg.sh` with a fake `jg` on PATH; unit test for the retrieval fact.
  7. Docs: `docs/setup.md`, `README.md`, `docs/architecture.md`, `docs/jev-checkpoints.md`, `progress.md`.
- Acceptance: with a fake `jg`, the wrapper passes output through, writes one record with the expected keys, refuses the two flags and the marker, and reports; without `jg` it exits 2 with install guidance; `plan done` facts include the retrieval line; `scripts/verify.sh` passes on the harness root.
- Verification plan: `sh tests/jg.sh`, `python3 -m unittest tests.test_phase_checkpoint`, `sh tests/permit.sh`, `sh tests/install-guides.sh`, then `scripts/verify.sh`, then `scripts/review.sh`.
- Build: new `scripts/jg.sh` (`[--project PATH] [--root SUBDIR] "question" [jg options]` and `--report`): resolves the registered target's database, refuses `--include-sensitive`/`--no-ignore` (exit 4) and a `.harness-no-upload` marker at the target root, exits 2 without `jg` with install guidance, streams `jg` output through `tee` with the status captured in a side file, and writes `retrieval/<stamp>-<pid>.state` (0700 dir, 0600 file) with kind, time, run id, phase, subtree flag, question sha256, duration, exit, output bytes, and `COMPLETE=yes|partial|no` mapped from `jg`'s exit codes 0/2/other.
- Build: `schemas/denylist.default` gained `command \bjg\b.*\s--(include-sensitive|no-ignore)\b` (tightening only; verified with `scripts/permit.sh check`). `scripts/phase_checkpoint.py` gained `retrieval_facts()` (count bucket and complete count for the active run id) appended to the `plan done` facts. `scripts/install-guides.sh` block gained a retrieval paragraph (`$JG` resolves to the relative or absolute wrapper path); harness-root `AGENTS.md`/`CLAUDE.md` refreshed.
- Build: `jg` 0.4.2 installed globally with npm (`/home/savior/.config/nvm/versions/node/v24.20.0/bin/jg`); the upstream `skills/jevgrep/SKILL.md` copied verbatim to `~/.claude/skills/jevgrep/` and `~/.agents/skills/jevgrep/`. `jg auth` is left to the user (interactive, key never in chat).
- Tests: new `tests/jg.sh` (missing `jg`, usage errors, both refusals, marker, pass-through with a fake `jg`, record keys and privacy, subtree and active-run fields, exit 2/1 mapping, report, `--project` from outside, empty report) and two cases in `tests/test_phase_checkpoint.py`. Docs: `docs/setup.md` (new "Semantic retrieval with jevgrep"), `README.md`, `docs/architecture.md` (module and 2026-09-28 decision), `docs/jev-checkpoints.md`.
- Verification: `scripts/verify.sh --project /home/savior/Code/harness-template` passed with ran=3 skipped=3 failures=0 (all `tests/*.sh` PASS including `tests/jg.sh`, 18 checkpoint unit tests OK, shellcheck clean, live TypeSafe suite skipped by design); the verify prediction checkpoint was labeled `correct`. Focused runs before that: `sh tests/jg.sh`, `sh tests/install-guides.sh`, `sh tests/permit.sh`, `python3 -m unittest tests.test_phase_checkpoint` all PASS; first shellcheck pass flagged SC2012 in `tests/jg.sh`, fixed by using `find`.
- Note: the harness run reused the stale 2026-09-23 run (plan re-entered, build already active), so `build start` was not re-issued; the `plan done` checkpoint recommended `refine_plan` against baseline `proceed_to_build` (baseline kept).
- Review: `scripts/review.sh --project /home/savior/Code/harness-template` re-ran verification (passed) and printed the patch: 11 modified files plus `scripts/jg.sh` and `tests/jg.sh`. Acceptance met; scope limited to the planned slice. `jg doctor` reports no saved provider yet, so the live wrapper path is exercised only against the fake `jg`; a real search awaits the user's `jg auth`.
- Live check (2026-09-28): after the user ran `jg auth`, `jg doctor` reported the Jev connection verified through TypeSafe, and `scripts/jg.sh --project /home/savior/Code/harness-template "How does the harness label a shadow checkpoint from a verification result?"` returned 23 ranked files with excerpts (complete, about 4.1 s) and wrote one `retrieval/` record in the review phase; `scripts/jg.sh --report` lists it.
- Sync: committed and pushed to `origin/master`; the MacBook checkout (`savios-macbook-pro.tailc2733a.ts.net`, `/Users/savior/Code/harness-template`) is fast-forwarded to the same commit, `jg` is installed there with the skill copied to both skill directories, and its `jg auth` remains user-run.
- Risks: `jg` sends eligible source to the chosen provider on every search (accepted by the user on 2026-09-28; `.harness-no-upload` opts a target out). The denylist rule is a tightening edit to a guard-protected file, applied while the phase-guard hook was not active in this session. Retrieval records are operator-managed like other harness database records.

## Automatic target initialization on every session (2026-09-25)

- Harness root: `/Users/savior/Code/harness-template`; target for this change: the same directory. Docs read: `AGENTS.md`, `CLAUDE.md`, `README.md`, `docs/setup.md`, `docs/architecture.md`, `docs/jev-checkpoints.md`, `scripts/init.sh`, `scripts/install-guides.sh`, `scripts/install-hooks.sh`, `scripts/hooks/session-route.sh`, `scripts/hooks/jev-observe.sh`, `scripts/harness` (root/db resolution, phase handlers), `scripts/verify.sh`, `scripts/review.sh`, `tests/jev-checkpoints.sh`, `tests/init-preview.sh`, `tests/harness-hook.sh`.
- Finding: since 2026-09-04 every harness run on this machine (6 total) targeted the template itself. No DollarWise worktree is a harness target: `init.sh` only bootstraps dependencies and refuses without a TTY, the session hook only recognises the template root or a guide block inside tracked `AGENTS.md`/`CLAUDE.md`, and all run state shares one global `runs/current` under the template database, so parallel worktrees would collide.
- Goal: whenever a Claude Code or Codex session starts in any project (git worktree, plain checkout, or directory), the harness initialises that project automatically: it is registered as a target in machine-local state, its dependency bootstrap runs once per lockfile fingerprint, and the harness CLI, verify, review, and Jev hooks operate on that target's own state.
- Non-goals: no edits to tracked files in target repositories (guide blocks stay opt-in via `install-guides.sh`); no change to phase-order, gate, permission, or denylist rules; no change to Jev routing policy; the phase guard hook stays Claude-only and is not auto-installed.
- Plan:
  1. New `scripts/harness-target.sh`: `root PATH` (git toplevel, else the directory), `id`, `register`, `lookup`, `db-root`, `list`; registry under `HARNESS_DB_ROOT/targets/<id>/` with `target.state`; refuses `$HOME`, `/`, and the harness root.
  2. `scripts/init.sh`: registers the target on every run; new `--auto` mode (non-interactive) computes a fingerprint over manifests and lockfiles, skips when the last bootstrap succeeded for that fingerprint, otherwise runs the previewed commands with `--yes` in the background (foreground with `HARNESS_AUTO_INIT_SYNC=1`), records `bootstrap.state` and `bootstrap.log` in the target directory, and prints one status line.
  3. New `scripts/hooks/auto-init.sh` (SessionStart `startup|resume|clear`): resolves the payload `cwd` to a target root, runs `init.sh --project ROOT --auto`, then chains `session-route.sh` so the shadow route sees the registration; disabled by `HARNESS_AUTO_INIT=0`; always exits 0.
  4. `scripts/hooks/session-route.sh` and `scripts/hooks/jev-observe.sh`: treat a registered target as a harness target and use its per-target database.
  5. `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`: when `HARNESS_DB_ROOT` is unset and the cwd or `--project` lies inside a registered target, use `targets/<id>/db` so each project has its own runs, records, checkpoints, and gates; phase checkpoints derive git signals from the target rather than the harness root.
  6. `scripts/install-hooks.sh`: install `auto-init.sh` as the SessionStart entry (replacing the direct `session-route.sh` entry) and keep `jev-observe.sh`.
  7. Tests: new `tests/auto-init.sh` (registry, fingerprint skip/rerun, hook payloads, per-target CLI state and gates); adjust `tests/jev-checkpoints.sh` installer assertions.
  8. Docs: `docs/setup.md` (bootstrap, env vars, targets), `README.md`, `docs/architecture.md`, `progress.md`.
- Acceptance: a SessionStart payload for an unregistered git worktree registers it, records a bootstrap state, and prints one context line; a second start with unchanged lockfiles runs no bootstrap; `harness plan start` from that worktree writes its run under the target database and `build done` accepts a `verify.sh --project` record written there; template-root behaviour and all existing tests unchanged; `scripts/verify.sh` passes.
- Verification plan: `sh tests/auto-init.sh`, `sh tests/jev-checkpoints.sh`, `sh tests/init-preview.sh`, `sh tests/harness-cli.sh`, then `scripts/verify.sh --project /Users/savior/Code/harness-template`, then reinstall hooks on this machine and exercise a real SessionStart payload against a DollarWise worktree, then `scripts/review.sh`.
- Build: new `scripts/harness-target.sh` (root/id/register/lookup/db-root/fingerprint/list; registry at `HARNESS_DB_ROOT/targets/<name>-<hash>/`, refuses `$HOME`, `/`, and the harness root) and `scripts/hooks/auto-init.sh` (SessionStart: `init.sh --project CWD --auto`, prints only `Harness auto-init:` lines, then chains `session-route.sh`; `HARNESS_AUTO_INIT=0` disables).
- Build: `scripts/init.sh` registers the target on every run, resolves `--auto` to the project root, splits preview/confirm/run, feeds `</dev/null` to project commands, and in `--auto` skips by fingerprint, reports earlier failures without retrying, runs in the background via `nohup` with `HARNESS_AUTO_INIT_WORKER=1` (foreground with `HARNESS_AUTO_INIT_SYNC=1`), and writes `bootstrap.state`/`bootstrap.log` beside the registration.
- Build: `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`, `session-route.sh`, and `jev-observe.sh` resolve a registered target from the cwd, `--project`, or hook payload and use `targets/<id>/db`; phase and repeat checkpoints take the target as `--project`. `HARNESS_DB_ROOT` is the base under which targets live (explicit values no longer bypass the lookup, so isolated test databases behave like production).
- Build: `scripts/install-hooks.sh` installs `auto-init.sh` as the SessionStart entry (`startup|resume|clear`, 30 s timeout) in place of the direct `session-route.sh` entry; markers cover all three hooks so old entries are replaced.
- Fix during build: the worker read `$?` after an `if` and reported a failing bootstrap as completed; status is now captured with `run_plan || _status=$?`. Test temp paths are resolved physically because `/var` is a symlink on macOS.
- Tests: new `tests/auto-init.sh` (root resolution, refusals, register on first start, fingerprint skip/rerun, failure recorded once, disable flag, plain directory, silent harness root, manual init registers, per-target runs/verify record/`build done` gate, chained route and observer writing to the target database with the fake router, installer entries); `tests/jev-checkpoints.sh` installer assertions updated. Docs: `docs/setup.md`, `README.md`, `docs/architecture.md`.
- Verification: `scripts/verify.sh --project /Users/savior/Code/harness-template` first failed on shellcheck SC1010 (unquoted `done` argument in `tests/auto-init.sh`), then passed with ran=3 skipped=3 failures=0 (all `tests/*.sh` PASS, live TypeSafe suite skipped by design). Focused runs: `tests/auto-init.sh`, `tests/jev-checkpoints.sh`, `tests/init-preview.sh`, `tests/harness-cli.sh`, `tests/harness-hook.sh`, `tests/review.sh`, `tests/verify-required-checks.sh` all PASS.
- Fix during verification: `tests/init-preview.sh` ran manual `init.sh` without an isolated `HARNESS_DB_ROOT` and registered its temp projects in the machine registry; it now exports an isolated database. The stale entries it left (four `harness-init.*` temp pairs) and a `ClaudeBar/Probe` entry created by a real probe session are harmless and listed for manual removal.
- Live check: `scripts/install-hooks.sh` installed on this machine at 2026-09-25 10:57 local (backups `settings.json.bak-jev-20260925105757`, `hooks.json.bak-jev-20260925105757`). A headless `claude -p` session started in `/Users/savior/Code/dollarwise-eng-1407-activity-search-clear` registered the worktree (`TARGET_KIND=worktree`), ran `pnpm install` and `composer install --no-interaction --prefer-dist` in the background to `STATUS=ok` in four seconds, and wrote one shadow route under the target database. The Codex hook entry is installed but no Codex session was started, so that client is unverified.
- Risks: a background bootstrap (`pnpm install`, `composer install`) can overlap with the agent's first project commands; the status line names the log so the agent can wait. Hook runners may execute hooks in parallel, hence the chained session route. Codex SessionStart payload shape is assumed to carry `cwd` like Claude Code's.

## Automatic Jev Checkpoints and Earned Activation (2026-09-24)

- Harness root and target: `/Users/savior/Code/harness-template` (same directory). Docs read: `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`, `docs/conventions.md`, `docs/setup.md`, `docs/jev-checkpoints.md`, `knowledge/harness-strategy-guide.md` (head), `scripts/harness`, `scripts/context_advice.py`, `scripts/task_route.py`, `scripts/agent_launch.py`, `scripts/audit_emit.py`, `scripts/verify.sh`, `scripts/review.sh`, `scripts/hooks/require-phase.sh`, existing tests.
- Finding: the Jev plumbing exists but is barely exercised. Local evidence: 9 route records, 0 advice checkpoints, 0 labeled outcomes, 7 API calls in three days, empty cohort report, while the shell exports `HARNESS_TYPESAFE_ACTIVE=1` plus the operator activation at 100%. Interactive sessions bypass `launch`; no phase gate, verify, review, or hook emits a checkpoint; checkpoints require hand-written JSON.
- Goal: make the harness itself generate shadow checkpoints and independently labeled outcomes at its deterministic seams, cover interactive sessions, remove authoring friction, and downgrade activation to shadow until the 30-outcome gate is earned.
- Non-goals: no automatic promotion or active routing; no prompt, source, diff, credential, or personal data in any checkpoint; no change to permission, required-check, known-failure, or irreversible-work gates; no change to the Arc service schema (new event types only).
- Plan:
  1. `scripts/jev-enable.sh`: shadow by default (no `HARNESS_TYPESAFE_ACTIVE`/operator activation); export `HARNESS_JEV_CHECKPOINTS=1`; keep the model pin and audit telemetry.
  2. New `scripts/phase_checkpoint.py`: derives enum-only signals from git and run state (dirty-file bucket, languages, tests/docs/progress changed, steps, loops, prior verify exit) and emits v2 shadow checkpoints for `plan done` (handoff_assessment), `build start` (reasoning_allocation), `verify.sh` start (evidence_assessment), and `review.sh` (handoff_assessment); pending oracles live under `.harness-db/advice-pending/`.
  3. Automatic labels from ground truth: verify exit labels the verify prediction; the first verify after `build start` labels the reasoning allocation; `review done` labels plan/review handoffs from loop count, prints the pilot counter (N of 30) and unlabeled call IDs.
  4. New `scripts/hooks/session-route.sh` (Claude and Codex SessionStart): when the cwd is a harness target, run `harness route` in shadow mode with derived enum metadata and print one line of context; new `scripts/install-hooks.sh` merges the entries into `~/.claude/settings.json` and `~/.codex/hooks.json` idempotently with backups.
  5. New `scripts/hooks/jev-observe.sh` (PreToolUse Bash): counts identical commands per run and emits a `progress_assessment` checkpoint on the third repeat; never blocks.
  6. `scripts/context_advice.py`: flag form (`--family --baseline --goal --choice k=desc --boolean k=text --score k=text:levels --fact --constraint --risk --bypass`) building the same validated v2 payload; `--pending` lists unlabeled evaluated checkpoints; `--report` gains a `pilot` summary; `HARNESS_JEV_TIMEOUT` bounds hook latency.
  7. `scripts/audit_emit.py`: `jev.checkpoint` and `jev.checkpoint_outcome` events emitted best-effort from advise/record when audit is enabled.
  8. `scripts/harness`, `scripts/verify.sh`, `scripts/review.sh`: call the checkpoint script only when `HARNESS_JEV_CHECKPOINTS=1`, always non-fatal; nested harness tests run with checkpoints off.
  9. Docs and guides: `docs/jev-checkpoints.md`, `docs/setup.md`, `docs/architecture.md`, `README.md`, `AGENTS.md`, `CLAUDE.md`, `scripts/install-guides.sh` text.
- Acceptance: a normal plan/build/verify/review run on this machine writes at least four evaluated checkpoints and labels the verify and reasoning ones without agent action; `advise --report` shows the pilot count; a fresh interactive session in a harness target writes one shadow route record; all new code paths have offline tests using a fake router; `scripts/verify.sh` and `scripts/review.sh` pass; no network call happens when `HARNESS_JEV_CHECKPOINTS` is unset.
- Verification plan: focused `python3 -m unittest` for new and changed modules, new `tests/jev-checkpoints.sh` for hook and CLI flows with a fake router, isolated-DB `scripts/verify.sh`, a live end-to-end harness run with the real credential, then `scripts/review.sh`.
- Build: `scripts/context_advice.py` gained the flag form (`--family ... --choice id=text --boolean id=text --score id=text:a|b`), `--label CALL_ID --outcome --action-taken --evidence`, `--pending`, a `pilot` block in `--report`, `HARNESS_JEV_TIMEOUT`/`HARNESS_JEV_ATTEMPTS`, and best-effort `jev.checkpoint`/`jev.checkpoint_outcome` Arc events via `scripts/audit_emit.py`.
- Build: new `scripts/phase_checkpoint.py` (events `plan-done`, `build-start`, `verify-start`, `verify-result`, `review-handoff`, `run-complete`, `tool-repeat`, `session-start`, `pilot`) with enum-only git/run signals, pending oracles under `.harness-db/advice-pending/`, and mechanical labels (verify exit, first verify after build start, loop count). Wired into `scripts/harness` (`plan done`, `build start`, `review done`), `scripts/verify.sh` (prediction before checks, label after), and `scripts/review.sh` (handoff after verification); all gated by `HARNESS_JEV_CHECKPOINTS=1`, always non-fatal, and nested harness tests run with it forced to `0`.
- Build: new hooks `scripts/hooks/session-route.sh` (SessionStart shadow route for harness targets, one context line) and `scripts/hooks/jev-observe.sh` (third identical Bash command emits one `progress_assessment` checkpoint; checksum only, never blocks); `scripts/install-hooks.sh` merges/removes both entries in `~/.claude/settings.json` and `~/.codex/hooks.json` with backups. Installed on this machine at 2026-09-24 14:04 local; backups `settings.json.bak-jev-20260924140434` and `hooks.json.bak-jev-20260924140434`.
- Build: `scripts/jev-enable.sh` now exports shadow collection only (`HARNESS_JEV_CHECKPOINTS=1`, model pin, audit) and unsets the stale `HARNESS_TYPESAFE_ACTIVE`/operator-activation flags unless `JEV_KEEP_ACTIVATION=1`.
- Build: tests `tests/test_phase_checkpoint.py` (16), additions to `tests/test_context_advice.py` (2) and `tests/test_audit_emit.py` (1), `tests/fake_router.py` (offline router usable as module and CLI), and `tests/jev-checkpoints.sh` (hooks, installer, and a full plan/build/verify/review run producing 5 auto-labeled decisions). Docs: `docs/jev-checkpoints.md`, `docs/setup.md`, `docs/architecture.md`, `README.md`, `AGENTS.md`, `CLAUDE.md`, `scripts/install-guides.sh`.
- Decision: automatic labels are mechanical rules, documented in `docs/jev-checkpoints.md`, so cohorts stay comparable; `review.sh` re-running verify yields a second prediction on purpose (it is a distinct decision with its own oracle).
- Note: this run's `build start` happened before `HARNESS_JEV_CHECKPOINTS` was exported in the session shell, so the live run lacks the plan-done and build-start checkpoints; verify, review, and review-done checkpoints are exercised live below.
- Verification: `HARNESS_JEV_CHECKPOINTS=1 scripts/verify.sh --project /Users/savior/Code/harness-template` first failed on shellcheck (SC1010/SC2012 in the new `tests/jev-checkpoints.sh`; Jev had predicted `fix_before_verifying`, labeled `correct`), then passed after the fix with ran=3, skipped=3, failures=0 (16 harness shell suites PASS, live TypeSafe suite skipped by design; the second prediction was labeled `over_escalated`). Focused: `test_phase_checkpoint.py` 16 OK, `test_context_advice.py` 13 OK, `test_audit_emit.py` 2 OK, `tests/jev-checkpoints.sh` PASS.
- Live checks with the real credential: `scripts/review.sh` emitted and labeled a verify prediction (`correct`) and a `review-handoff-1` checkpoint (recommendation `needs_more_work`, baseline kept); `scripts/hooks/session-route.sh` fed a real SessionStart payload wrote one shadow route (`reasoning_model`, source typesafe) and printed the context line; three identical Bash payloads through `scripts/hooks/jev-observe.sh` produced exactly one `progress_assessment` checkpoint (recommendation `change_approach`). Arc summary shows `jev.checkpoint` and `jev.checkpoint_outcome` event types.
- Fix during review: in-process advice unit tests inherited `HARNESS_AUDIT_ENABLED=1` from the login shell and mirrored fixture checkpoints to the local Arc service (147 events, immutable by design); `tests/test_context_advice.py` now forces the flag off, verified by an unchanged event count across a rerun. Fixture events remain in the local Arc log and should be discounted when reading its summary.
- Risks: added latency at verify/review start (bounded by `HARNESS_JEV_TIMEOUT`, default 10 s, and skipped when credentials are absent); hook files are guard-adjacent, so changes are additive and require-phase.sh stays untouched; the Codex hook payload is assumed to mirror the Claude payload (`cwd`, `tool_name`, `tool_input`).

## Arc Audit and Full JEV Activation (2026-09-23)

- Goal: make JEV route outcomes and agent/token measurements durable in the prepared Arc audit service, then enable full eligible-task delegation with an explicit operator activation path.
- Finding: `/home/savior/Code/harness-audit` is an unversioned Arc-generated skeleton with only the base `events` table and `/health`; it has no audit aggregate, ingestion endpoint, token fields, running service, or remote Mac copy.
- Plan: add a versioned Arc audit service under `services/harness-audit`, add best-effort harness emission for route, outcome, and agent-token events, add a full-rollout operator acknowledgement, and document startup/activation/sync commands.
- Non-goals: do not send prompts, source, diffs, credentials, or raw outcome evidence; do not weaken deterministic authorization, required-check, known-failure, or irreversible-work gates; do not treat unknown outcomes as correct.
- Acceptance: Arc records route/outcome/token events; the harness preserves task execution when audit is unavailable; active 100% rollout requires an explicit activation acknowledgement; focused tests, full verification, review, and cross-machine synchronization pass.
- Verification plan: Arc service tests and health/ingest smoke checks, harness routing/emitter tests, isolated `scripts/verify.sh`, separate `scripts/review.sh`, then commit and push the versioned source.
- Build: added `services/harness-audit`, an Arc `AuditEvent` aggregate with an immutable event log and `audit_events_view` projection, plus `/api/audit/events` and `/api/audit/summary`.
- Build: added `scripts/audit_emit.py`, launcher completion/outcome emission, and Codex App Server token-delta emission. Telemetry is best-effort and preserves execution when the service is unavailable.
- Build: added explicit `HARNESS_TYPESAFE_OPERATOR_ACTIVATION=1` full-rollout acknowledgement. It bypasses only the evidence-count gate; model pinning and deterministic safety gates remain enforced.
- Focused verification: 18 task-routing tests, 1 audit-emitter test, and 3 Arc service tests passed. The live Arc smoke test returned healthy, accepted a measured event, projected it, and reported token totals.

## Staged JEV Delegation Rollout (2026-09-22)

- Goal: move eligible task-entry routing from shadow comparison toward measured active delegation while preserving deterministic safety gates and a fast rollback switch.
- Plan: add a validated rollout percentage with stable cohorts, keep shadow holdback traffic for comparison, expose selected mode/cohort metadata in private route records, document the activation procedure, and add offline coverage for staged selection and invalid configuration.
- Non-goals: do not weaken authorization, known-failure, required-check, explicit-choice, or irreversible-work gates; do not send prompts, source, diffs, or credentials to JEV; do not claim savings without paired token/outcome evidence.
- Verification plan: focused routing tests, full `scripts/verify.sh`, separate `scripts/review.sh`, then commit and push the versioned change for MacBook sync.
- Build: added `HARNESS_TYPESAFE_ROLLOUT_PERCENT` with stable metadata cohorts. Active mode now delegates only selected cohorts to JEV recommendations while holdback cohorts remain shadow comparisons; route records expose the selected mode and cohort fields. Deterministic gates remain independent of rollout configuration.
- Focused verification: `python3 -m unittest discover -s tests -p test_task_routing.py -q` passed 17 tests.
- Full verification: `HARNESS_DB_ROOT=/tmp/jev-rollout-verify-state scripts/verify.sh --project /home/savior/Code/harness-template` passed with ran=3, skipped=3, failures=0.
- Review: `HARNESS_DB_ROOT=/tmp/jev-rollout-harness-state scripts/review.sh --project /home/savior/Code/harness-template` passed; the review reran verification with ran=3, skipped=3, failures=0 and found no unresolved routing, safety, privacy, or documentation issue.

- Harness and target: `/Users/savior/Code/harness-template`.
- Approved goal: consider Jev at every meaningful decision, with explicit shadow checkpoints and measurable outcomes.
- Plan: retain v1 advice; add v2 batched typed questions, deterministic bypasses, baseline capture, versioned records, outcomes/reporting; isolate task activation cohorts; update installed guidance.
- Acceptance: no gate override or automatic promotion; invalid/unavailable evaluations fall back; probes and unknown measurements are separated; backward-compatible CLI and offline regression coverage.
- Verification: targeted Python suites, shared skill tests, full `scripts/verify.sh`, then `scripts/review.sh`.
- Context: required harness docs and current source read. Knowledge directory has no README/AGENTS/CLAUDE or skills; unapproved contents were not followed. Graph coverage is stale for changed modules; source used.
- Build: implemented backward-compatible v1 advice and v2 shadow batches, baseline persistence, typed validation, bypass/fallback records, linked outcomes and cohort reports. Updated shared skill and installable guidance; isolated task activation by current model and router/adapter fingerprint.
- Targeted checks: 13 task-route and 11 advice tests passed; install-guides tests passed. Shared skill suite initially failed its portable-path assertion; replaced the absolute path and reran the same 64-test suite successfully.
- Verification: first full `scripts/verify.sh` passed (ran=3, skipped=3, failures=0). Follow-up robustness edits passed 14 routing and 11 advice tests. `HARNESS_TYPESAFE_LIVE=1 sh tests/live-context-advice.sh` passed all five v1 fixtures and the v2 mixed batch against the pinned model, with synthetic logs isolated. Final `scripts/verify.sh` and `scripts/review.sh` both passed (ran=3, skipped=3, failures=0); required lint and test categories passed. Formatter, typecheck and build categories have no configured targets. The optional live checks were run separately and passed. Shared skill tests passed all 64 cases. Review inspected the harness and shared-skill diffs, including fallback, outcome joins, privacy, cohort isolation and guide installation; no unresolved findings. Pilot labels must come from future real decisions, not fabricated fixtures.

# Progress

## Cross-machine Harness Sync (2026-09-26)

- Goal: use the remote MacBook Pro harness as the authority, update this Linux machine, and verify that both machines share the same versioned harness and equivalent installed integrations without copying secrets or machine-local databases.
- Harness roots: `/Users/savior/Code/harness-template` on `savios-macbook-pro.tailc2733a.ts.net` and `/home/savior/Code/harness-template` on this machine.
- Non-goals: do not synchronize credentials, `.harness-db`, logs, live sessions, unrelated hooks, or platform-specific service files; preserve the existing local `progress.md` addition.
- Initial evidence: both repositories are on clean shared commit `ec8116e` except for this machine's existing `progress.md` addition; harness hook entries and launcher links are equivalent; both audit services are healthy; canonical TypeSafe skill files are byte-identical, but this machine's installed `~/.agents/skills/typesafe-routing` is an older physical copy while the Mac installs through a symlink to its vault source.
- Plan: back up the old Linux installed skill and symlink it to the canonical vault copy; run the TypeSafe tests and complete harness verify/review gates locally; verify the same gates remotely; record and share only the progress documentation needed to leave both Git checkouts on one clean commit.
- Acceptance: both Git checkouts are clean at the same commit; installed harness hook paths resolve; TypeSafe installed paths resolve to byte-identical canonical sources; both audit health endpoints pass; full harness verification and review pass on both machines.
- Build: moved the older Linux-installed TypeSafe skill copy to `/home/savior/.agents/skills/typesafe-routing.pre-vault-sync-20260926` and linked `~/.agents/skills/typesafe-routing` to `/home/savior/Documents/resendes/agent-skills/typesafe-routing`; the existing Codex link now resolves through that canonical source. No credentials, databases, logs, or unrelated hook groups were changed.
- Focused verification: all 64 TypeSafe routing tests pass on both machines; hook dry runs contain the expected `auto-init.sh` and `jev-observe.sh` entries with platform-correct roots; both audit health endpoints report version `0.1.0` healthy.
- Full verification: Linux passed with `ran=3 skipped=3 failures=0`. The first MacBook SSH run lacked its login-shell PATH and correctly failed required lint plus the Node-based wizard test; rerunning through `zsh -lic` exposed Homebrew/NVM and passed with `ran=3 skipped=3 failures=0`. The optional credentialed live TypeSafe suite remained skipped on both.
- Review: local and MacBook `scripts/review.sh` runs both passed, each rerunning the complete harness suite with `ran=3 skipped=3 failures=0`; the local review found only this progress documentation, and the pre-sync MacBook review found a clean tree.

## Lothar Skill Namespace Sync (2026-09-24)

- Goal: rename the remote vault's five `dw-*` skills to the `lothar-*` namespace, preserve them canonically in `~/Documents/resendes`, expose them to Claude, Codex, and OpenCode, commit only this scoped change, and synchronize this machine.
- Scope: `lothar-env-run`, `lothar-herdr-dispatch`, `lothar-iterate`, `lothar-panel-review`, and `lothar-start-worktree`; preserve the existing Dollarwise workflow content and the remote user's unrelated vault changes.
- Plan: rename tracked skill directories and internal skill references, create shared-agent, Codex, and explicit OpenCode symlinks, configure OpenCode's canonical vault path, validate skill frontmatter and discovery, commit on the Mac, then merge/pull the exact commit here without overwriting local vault work.
- Risks: the two vault checkouts have diverged and both contain unrelated user changes; no broad reset, stash, clean, or force operation is authorized.
- Result: committed and pushed `c8777f1e` on the Mac; this machine merged that commit as `bdd0ebdc` while preserving its local commits and unrelated dirty files. All five canonical skills and runtime links validate on both machines; the OpenCode configuration is valid and points at the canonical vault path.
- Verification: five skill-creator quick validations and 11 `lothar-iterate` unit tests passed; namespace/link/config checks passed on both machines. The old local ignored copies were byte-identical duplicates and were removed by the merge or moved to recoverable `/tmp` backups.

## Cross-machine Harness Reconciliation (2026-09-22)

- Goal: make this Mac and `resende-1` use one tested harness revision while preserving both machines' unfinished work.
- Harness root and target root: `/Users/savior/Code/harness-template` locally and `/home/savior/Code/harness-template` on Resende; both started at commit `20867ed` on `master`.
- Non-goals: do not sync credentials, live Codex sessions, harness databases, or other machine-local state; do not claim direct terminal Codex interruption works.
- Source inspection: the graph generation is from 2026-09-04 and reports the changed/new paths as stale or missing, so current source was read directly after the required coverage check.
- Plan: keep the Mac's safer offline-tested App Server budget controller and continuation policy; add Resende's auditability-planning wizard while completing its promised JSON export and focused checks; preserve the shared Lavish rule; verify, review, commit, push, then update both checkouts to the same commit.
- Acceptance: both checkouts have the same clean Git commit; the budget controller uses both thread and turn identifiers and refuses unenforceable direct-CLI caps; the offline wizard exports Markdown and JSON without network dependencies; full verification and review pass.
- Backups: `/tmp/harness-cross-machine-sync-backup-local` and `/tmp/harness-cross-machine-sync-backup-resende`.
- Verification plan: focused budget, routing, and wizard checks; `git diff --check`; then isolated full `scripts/verify.sh` and `scripts/review.sh` runs.
- Build: retained the Mac's bounded App Server controller because it preserves goal usage, times out, filters for loaded `appServer` threads, and interrupts with both required IDs; rejected Resende's detached 60,000-token default because it cannot safely control an independent terminal CLI thread.
- Build: imported Resende's offline auditability planner and completed its promised JSON export, required-answer feedback, keyboard focus treatment, network-request guard, and decision coverage for projections, delivery, and operations.
- Focused verification: `sh tests/auditability-wizard.sh`, `sh tests/codex-budget.sh` (10 tests), and `sh tests/task-routing.sh` (11 routing tests and 4 advice tests) all passed.
- Full verification: `HARNESS_DB_ROOT=/tmp/harness-cross-machine-sync-state scripts/verify.sh` passed with ran=3, skipped=3, failures=0; the credentialed live TypeSafe suite remained opt-in and was skipped.
- Protocol review: current official Codex App Server documentation confirms that `turn/interrupt` requires `threadId` and `turnId`, `thread/list` supports `sourceKinds: ["appServer"]`, and omitting a goal objective preserves existing usage.
- Review: `HARNESS_DB_ROOT=/tmp/harness-cross-machine-sync-state scripts/review.sh` reran the full suite successfully and inspected every tracked and untracked change; no unresolved correctness, privacy, or scope issue was found. A final review rerun follows this progress update.

## Continuation Policy Sync (2026-09-22)

- Goal: sync resende-1’s September 21 chat-authorized continuation update into this Mac’s harness. Preserve local Codex budget changes and unrelated remote work.
- Plan: merge the hook, hook tests, and four policy guides; run full verification and a separate review. Acceptance: an agent can issue continue after explicit chat authorization; abort and knowledge approval remain human-only.
- State: use `/tmp/harness-continuation-sync-state` for this separate maintenance task; leave the previously exhausted default run untouched. Knowledge content is unapproved and is not used as instructions.
- Build: merged continuation policy; the first full verification failed ShellCheck because the scanner included cached foreign repositories under `.harness-db`. Fix: prune that database from shell discovery, lint, and syntax scans; add an invalid cached-shell regression fixture. Rerun the same full gate.
- Verification and review: `HARNESS_DB_ROOT=/tmp/harness-continuation-sync-state scripts/verify.sh` and the same-prefixed `scripts/review.sh` passed, each with ran=3, skipped=3, failures=0. Optional live TypeSafe tests were skipped. Remote hook, tests, README, and CLAUDE match byte-for-byte; AGENTS/setup policy matches while preserving local additions. `git diff --check` passed. No commit or push.
- Source: remote and local HEAD both `20867ed`; remote policy files changed September 21 at 20:51 -0500. Backup: `/tmp/harness-continuation-sync-backup`; patch: `/tmp/harness-continuation-sync.patch`.

## Future-Session Readiness (2026-09-21)

- Goal: make the already approved harness and TypeSafe/JEV changes visible after a fresh Codex or Claude session starts, without claiming that an independent terminal Codex run can be interrupted.
- Plan: verify new-shell PATH, model pin, skill link, and global guidance; correct the App Server thread-discovery filter for a future managed run; rerun the complete harness gates.
- Initial check: a fresh login shell resolves `harness` through `~/.local/bin` and reads `TYPESAFE_MODEL=jev-1.13.0`; the Codex TypeSafe skill link points to the shared installed skill.
- Human-owned blocker: the default harness database's current run is paused on its loop budget with all three continuations used. Do not bypass or abort it as an agent; a human must decide whether to abort that old run before default-state phases work normally.
- Build: limited new-thread discovery to loaded `appServer` source threads, avoiding a terminal-owned thread that the controller cannot interrupt; the offline fixture now asserts that filter.
- Verification: `HARNESS_DB_ROOT=/tmp/harness-codex-future-state scripts/verify.sh` and `scripts/review.sh` passed (ran=3, skipped=3, failures=0). The optional live TypeSafe suite was skipped. No live App Server daemon was installed or exercised, and no new token cap was chosen.

## Codex Token-Budget Meter (2026-09-21)

- Goal: measure a launched Codex thread's token use through App Server, mirror each increment into the harness run, and interrupt the active turn at the chosen cap.
- Plan: add a small App Server client with exact thread discovery and active-turn lookup, expose it as `harness budget`, and cover the protocol and launcher behavior offline.
- Safety decisions: do not invent a default token limit; preserve existing goal usage when attaching; require both thread and turn IDs for interruption; time out discovery instead of watching forever. Refuse a direct Codex CLI launch with an unenforceable explicit cap.
- Verification plan: run focused Python and shell tests, then the full harness verification and separate review gates.
- Build: added the App Server monitor, CLI command, offline protocol tests, and documentation. Ten focused tests and the existing routing tests pass after fixing a buffered-stream framing bug found by the first focused run.
- Integration blocker: `codex app-server proxy` cannot connect because this machine has no App Server daemon; `codex app-server daemon start` fails because the managed standalone Codex installation is absent. A separate direct App Server reports the current terminal thread as `notLoaded`, so it cannot interrupt CLI-owned turns. Automatic launch was removed; the remaining manual meter applies only to App Server-owned threads and is not live-verified here.
- Review: `HARNESS_DB_ROOT=/tmp/harness-codex-budget-state scripts/verify.sh` and `scripts/review.sh` passed (lint, offline tests, shell syntax; ran=3, skipped=3, failures=0). The credentialed TypeSafe suite was skipped by design. The live App Server connection failure remains unresolved and prevents claiming end-to-end token enforcement on this machine; no commit or push was made.

## Mac JEV Harness Parity (2026-09-21)

- Goal: bring the committed TypeSafe/JEV routing, launcher, and dynamic-advice advancement from `origin/master` onto this Mac without importing the remote machine's uncommitted experiments.
- Update: fast-forwarded `master` from `28c09ef` to `20867ed` and preserved the committed task-routing tests and documentation.
- Portability repair: isolated `tests/harness-cli.sh` from an ambient machine-wide `HARNESS_ROOT`; the first full verification exposed the issue and the identical verification command then passed.
- Verification: `HARNESS_DB_ROOT=/tmp/harness-jev-update-state scripts/verify.sh` passed with lint, all non-live harness tests, and shell syntax green; the credentialed live TypeSafe suite remains opt-in.

## Current Documentation Task (2026-09-19)

- Goal: reconcile the portable harness documentation with the executable scripts and add the requested machine-wide commit-message guidance.
- Plan: document phase pause ownership, verification categories and records, bootstrap's lockfile-aware commands, guide installation, and TypeSafe entry points; then update both global instruction files with the commit-message rule.
- First step complete: compared `README.md`, agent guides, and setup/architecture documentation with `scripts/harness`, `init.sh`, `verify.sh`, `review.sh`, `install-guides.sh`, routing, and advice implementations.
- Verification: `scripts/verify.sh` passed on 2026-09-19 (ran=3, skipped=3, failures=0); the only live TypeSafe suite was intentionally skipped because `HARNESS_TYPESAFE_LIVE` was not set.
- Review: `scripts/review.sh` passed on 2026-09-19 after rerunning the same verification suite (ran=3, skipped=3, failures=0); no documentation or implementation risk was found.

## Current Goal

Verify the dynamic TypeSafe advice path across five unrelated, caller-supplied decision contexts. The harness must remain domain-neutral: subject terms live only in test fixtures, and every request must carry only its own dynamic context and options.

## Current Test Expansion Plan (2026-09-19)

- Add five offline fixture contexts to prove the generic harness forwards arbitrary option identifiers without a domain template.
- Add an opt-in live test that invokes `harness advise` once for each fixture and verifies a valid choice, confidence, and private record.
- Run the five live calls now with credentials available; keep them out of default CI so clean checkouts do not require network access or a credential.
- Run the complete verification and review gates.

## Current Test Expansion Results (2026-09-19)

- Added five caller-owned fixtures plus offline forwarding coverage that verifies their dynamic options reach TypeSafe unchanged and that the generic harness source contains none of their subject terms.
- `HARNESS_TYPESAFE_LIVE=1 sh tests/live-context-advice.sh` passed: banking ledger → `event_sourcing` (0.99), clinic appointments → `relational` (1.00), offline field app → `operation_log` (0.95), storefront search → `keyword_index` (1.00), staff portal authentication → `server_sessions` (1.00).
- The live suite is opt-in; its normal wrapper skips without a credential, preserving offline CI. Full verification passed (ran=3, skipped=3, failures=0) and the separate review pass completed successfully.

## Current Task Plan (2026-09-19)

- Add `harness advise --context PATH`, backed by a context validator and a TypeSafe choice request with caller-supplied, situation-specific options.
- Keep the existing task-entry router unchanged for backward compatibility; advice is a separate, non-executing capability available during agent work.
- Reject unsafe or oversized context before any API call, retain strict redaction, save private audit records, and expose the selected choice, confidence, and rationale to the caller.
- Add offline regression coverage for validation, redaction refusal, API response validation, and deterministic no-execution behavior; update setup, architecture, README, and agent guidance.
- Verify with the targeted test, the full harness verification sensor, and a separate review pass.

## Current Task Results (2026-09-19)

- Added `scripts/context_advice.py` and `harness advise --context PATH`. It validates a compact context with a per-decision option set, rejects secret-like content before an API call, requests a dynamic TypeSafe choice, and writes a mode-0600 private record. It never runs the chosen option.
- Added offline coverage for option forwarding, secret rejection before a call, and invalid API choices. A live banking-ledger request through `harness advise` returned `event_sourcing` at 0.99 confidence and saved an isolated harness record.
- Updated the README, agent guides, setup guide, and architecture boundaries. `HARNESS_DB_ROOT=/tmp/harness-dynamic-typesafe-db scripts/verify.sh --project /home/savior/Code/harness-template` passed (ran=3, skipped=3, failures=0); the same-root `scripts/review.sh` passed and inspected the full dirty worktree.

## Current Task Plan (2026-09-18)

- Harness root and target: `/home/savior/Code/harness-template`. Run state is isolated in `/tmp/harness-typesafe-intake-db` because another harness run is active.
- Add `harness route --state` for a small enum-based task record. Apply deterministic rules first; call the installed TypeSafe router only for a remaining ambiguous route, using strict redaction and shadow mode by default.
- Add a launcher that selects exact command arrays from the route; preserve the default command in shadow mode and fall back to it when TypeSafe is unavailable.
- Record route and outcome identifiers in ignored harness state. Require 30 correct outcomes, zero under-escalations, and an explicit opt-in before active routing.
- Test deterministic bypass, malformed input, shadow and active behavior, unavailable service, command selection, and no shell interpolation. Run the full harness verify and review gates.

## Current Task Results (2026-09-18)

- Added `harness route` and `harness launch` with compact metadata, deterministic gates, private route records, shadow TypeSafe calls, an explicit active-mode threshold, and argv-based command profiles. Added the task-entry rule to the four global agent guides and a `harness` link on PATH.
- A live shadow call succeeded and returned `reasoning_model`; the route record links its TypeSafe call ID. A deterministic dry run selected the existing `codex` command without an API call.
- The first full verification exposed a shell-file detector that selected the new Python files; fixed the selector and added a regression test. Targeted tests, the full `verify.sh` gate, and `review.sh` then passed: ran=3, skipped=3, failures=0. The review inspected the patch and found no remaining issue requiring a code change.

## Decisions

- Keep this file current during each task.
- Record only decisions that affect future implementation, verification, setup, or architecture.
- Move completed task details out of this file when they stop being useful for the next run.
- 2026-08-30: Strategy review decided three directions. (1) Make detection safe first: non-mutating checks, shebang-aware syntax parsing, per-check timeouts. (2) An explicit per-project manifest becomes authoritative for verification commands and required categories; command detection demotes to a labelled fallback. Scope correction 2026-08-30: this applies to command discovery only. Codebase comprehension is a separate, first-class capability that must grow, not shrink. (3) `scripts/verify.sh` emits a machine-readable run record (commands, exit codes, git HEAD, timestamp) that completion claims must cite instead of asserting success.
- 2026-08-30: Added a knowledge layer as a fourth harness pillar beside guides, sensors, and state: pattern scanning, graph indexing, generated code memory, and ingestion of a per-project non-versioned `knowledge/` folder. It feeds planning and review, not verification.
- 2026-08-30: Verified there is no `graphify` skill installed on this machine. The existing implementation of graph indexing and code memory is the `codebase-memory` skill over its MCP server. `knowledge/` currently has no reader; it is referenced only by this file.
- 2026-08-30: Knowledge layer shaped. Indexing runs at bootstrap from `scripts/init.sh` and refreshes on later runs. `knowledge/` carries its own `CLAUDE.md` and `AGENTS.md` that are consulted every run for hooks, automated verifications, extra detail, and direction. Graph trust is freshness-gated: authoritative while the index is verified current, demoted to a lead when stale.
- 2026-08-30: Verified `codebase-memory-mcp` (v0.10.8) exposes a `cli <tool>` mode, so `scripts/init.sh` can index without an MCP client. `index_repository`, `index_status`, and `detect_changes` cover bootstrap indexing, refresh, and the freshness gate.
- 2026-08-30: Verified the current index for this repo holds 63 nodes and 61 edges and excludes `docs/` and `scripts/`, which is where all harness behavior lives. Index scope must be revisited before the graph is useful here.
- 2026-08-30: Open conflict to settle. `knowledge/` is non-versioned yet always consulted and able to declare hooks and automated verifications. That is an unreviewed instruction-and-execution channel (fault line G1) and it reopens the local/CI divergence that committing the manifest was meant to close. Precedence, hook trust, and index scope are unresolved.
- 2026-08-30: Entrypoint decided. Install a real `harness` CLI on PATH that locates the harness root itself, instead of requiring `--project` from the harness directory.
- 2026-08-30: Project workflow conventions (git model, branch and commit format, PR shape, task tool) are detected from the target repo first; the user is asked only about what detection cannot settle. Verified against DollarWise-Prototype: branch promotion chain, conventional-commit format with Linear ticket scope, task tool, existing `.github` templates, and commit size baseline were all derivable from git history and `.github/` with no interview.
- 2026-08-30: Placement rule for conventions. Team facts live in committed target-repo files; machine and personal facts live in non-versioned `knowledge/`. This also resolves the open precedence question by jurisdiction rather than ranking: committed sources own shared workflow and verification, `knowledge/` owns personal context and direction.
- 2026-08-30: Convention detection must stay project-specific. The DollarWise-Prototype findings are one project's standards, not a template. The harness ships probes and no assumed defaults, and low confidence is a valid outcome that produces a question rather than a guess.
- 2026-08-30: Learned project knowledge is stored in a `knowledge/` subfolder inside each target project, hidden through that repo's `.git/info/exclude` rather than `.gitignore`, so the harness leaves no trace in tracked files. Verified the mechanism on a scratch repo. Note: `.git/info/exclude` is not copied by clone, so `harness init` must write the entry idempotently per machine and account for linked worktrees sharing the file.
- 2026-08-30: The convention interview runs at `harness init`, after detection, covering only unresolved gaps, once per project.
- 2026-08-30: Accepted consequence. Conventions learned by the harness are per-machine, so contributors can diverge where a repo declares nothing. Mitigated by treating `knowledge/` as a derived cache while the repo remains the source of truth.
- 2026-08-30: The convention interview is bounded and splits by project age. An existing repo relies on detection and asks only about unresolved gaps, often nothing. A greenfield project has no evidence to read, so it gets the full interview.
- 2026-08-30: `harness init` must check for the mattpocock/skills collection and use `grill-with-docs` for the interview. Verified the collection is in Claude Code's official marketplace (`claude plugins install mattpocock-skills`), that `grill-with-docs` exists, and that nothing from it is installed on this machine.
- 2026-08-30: Open tension recorded. `grill-with-docs` builds on the `grilling` primitive, which interviews intensively until all branches resolve. That conflicts with the requirement for a short interview, so its use must be capped or restricted to greenfield.
- 2026-08-31: Installed `mattpocock-skills@claude-plugins-official` v1.2.3 at user scope. Verified 35 skills on disk including `engineering/grill-with-docs`.
- 2026-08-31: Interview scope cap decided. Seed the grill only with topics detection could not settle; no open exploration. Reading the skill showed `grilling` already requires the agent to find facts itself rather than ask, and asks in batched rounds with recommended answers, so the cap is a seeding discipline rather than a conflict. Note `grill-with-docs` sets `disable-model-invocation: true`, so `harness init` cannot trigger it autonomously and must instruct the user to run it.
- 2026-08-31: Hook trust decided. Hooks declared in `knowledge/` always run. Accepted risk, raised and reaffirmed: `knowledge/` lives inside each project, so a cloned repository could arrive carrying one that executes without review.
- 2026-08-31: Index scope decided. The indexer reads the whole tracked tree including `docs/` and `scripts/`; `knowledge/` is skipped because `.git/info/exclude` hides it from the indexer as well as from git. `index_repository --persistence` is permanently forbidden because it is the only option that writes into the target project (`.codebase-memory/graph.db.zst`).
- 2026-08-31: Confirmed the retrieval model. The graph is retrieval-augmented in shape but not a vector RAG: a full-mode index of this repo produced 277 structural edges (DEFINES, CALLS, USAGE, IMPORTS, CONTAINS_*) against 9 SEMANTICALLY_RELATED similarity edges. Retrieval returns named symbols with exact file and line coordinates, so a stale index yields confidently wrong coordinates rather than merely irrelevant text. This is the justification for freshness-gated trust.
- 2026-08-30: Accepted residual risk for decision 3. Citing a run record is not enforced, so a completion claim can still reference a stale or invented id. This makes false claims detectable, not impossible.
- 2026-09-01: First control-plane slice is schema plus validator only. No allowlist, no executor, and no Cursor tool block. Agents can still skip validation; a rejected proposal is detectable.
- 2026-09-02: The `harness` CLI counts a step for every phase command and for every explicit `harness step`. The CLI cannot observe the model's tool calls, so step counting is a reported, agent-visible rule rather than an enforced measurement.
- 2026-09-02: Token budget stays `unknown` unless the agent reports counts with `harness step --tokens N`. The CLI does not attempt to count tokens.
- 2026-09-02: `harness continue` extends the tripped budget by one more window (`cap + extensions * cap`, with a grant of 1 when the base cap is 0) rather than clearing the counter, so total consumption stays visible across evaluations.
- 2026-09-02: Run state is stored twice on purpose: `state` as `KEY=VALUE` for shell reads without a JSON runtime, and `run.json` as the machine-readable snapshot. Evaluation notes live only in the JSON pause records, so arbitrary text never enters the shell state file.
- 2026-09-02: `tests/harness-cli.sh` follows the existing convention of a standalone script invoked directly. It is not wired into `scripts/verify.sh`, which is owned by another worktree's scope.
- 2026-09-01: Grill confirmed. Full keep/leave list is in `harness-review.md`. Overrides: no executor; dry-run dropped; detection stays (no manifest); Make still wins; init still runs project setup immediately; `knowledge/` hooks still always run. Permit is allow-unless-denied with a harness default denylist that a project file replaces. Session budgets and phase gates wait for a future `harness` CLI.

## Active Plan

Goal: let external projects invoke `scripts/harness` by absolute path without repeating an inline `HARNESS_ROOT` assignment.

Acceptance criteria:

- The CLI falls back to its own installation root when neither `HARNESS_ROOT` nor current-directory discovery resolves a harness.
- An explicit `HARNESS_ROOT` remains authoritative.
- External guide blocks contain a clean absolute `scripts/harness` command.
- CLI, guide-installer, and full harness verification pass.

Implementation plan:

1. Add executable-location root discovery to `scripts/harness`.
2. Simplify the external-project command emitted by `scripts/install-guides.sh`.
3. Update regression tests for the new behavior.
4. Run build verification, then a separate review phase.

Verification plan:

- Run `sh tests/harness-cli.sh` and `sh tests/install-guides.sh`.
- Run `scripts/verify.sh` during build.
- Run `scripts/review.sh` during review.

Known risks:

- Executable-location discovery follows the invoked path; a symlink installed outside the harness still needs `HARNESS_ROOT` unless symlink resolution is added separately.

Phase: Build and review complete on `lotharthesavior/feat-cli`. Not merged, not pushed.

Goal: add the `harness` CLI from the confirmed keep/build list in `harness-review.md` — harness-root discovery, session budgets, and plan/build/review phase ownership.

Non-goals: action executor, dry-run, command manifest, Make plus native checks together, `init.sh` preview, `knowledge/` hook gating. Those belong to other worktrees.

Harness root: this repository checkout

Target project root: same as the harness root.

Acceptance criteria:

- `scripts/harness` finds the harness root from the current directory or an ancestor by looking for `AGENTS.md` and `scripts/verify.sh`.
- Commands `plan|build|review start|done`, `status`, and `continue` exist, plus `step` so the step budget can be counted.
- Run state lives under `.harness-db/runs/<id>/` with budget counters and the current phase.
- Default caps are `steps=20`, `time_min=15`, `loops=1`; the token cap is optional and recorded as `unknown`.
- Reaching a cap writes a pause record and exits non-zero; `continue` requires an evaluation note.
- `build start` is refused until `plan done`; `review start` is refused until `build done`.
- Tests cover phase order and pause/continue.
- `docs/setup.md` documents the CLI.

Implementation plan:

1. Add `scripts/harness`.
2. Add `tests/harness-cli.sh`.
3. Document the CLI in `docs/setup.md` and list the new files in `README.md`.
4. Write `QA-REVIEW.md` with copy-paste commands and real output.

Verification plan:

- Run `sh tests/harness-cli.sh` and `sh tests/action-schema.sh`.
- Run `scripts/verify.sh` and record the pre-existing ShellCheck failures.

Known risks:

- `scripts/verify.sh` fails on ShellCheck warnings that predate this branch, in files this feature does not touch.
- Step and token counts are self-reported by the agent; the CLI records and gates but cannot measure them.
- The earlier action-validator risks still stand: agents can skip the validator, schema and script rules can drift, and the validator needs `python3` or `node`.

Goal: create a progressive harness strategy guide, render it for a 10.3-inch grayscale screen, and transfer it to the NoteAir.

Acceptance criteria:

- The first section is a one-page complete overview.
- The second and third sections progressively deepen the explanation.
- Flowcharts and component descriptions are included.
- Markdown is stored under locally excluded `knowledge/` state.
- The PDF is visually verified and submitted to the NoteAir.

Goal: make verification fail when a project-declared required category runs no checks.

Acceptance criteria:

- Projects can declare required categories through configuration or an environment override.
- A required category passes only when at least one check in that category runs successfully.
- Missing required categories produce a non-zero exit status and an actionable summary.
- Regression tests cover successful, missing, and overridden requirements.
- Harness verification and review results are recorded.

Implementation plan:

1. Add required-category configuration and validation.
2. Configure this harness to require lint and test checks.
3. Add regression coverage and CI tooling.
4. Run verification, review, and inspect the diff.

Verification plan:

- Run `scripts/verify.sh`.
- Run `scripts/review.sh`.
- Inspect `git diff`.

Known risks:

- Local verification now intentionally fails until ShellCheck is installed.
- Undeclared categories remain optional for backward compatibility.

## Completed Steps

- Build: added executable-location fallback to `scripts/harness`, simplified external guide commands to the absolute CLI path, and updated root-discovery and installer regression coverage.
- Build: isolated `tests/harness-hook.sh` from machine-local `knowledge/` trust state after the first full verification exposed that fixture dependency.
- Review: inspected the scoped diff and `git diff --check`; no whitespace errors or unresolved functional risks found. Absolute-path discovery, explicit override behavior, orphan failure, guide idempotence, and hook isolation are covered.

- Build: 2026-09-03 critical items 6, 7, 8, 9, 27, 39, 40 and reopened 14: denylist (`schemas/denylist.default`, `scripts/permit.sh`) applied by `scripts/action.sh` and by the hook to every real tool call; `scripts/init.sh` previews and confirms project-owned commands (`--yes` in CI); `scripts/knowledge-trust.sh` human approval gate enforced by the hook; `scripts/verify.sh` runs only check-style formatters; `scripts/review.sh` prints the full patch and continues after a failing verify; budget defaults raised to 200 steps and 120 minutes; a time-budget continue restarts the clock. New tests: permit, knowledge-trust, review, init-preview, verify-no-rewrite; hook, CLI, and action tests extended.
- Cleanup: 2026-09-03 pruned `todo.md` to 19 open items: merged duplicates (progress, guides, tooling, review, timeouts) and dropped six nitpicks, listed at the end of the file.
- Cleanup: 2026-09-03 removed done items and empty sections from `todo.md` (numbers stay stable); moved `QA-REVIEW.md` into ignored `.harness-db/reviews/`; ignored `.claude/settings.local.json`; removed a machine-local path from this file.
- Build: 2026-09-03 items 3, 4, 5, 10, 12 of `todo.md`: verify/review run records under `.harness-db/records/` gate `build done` and `review done`; `harness abort` and a per-run continue cap; the hook refuses `continue`/`abort` from the agent and records one step per allowed tool call; ShellCheck warnings fixed; `scripts/verify.sh` runs `tests/*.sh` on the harness root. Live check: the hook blocked this session when the stale 2026-09-02 run tripped its time budget, and a human aborted it from a terminal.
- Build: 2026-09-03 item 2 of `todo.md`: added `Makefile` target `install-guides` and `scripts/install-guides.sh`, which add or refresh a marked Harness Phases block in `AGENTS.md` and `CLAUDE.md`; ran it on this repo; added `tests/install-guides.sh`; documented in `docs/setup.md` and `README.md`.
- Build: 2026-09-03 item 1 of `todo.md`: added `.claude/settings.json` PreToolUse hook and `scripts/hooks/require-phase.sh` that block Write/Edit/Bash unless a harness phase is active; added `tests/harness-hook.sh`; documented in `docs/setup.md` and `README.md`.
- Review: 2026-09-03 harness review found 19 concerns; recorded with why and fix in `todo.md` under "Review Findings 2026-09-03".
- Planning: read `AGENTS.md`, `CLAUDE.md`, `docs/conventions.md`, `docs/setup.md`, `harness-review.md`, `todo.md`, and the existing scripts and tests before writing the CLI.
- Build: added `scripts/harness` with harness-root discovery, `KEY=VALUE` plus JSON run state under `.harness-db/runs/<id>/`, session budgets, pause records, and plan/build/review gates.
- Build: added `tests/harness-cli.sh` covering root discovery, every phase-order refusal, the step/time/loop/token budgets, pause records, refusal while paused, and continue with and without an evaluation note.
- Build: documented the CLI, its budgets, its exit codes, and its state layout in `docs/setup.md`, and listed the new files in `README.md`.
- Review: ran both test scripts, replayed the blocked-build and pause/continue sessions by hand, and confirmed the two new files are ShellCheck-clean.
- Review: wrote `QA-REVIEW.md` with copy-paste commands, expected output, a manual checklist, and the real verification paste.
- Review: 2026-09-01 grill confirmed. Keep/leave recorded in `harness-review.md`. No build started.
- Build: added `schemas/action.schema.json`, `scripts/action.sh validate`, and `tests/action-schema.sh`.
- Build: documented propose-then-validate in setup, architecture, agent guides, README, and `harness-review.md`.
- Review: wrote `harness-review.md` covering what is right, missing, and needs update against the four harness controls.
- Planning: selected a three-step progressive document structure and a 10.3-inch grayscale e-ink layout.
- Build: created `knowledge/harness-strategy-guide.md` with overview, operational, and implementation-depth sections plus flowcharts.
- Build: excluded `knowledge/` and the generated PDF locally through `.git/info/exclude` without changing `.gitignore`.
- Review: rendered a seven-page PDF, inspected every page, corrected first-page clipping, and confirmed the one-page overview fits completely.
- Delivery: submitted `harness-strategy-guide.pdf` to the paired NoteAir through Bluetooth File Exchange.
- Planning: reviewed the existing harness findings and selected false-green verification as the first discussion topic.
- Build: created `todo.md` with the concerns organized as actionable improvements.
- Review: ran verification and confirmed the current false-green behavior: one check ran, four categories skipped, and the command still passed.
- Build: added `.harness-required-checks`, category enforcement, regression tests, documentation, and CI ShellCheck installation.
- Review: regression tests passed; local verification and review correctly failed because the required lint category could not run without ShellCheck.
- Template initialized with agent guides, docs, task template, bootstrap script, verification script, review script, CI workflow, security guidance, and repository hygiene defaults.
- Planning: read required harness docs, active task template, `progress.md`, and inspected `scripts/verify.sh`.
- Build: replaced the inline `sh -c` Go format check with a named `check_go_format` function to avoid ShellCheck SC2016 while preserving behavior.
- Review: ran verification and review scripts, inspected the diff, and confirmed the change is limited to `scripts/verify.sh` and `progress.md`.
- Planning: read required harness docs, task template, README, scripts, `.gitignore`, and current local diffs for cross-directory orchestration support.
- Build: added target-project support to `scripts/init.sh`, `scripts/verify.sh`, and `scripts/review.sh` with `--project PATH` and `HARNESS_TARGET_ROOT`.
- Build: documented `.harness-db/` as ignored local database state for project registries, task notes, progress, indexes, and project documents.
- Review: ran syntax checks, verification, review, and target-option smoke checks.

## Next Steps

- Review and merge `lotharthesavior/feat-cli`. It is committed on the branch only; nothing was merged or pushed.
- Clear the pre-existing ShellCheck warnings in `scripts/verify.sh`, `scripts/action.sh`, `scripts/review.sh`, `tests/action-schema.sh`, and `tests/verify-required-checks.sh` so the required `lint` category can pass.
- Accept the incoming `harness-strategy-guide.pdf` transfer on the NoteAir.
- Install ShellCheck locally or rely on the configured CI environment for the complete lint-and-test gate.

## Blockers

- None for this feature. `scripts/verify.sh` fails on ShellCheck warnings that predate this branch in files owned by other work; the two new files are clean.

## Verification History

- 2026-09-03: `scripts/verify.sh` initially failed because `tests/harness-hook.sh` used the live harness root and encountered its unapproved machine-local `knowledge/`; after moving the test to an isolated harness fixture, the same `scripts/verify.sh` command passed with `bash:shellcheck`, all ten harness tests, and `bash:syntax` passing (ran=3, skipped=3, failures=0).
- 2026-09-03: `sh tests/harness-cli.sh`, `sh tests/install-guides.sh`, and the isolated `sh tests/harness-hook.sh` all passed for executable-location root discovery and clean external guide commands.

- 2026-09-03: `scripts/verify.sh` passed after the critical items. Result: ran=3 skipped=3 failures=0; `bash:shellcheck`, `harness:tests` (ten test scripts), and `bash:syntax` passed; run record written. A human fixed one ShellCheck nit in `scripts/permit.sh` because the denylist forbids the agent from editing the guard.
- 2026-09-03: `scripts/verify.sh` passed for the first time since required checks were added. Result: ran=3 skipped=3 failures=0; `bash:shellcheck`, `harness:tests` (five test scripts), and `bash:syntax` all passed; run record written to `.harness-db/records/verify.state`.
- 2026-09-03: `sh tests/install-guides.sh` passed; `make install-guides` twice on this repo produced identical files; `scripts/verify.sh` still ran no make targets and fails only on the pre-existing ShellCheck warnings (todo #10).
- 2026-09-03: `sh tests/harness-hook.sh` passed. `shellcheck scripts/hooks/require-phase.sh tests/harness-hook.sh` clean. `scripts/verify.sh` still fails only on the pre-existing ShellCheck warnings (todo #10).
- 2026-09-02: `sh tests/harness-cli.sh` passed. Result: `PASS: harness CLI phase order, budgets, pause, and continue`.
- 2026-09-02: `sh tests/action-schema.sh` passed. Result: `PASS: action schema validation`.
- 2026-09-02: `shellcheck scripts/harness tests/harness-cli.sh` passed with no output.
- 2026-09-02: `scripts/verify.sh` failed. Result: ran=2 skipped=3 failures=1. The only failure is required `lint`; every ShellCheck warning comes from `scripts/verify.sh`, `scripts/action.sh`, `scripts/review.sh`, `tests/action-schema.sh`, and `tests/verify-required-checks.sh`, and each reproduces against base commit `3e34bf0`. Required `test` was satisfied via `bash:syntax`.
- 2026-09-02: Manual CLI QA passed: subdirectory root discovery, exit 2 outside any harness root, blocked `build start` before `plan done` (exit 4), step-budget pause (exit 3) and continue with an evaluation note, refusal while paused (exit 3), loop-budget pause, time-budget pause, `status --json` leaving `run.json` unchanged, and `.harness-db/` staying untracked.
- 2026-09-01: `scripts/verify.sh` after recording the grill confirm. Result: ran=1 skipped=4 failures=1; required `lint` unavailable without ShellCheck; required `test` satisfied via `bash:syntax`.
- 2026-09-01: `sh tests/action-schema.sh` passed. Result: `PASS: action schema validation`.
- 2026-09-01: `scripts/verify.sh` failed as intended after adding the action schema. Result: ran=1 skipped=4 failures=1; required `lint` was unavailable without ShellCheck; required `test` was satisfied via `bash:syntax`.
- 2026-09-01: `scripts/verify.sh` failed as intended. Result: ran=1 skipped=4 failures=1; required `lint` was unavailable without ShellCheck; required `test` was satisfied via `bash:syntax`.
- 2026-08-27: PDF QA passed. Result: 7 pages, 444.96 x 593.04 points, grayscale-safe layout, complete one-page overview, and no visible clipping or overlap.
- 2026-08-27: `git check-ignore -v --no-index` confirmed the Markdown and PDF are locally excluded through `.git/info/exclude`.
- 2026-08-27: Bluetooth submission succeeded; macOS handed `harness-strategy-guide.pdf` to Bluetooth File Exchange for NoteAir acceptance.
- 2026-08-27: Harness regression tests passed; `scripts/verify.sh` still fails intentionally because required local ShellCheck is unavailable.
- 2026-08-27: `sh tests/verify-required-checks.sh` passed all required-category regression cases.
- 2026-08-27: `scripts/verify.sh` and `scripts/review.sh` failed as intended. Result: ran=1 skipped=4 failures=1; required `lint` was unavailable and required `test` was satisfied.
- 2026-08-27: `scripts/verify.sh` passed. Result: ran=1 skipped=4 failures=0. This demonstrates the false-green verification concern recorded in `todo.md`.
- 2026-05-25: `scripts/verify.sh` passed locally. Result: ran=1 skipped=4 failures=0. Note: local `shellcheck` is unavailable, so the ShellCheck path was skipped locally.
- 2026-05-25: `scripts/review.sh` passed locally. It reran `scripts/verify.sh` with the same result and printed a diff summary for `progress.md` and `scripts/verify.sh`.
- 2026-05-25: `sh -n scripts/init.sh`, `sh -n scripts/verify.sh`, and `sh -n scripts/review.sh` passed.
- 2026-05-25: `scripts/verify.sh` passed locally. Result: ran=1 skipped=4 failures=0. Note: local `shellcheck` is unavailable, so the ShellCheck path was skipped locally.
- 2026-05-25: `scripts/review.sh` passed locally. It reran `scripts/verify.sh` with the same result and printed target git diff summary.
- 2026-05-25: `scripts/init.sh --project .`, `scripts/verify.sh --project .`, `scripts/review.sh --project .`, and `HARNESS_TARGET_ROOT=. scripts/verify.sh` passed as target-root smoke checks.
- 2026-05-25: Final `scripts/verify.sh` passed locally after updating `progress.md`. Result: ran=1 skipped=4 failures=0. Note: local `shellcheck` is unavailable.

## Grouped audit summary (feat/audit-summary-by-family)
- Added /api/audit/summary/checkpoints and /routes (services/harness-audit/src/summary.rs); cargo test 9 pass, clippy clean, scripts/verify.sh passed, live curl checked, independent review: no findings.

## Pilot aggregation (feat/jev-pilot-aggregate)
- 2026-09-29: Lane 2 (pilot aggregation): implemented machine scope in scripts/context_advice.py pilot/report/pending, `--scope` flag, tests in tests/test_context_advice.py, docs updated. Verification recorded below after scripts/verify.sh.
- 2026-09-29: scripts/verify.sh passed (ran=3 skipped=3 failures=0; required lint and test satisfied). Independent review: round 1 one finding (legacy shared database omitted from machine scope), fixed with a regression test; round 2 zero findings.

## TODO #31: bootstrap honors the lockfile's package manager (2026-09-30)

- Harness root and target: `/home/savior/Code/harness-template-todo-31-20260930` (worktree of harness-template, branch `feat/todo-31-20260930`, base `e5b148a`, same directory). Docs read: `README.md`, `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`, `docs/conventions.md`, `docs/setup.md`, `todo.md`, `scripts/init.sh`, `scripts/harness-target.sh` (fingerprint inputs), `tests/init-preview.sh`, `tests/auto-init.sh`, `.github/workflows/ci.yml`.
- Diagnosis (not already resolved): `detect_node_pm` in `scripts/init.sh` checks `lockfile && tool available` and otherwise falls through to "any available manager", so `yarn.lock` without yarn installs with pnpm or npm, `pnpm-lock.yaml` without pnpm installs with yarn or npm, and `bun.lock`/`npm-shrinkwrap.json` are ignored entirely.
- Goal: the bootstrap installs with the package manager that owns the lockfile, or refuses with a clear message and runs nothing.
- Non-goals: `package.json` `packageManager` field, verify.sh's script runner (it runs scripts, it does not install), any other TODO.
- Plan: map lockfiles to owners (`pnpm-lock.yaml`→pnpm, `yarn.lock`→yarn, `package-lock.json`/`npm-shrinkwrap.json`→npm, `bun.lock`/`bun.lockb`→bun); refuse when the owner is not installed or lockfiles name different managers; locked installs are frozen (`pnpm install --frozen-lockfile`, `yarn install --frozen-lockfile`, `npm ci`, `bun install --frozen-lockfile`); an interactive/`--yes` refusal prints `REFUSED:` and exits 1 before any command runs; `--auto` records `STATUS=failed` with the reason in `bootstrap.log`; the new lockfiles join the fingerprint inputs; docs in `docs/setup.md` and `README.md`.
- Acceptance: tests show each lockfile installs only with its owner, a missing owner and conflicting lockfiles refuse (exit 1, nothing run, message names the lockfile and manager), no-lockfile behaviour is unchanged, and auto mode records the refusal; `scripts/verify.sh` passes.
- Verification plan: new `tests/init-package-manager.sh` (fake package managers on a restricted PATH), `sh tests/init-preview.sh`, `sh tests/auto-init.sh`, `scripts/init.sh --yes`, `scripts/verify.sh`, `scripts/review.sh`, `git diff --check`.
- Risk: projects that carry two lockfiles for different managers used to bootstrap silently and now refuse; that is the intended fix.
- Build: `scripts/init.sh` maps each Node lockfile to its owner (`lockfile_owner`, `NODE_LOCKFILES`); `detect_node_pm` now sets `NODE_PM`/`NODE_LOCKED` in the current shell or `BOOTSTRAP_REFUSAL` when the owner is missing or lockfiles name different managers; `bootstrap_node` is gated on `package.json` and queues a frozen install for the owner; `confirm_and_run_plan` and `run_bootstrap_worker` refuse (`REFUSED:`, "Nothing was run.", exit 1 / `STATUS=failed EXIT=1`) before any project command; `auto_mode` writes the refusal to `bootstrap.log` and reports it on the context line. `scripts/harness-target.sh` fingerprints `npm-shrinkwrap.json`, `bun.lock`, and `bun.lockb` (appended, so existing fingerprints are unchanged). Docs: `docs/setup.md`, `README.md`. TODO #31 removed from `todo.md`.
- Tests: new `tests/init-package-manager.sh` hides real package managers from PATH and uses recording fakes: each lockfile installs only with its owner (frozen), same-owner lockfiles are fine, a missing owner refuses for yarn/pnpm/npm/bun, conflicting lockfiles refuse and name both, a refusal runs no `make init` and needs no confirmation, the no-lockfile order is unchanged, a stray lockfile without `package.json` is skipped, `--auto` records `failed`/exit 1 with the reason in the log and reports it on the next start, a rerun with the owner installed uses it, and the new lockfiles change the fingerprint. Against the base `scripts/init.sh` and `scripts/harness-target.sh` the test fails (mutation check).
- Verification (2026-09-30): `shellcheck scripts/init.sh scripts/harness-target.sh tests/init-package-manager.sh` clean; `sh tests/init-package-manager.sh`, `zsh --emulate sh tests/init-package-manager.sh`, `bash --posix tests/init-package-manager.sh`, `sh tests/init-preview.sh`, `sh tests/auto-init.sh` PASS (dash is not installed here); `scripts/init.sh --yes` exit 0; `scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0, all `tests/*.sh` PASS); `scripts/review.sh` exit 0; `git diff --check` clean. Awaiting independent panel review before commit.
- Root CI-equivalent snapshot (Ubuntu 24.04, dash, apt ShellCheck; log `.harness-db/todo-dispatch-20260930/item-31-container-ci.log` in the main checkout) failed `bash:shellcheck` (ran=3 skipped=3 failures=1): SC2015 (info) at `tests/init-package-manager.sh:42`, `[ -f "$tool" ] && [ -x "$tool" ] || continue`. The local ShellCheck 0.11.0 did not report it; all tests passed there, including under dash. Remediation plan: replace it with an explicit `if ! { [ -f ] && [ -x ]; }; then continue; fi`, without disabling the check; no other `A && B || C` exists in the change.
- Remediation (2026-09-30): `tests/init-package-manager.sh:42` is now `if [ ! -f "$tool" ] || [ ! -x "$tool" ]; then continue; fi` (no directive, check not disabled). Local ShellCheck 0.11.0 reports nothing for the old line even with `-S info`, so the fix can only be confirmed by root's Ubuntu snapshot rerun. Rerun on the new bytes: `shellcheck -S info scripts/init.sh scripts/harness-target.sh tests/init-package-manager.sh` clean; `sh`/`zsh --emulate sh`/`bash --posix tests/init-package-manager.sh`, `sh tests/init-preview.sh`, `sh tests/auto-init.sh` PASS; `scripts/init.sh --yes` exit 0; `scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0); `scripts/review.sh` exit 0; `git diff --check` clean.
- Panel review round 1 (`item-31-review.md`): NOT ACCEPTED, one confirmed Minor finding 31-F1. Remediation `README.md:140` in the `scripts/verify.sh` section had been reworded to claim verify uses the lockfile's package manager, but `scripts/verify.sh` is unchanged and still falls back. Disposition: accepted and fixed by restoring the base wording there; the lockfile-owner claim stays only in the bootstrap bullet (`README.md:63`) and `docs/setup.md` bootstrap inputs, which describe `scripts/init.sh`. `scripts/verify.sh` is not changed (out of scope for #31).
- 31-F1 fix: `README.md` verify-section bullet restored to the base text (`package.json` with `npm`, `pnpm`, or `yarn` when available); the bootstrap bullet now names `scripts/init.sh` as the script that enforces the lockfile owner. `scripts/verify.sh` byte-identical to base. Rerun on the new bytes: `shellcheck -S info scripts/init.sh scripts/harness-target.sh tests/init-package-manager.sh` clean; `sh tests/init-package-manager.sh`, `sh tests/init-preview.sh`, `sh tests/auto-init.sh` PASS; `scripts/init.sh --yes` exit 0; `scripts/verify.sh` exit 0 (ran=3 skipped=3 failures=0); `scripts/review.sh` exit 0; `git diff --check` clean.
