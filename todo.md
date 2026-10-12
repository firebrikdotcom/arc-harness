# Harness Improvement TODO

Items are numbered so they can be referred to as `#N`. Numbers are stable: a done or dropped item is removed and its number is not reused, so gaps are expected. Pruned 2026-09-03: duplicates were merged into one item each and nitpicks were dropped. Resolved the same day: #6 and #7 (denylist enforced by the hook), #8 (bootstrap preview), #9 (knowledge trust), #27 (check-only verify), #39 and #40 (full-patch review that continues after failure), and #14 (a time-budget continue restarts the clock).

## The pass or fail sensor gives wrong answers


## Before public publication

The repository is public. #58 (supervisor paths rendered from the install root, with `.env` overrides and a test from another directory) was resolved 2026-10-10.

- [ ] 55. `README.md` does not mention the license.
  - Why: readers look for reuse terms on the front page; `LICENSE` exists but nothing points to it.
  - Fix: link `LICENSE` from `README.md` and check third-party attribution requirements.
- [ ] 56. Older commits still contain personal paths, machine names, and private run notes.
  - Why: cleaning today's tree leaves every earlier commit readable on GitHub; `tests/personal-paths.sh` guards only the current tree.
  - Fix: decide whether to rewrite history (`git-filter-repo` in a disposable clone, then a coordinated force-push) or publish a fresh single-commit repository; run a full-history secret scan either way.
- [ ] 57. Optional integrations cannot be set up from a fresh checkout.
  - Why: TypeSafe routing defaults to `~/.agents/skills/typesafe-routing/scripts/route.py`, which is not shipped here or given an installation procedure.
  - Fix: document a versioned source, installation, prerequisites, and fallback for that adapter and jevgrep; test both the missing-integration fallback and configured operation.
- [ ] 59. CI does not build or test the Rust audit service.
  - Why: `scripts/verify.sh` and CI exercise the harness scripts only, so `services/harness-audit` can break unnoticed.
  - Fix: run `services/harness-audit/compose.test.yml` (format, clippy, `cargo test --locked`) in local verification and CI, without production credentials.
- [ ] 60. Public users and contributors lack documentation.
  - Why: `docs/architecture.md` still has "unknown" placeholders, and there is no contributor guide or vulnerability-reporting channel.
  - Fix: fill in the actual modules and prerequisites, add `CONTRIBUTING.md` with a fresh-checkout workflow, add a private reporting route to `SECURITY.md`, and link them from `README.md`.

## Dropped 2026-09-03

Nitpicks or items that stopped mattering after other fixes: #25 (interpreter-aware syntax check), #26 (non-shell files under `scripts/`), #32 (idempotence definition), #48 (state backup guidance), #54 (graph index scope, external tool).
