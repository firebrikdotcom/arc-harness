# Harness Improvement TODO

## Current Discussion

- [x] Prevent false-green verification when project-declared required checks are skipped.

## Verification

- [x] Distinguish required checks from optional checks.
- [x] Fail CI when required tools or checks are missing.
- [ ] Define a minimum quality gate for tests, linting, and type-checking.
- [ ] Make ShellCheck available consistently in local development and CI.
- [ ] Run shell syntax checks with the interpreter declared by each file.
- [ ] Stop treating every file under `scripts/` as shell code.
- [ ] Ensure verification commands never rewrite project files.
- [x] Allow Make targets and language-native checks to run together when needed. (2026-09-01: leave as-is; Make still wins.)
- [x] Support manifests in monorepo subdirectories. (2026-09-01: no authoritative manifest; detection stays.)
- [ ] Add timeouts for verification commands.

## Bootstrap

- [ ] Refuse to substitute a different package manager when a lockfile selects one.
- [ ] Define what bootstrap idempotence means and test it.
- [x] Add a preview or trust boundary before running project-owned setup scripts. (2026-09-01: leave as-is; init still runs immediately.)
- [ ] Add timeouts and non-interactive defaults to dependency installation.

## Continuous Integration

- [ ] Provision every required runtime and verification tool explicitly.
- [ ] Pin third-party GitHub Actions to immutable commit SHAs.
- [ ] Add fixture-based tests for supported project types and failure modes.
- [ ] Keep local and CI verification behavior aligned.

## Review

- [ ] Show the actual patch during review, not only diff statistics.
- [ ] Continue collecting review evidence after verification fails.
- [ ] Inspect staged, unstaged, and untracked changes.
- [ ] Connect task acceptance criteria to review output.

## Workflow and State

- [ ] Enforce or simplify planning, building, review, and PR phase separation.
- [ ] Keep the current goal and active plan consistent in `progress.md`.
- [ ] Replace the shared global progress file with per-project and per-run state.
- [ ] Resolve the conflict between root `progress.md` updates and `.harness-db/` state.
- [ ] Define a schema, locking strategy, migration policy, and retention policy for harness state.
- [ ] Add backup and portability guidance for ignored harness state.
- [ ] Define an unambiguous active-task pointer.
- [ ] Consolidate duplicated instructions into one canonical guide.
- [ ] Reduce mandatory reading for small, low-risk changes.
- [ ] Reduce progress updates to durable milestones and decisions.

## Security and Indexing

- [ ] Add a threat model for untrusted repositories, lifecycle scripts, prompt injection, and command authorization.
- [ ] Include core `docs/` and `scripts/` content in codebase graph coverage when supported.
- [x] Use generic command detection only as a fallback to explicit project-owned configuration. (2026-09-01: detection stays the source of truth.)
