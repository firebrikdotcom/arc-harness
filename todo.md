# Harness Improvement TODO

Items are numbered so they can be referred to as `#N`. Numbers are stable: a done or dropped item is removed and its number is not reused, so gaps are expected. Pruned 2026-09-03: duplicates were merged into one item each and nitpicks were dropped. Resolved the same day: #6 and #7 (denylist enforced by the hook), #8 (bootstrap preview), #9 (knowledge trust), #27 (check-only verify), #39 and #40 (full-patch review that continues after failure), and #14 (a time-budget continue restarts the clock).

## Safety checks do not check for danger

- [ ] 53. `SECURITY.md` has no threat model.
  - Why: without a list of what can go wrong, every safety fix is a guess.
  - Fix: add a short section covering untrusted repositories, lifecycle scripts, prompt injection, and command authorization.

## The pass or fail sensor gives wrong answers

- [ ] 11. The required `test` category is satisfied by `sh -n`, a syntax check.
  - Why: checking that a sentence is spelled right does not prove it is true.
  - Fix: move `sh -n` to the lint category so `test` passes only when real tests run.
- [ ] 30. Verification and bootstrap commands have no timeouts. (Merged #34.)
  - Why: a stuck install or test hangs the whole run forever.
  - Fix: wrap each command in a timeout with a clear failure message.
- [ ] 31. Bootstrap installs with a different package manager than the lockfile chose.
  - Why: a `yarn.lock` installed by npm gives a different set of packages than the team tested.
  - Fix: honor the lockfile's package manager or refuse with a clear message.
- [ ] 37. Detection for Node, PHP, Go, and Rust projects has no fixture tests.
  - Why: the harness's own tests cover the CLI but not the project types it claims to verify.
  - Fix: add small fixture projects and failure cases under `tests/`.
- [ ] 38. Local and CI verification need the same tools, but nothing checks that. (Merged #24, #35.)
  - Why: a green CI and a red laptop, or the reverse, means one of them is lying.
  - Fix: list required tools in one place, have `scripts/init.sh` check them, and have CI install exactly that list.
- [ ] 19. CI actions are pinned to floating tags like `@v4`. (Merged #36.)
  - Why: a floating tag can change under you tomorrow without you knowing.
  - Fix: pin `actions/checkout` and `actions/setup-node` to full commit SHAs.

## Review does not really review

- [ ] 42. Review output is not tied to the task's acceptance criteria.
  - Why: a review that never looks at the goal cannot say whether the goal was met.
  - Fix: read the active task's acceptance criteria and print each one for the reviewer to answer.
- [ ] 16. The same AI with the same memory marks plan, build, and review done.
  - Why: checking your own homework does not catch what you did not see.
  - Fix: run review in a fresh session or subagent that only gets the diff and the acceptance criteria.

## Memory gets messy

- [ ] 13. `progress.md` holds many goals and months of history instead of the current run. (Merged #44, #45, #46, #49, #52.)
  - Why: reading a whole diary to find today's page wastes time and mixes things up.
  - Fix: keep `progress.md` to the current run's goal, plan, and decisions; archive finished runs under `.harness-db/runs/<id>/`.
- [ ] 15. Two sessions in one checkout share `.harness-db/runs/current` with no lock. (Merged #47.)
  - Why: two people writing on the same sticky note at once make a mess.
  - Fix: use a per-session run id from an environment variable, or a lock file around state writes.
- [ ] 17. Mandatory reading is six or more files that duplicate each other and mostly say "unknown". (Merged #18, #50, #51.)
  - Why: a huge, repeated instruction pile makes the AI skim, and copies drift apart.
  - Fix: one short core guide; `README.md`, `CLAUDE.md`, and `AGENTS.md` point to it, and other docs open only when a task needs them.

## Dropped 2026-09-03

Nitpicks or items that stopped mattering after other fixes: #25 (interpreter-aware syntax check), #26 (non-shell files under `scripts/`), #32 (idempotence definition), #48 (state backup guidance), #54 (graph index scope, external tool).
