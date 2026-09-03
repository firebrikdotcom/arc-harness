# Harness Improvement TODO

Items are numbered so they can be referred to as `#N`. The 2026-09-03 review findings come first because they are the current discussion; the older backlog follows.

## Review Findings 2026-09-03

Found by exercising the harness on branch `lotharthesavior/feat-cli`. Each item has a why and a fix, in plain words.

### Rules are only words

- [x] 1. Nothing forces the AI to use the harness. (Done 2026-09-03 for Claude Code: `.claude/settings.json` + `scripts/hooks/require-phase.sh`, tested by `tests/harness-hook.sh`.)
  - Why: The harness is a list of rules the robot is asked to follow, but nothing checks that it does, so the robot can skip every rule and nobody would notice.
  - Fix: Put a guard in front of the robot's tools that says "no" any time it tries to edit or run something without first telling the harness which step it is on.
- [x] 2. `CLAUDE.md` and `AGENTS.md` never mention `scripts/harness`. (Done 2026-09-03: `make install-guides` / `scripts/install-guides.sh` writes a marked block into both files; applied to this repo; tested by `tests/install-guides.sh`.)
  - Why: the AI only does what its instruction sheet says, and the sheet does not list the new commands.
  - Fix: add the plan, build, and review commands to both files, or point both at one shared core guide.
- [x] 3. A full plan, build, review run completes without `scripts/verify.sh` or `scripts/review.sh` ever running. (Done 2026-09-03: verify and review write `.harness-db/records/*.state`; `build done` and `review done` require a fresh passing record.)
  - Why: the gate only checks the order of the stamps, not whether the work behind each stamp happened.
  - Fix: make `build done` refuse unless a fresh verify run record exists, and `review done` refuse unless review ran.
- [x] 4. The AI can `continue "ok"` to itself forever after a budget pause. (Done 2026-09-03: the hook refuses `continue` and `abort` from the agent, and the CLI caps continues per run at 3 via `HARNESS_BUDGET_CONTINUES`; `harness abort` added.)
  - Why: letting the runner also be the referee means the whistle never really stops the game.
  - Fix: require a human-written approval file or cap the number of continues per run.
- [x] 5. Steps and tokens are only counted when the AI reports them. (Done 2026-09-03 for steps: the hook records one step per allowed Write/Edit/Bash call. Tokens remain unknown; the hook payload has no counts.)
  - Why: a piggy bank only counts the coins you choose to put in.
  - Fix: count steps from a hook on every tool call instead of asking the AI to report them.

### Safety checks do not check for danger

- [ ] 6. The action validator passes `rm -rf /` and writes to `.git/hooks/`.
  - Why: checking that a letter has the right shape does not tell you if it says something mean.
  - Fix: add the planned denylist for dangerous commands and protected paths so validate rejects them.
- [ ] 7. The validator neither runs nor blocks the action.
  - Why: if the check happens in one room and the action in another, nothing connects them.
  - Fix: have the hook require a validated action file before each Bash or Write call, so check and action are tied together.
- [ ] 8. `scripts/init.sh` runs `make init`, `npm install`, and composer immediately in any repo. (Superseded 2026-09-01 decision in #33; reopened by this review.)
  - Why: running a stranger's setup script is like eating candy from a stranger.
  - Fix: print what would run and ask yes or no, with a `--yes` flag for CI, before running project scripts.
- [ ] 9. `knowledge/` hooks inside the target repo always run.
  - Why: a folder inside someone else's repo can say anything, and the AI obeys it.
  - Fix: trust `knowledge/` hooks only after the user approves them once per machine, recorded with a hash.

### The pass or fail sensor gives wrong answers

- [x] 10. `scripts/verify.sh` always fails on this repo because of old ShellCheck warnings in five files. (Done 2026-09-03: SC1007, SC2164, SC2209 fixed; `bash:shellcheck` passes.)
  - Why: when the alarm rings every day for nothing, people stop listening to it.
  - Fix: fix the five ShellCheck warnings so verify is green, then keep it green.
- [ ] 11. The required `test` category is satisfied by `sh -n`, a syntax check.
  - Why: checking that a sentence is spelled right does not prove it is true.
  - Fix: move `sh -n` to the lint category and make the test category run real tests.
- [x] 12. `tests/*.sh` are never run by `scripts/verify.sh` or CI. (Done 2026-09-03: `harness:tests` check runs them when verifying the harness root, so CI runs them too.)
  - Why: a test nobody runs is just a file.
  - Fix: make `scripts/verify.sh` run `tests/*.sh` when present so CI runs them too.

### Memory gets messy and can get stuck

- [ ] 13. `progress.md` holds three goals and months of history instead of the current run. (See also #44, #45, #52.)
  - Why: reading a whole diary to find today's page wastes time and mixes things up.
  - Fix: keep `progress.md` to the current run only and archive old runs under `.harness-db/runs/<id>/`.
- [ ] 14. A run left overnight can never resume, and there is no abort command. (Partly done 2026-09-03: `harness abort` exists. Still open: `continue` on the time budget only adds one window, so an old run re-pauses at once.)
  - Why: adding 15 minutes to a run that is 2350 minutes late never catches up, and there is no exit door.
  - Fix: add `harness abort`, and make `continue` on the time budget restart the clock from now instead of extending it.
- [ ] 15. Two sessions in one checkout share `.harness-db/runs/current` with no lock. (See also #47.)
  - Why: two people writing on the same sticky note at once make a mess.
  - Fix: use a per-session run id from an environment variable, or a lock file around state writes.
- [ ] 16. The same AI with the same memory marks plan, build, and review done.
  - Why: checking your own homework does not catch what you did not see.
  - Fix: run review in a fresh session or subagent that only gets the diff and the acceptance criteria.

### Smaller things that still hurt

- [ ] 17. Six or more files are mandatory reading and the architecture doc is mostly "unknown". (See also #51.)
  - Why: a huge instruction pile makes the AI skim, and it mostly says nothing yet.
  - Fix: one short core file, with links to docs to open only when a task needs them.
- [ ] 18. The same rules are copied into `README.md`, `CLAUDE.md`, and `AGENTS.md` and have drifted. (See also #50.)
  - Why: three copies of the same rule always drift apart.
  - Fix: keep one canonical guide and make the others one-liners that point to it.
- [ ] 19. CI actions are pinned to floating tags like `@v4`. (Same as #36.)
  - Why: a floating tag can change under you tomorrow without you knowing.
  - Fix: pin `actions/checkout` and `actions/setup-node` to full commit SHAs.

## Backlog

### Verification

- [x] 20. Prevent false-green verification when project-declared required checks are skipped.
- [x] 21. Distinguish required checks from optional checks.
- [x] 22. Fail CI when required tools or checks are missing.
- [x] 23. Define a minimum quality gate for tests, linting, and type-checking. (Done via `.harness-required-checks` requiring `lint` and `test`; #11 covers the weak test category.)
- [ ] 24. Make ShellCheck available consistently in local development and CI.
- [ ] 25. Run shell syntax checks with the interpreter declared by each file.
- [ ] 26. Stop treating every file under `scripts/` as shell code.
- [ ] 27. Ensure verification commands never rewrite project files.
- [x] 28. Allow Make targets and language-native checks to run together when needed. (2026-09-01: leave as-is; Make still wins.)
- [x] 29. Support manifests in monorepo subdirectories. (2026-09-01: no authoritative manifest; detection stays.)
- [ ] 30. Add timeouts for verification commands.

### Bootstrap

- [ ] 31. Refuse to substitute a different package manager when a lockfile selects one.
- [ ] 32. Define what bootstrap idempotence means and test it.
- [x] 33. Add a preview or trust boundary before running project-owned setup scripts. (2026-09-01: leave as-is; init still runs immediately. Reopened as #8.)
- [ ] 34. Add timeouts and non-interactive defaults to dependency installation.

### Continuous Integration

- [ ] 35. Provision every required runtime and verification tool explicitly.
- [ ] 36. Pin third-party GitHub Actions to immutable commit SHAs. (Same as #19.)
- [ ] 37. Add fixture-based tests for supported project types and failure modes.
- [ ] 38. Keep local and CI verification behavior aligned.

### Review

- [ ] 39. Show the actual patch during review, not only diff statistics.
- [ ] 40. Continue collecting review evidence after verification fails.
- [ ] 41. Inspect staged, unstaged, and untracked changes.
- [ ] 42. Connect task acceptance criteria to review output.

### Workflow and State

- [x] 43. Enforce or simplify planning, building, review, and PR phase separation. (Done for plan, build, review order via `scripts/harness`; #1 and #3 cover what the gate still misses.)
- [ ] 44. Keep the current goal and active plan consistent in `progress.md`.
- [ ] 45. Replace the shared global progress file with per-project and per-run state.
- [ ] 46. Resolve the conflict between root `progress.md` updates and `.harness-db/` state.
- [ ] 47. Define a schema, locking strategy, migration policy, and retention policy for harness state.
- [ ] 48. Add backup and portability guidance for ignored harness state.
- [ ] 49. Define an unambiguous active-task pointer.
- [ ] 50. Consolidate duplicated instructions into one canonical guide.
- [ ] 51. Reduce mandatory reading for small, low-risk changes.
- [ ] 52. Reduce progress updates to durable milestones and decisions.

### Security and Indexing

- [ ] 53. Add a threat model for untrusted repositories, lifecycle scripts, prompt injection, and command authorization.
- [ ] 54. Include core `docs/` and `scripts/` content in codebase graph coverage when supported.
- [x] 55. Use generic command detection only as a fallback to explicit project-owned configuration. (2026-09-01: detection stays the source of truth.)
