# Progress

Current task: repair intermittent architecture-board verification (run 20261031T191225Z-1872699).

- Cause reproduced: the fixture's invented `demo-1` registration competed with the startup hook's canonical path-derived ID. When the canonical ID sorted first, mapping succeeded there while the test waited at the wrong path.
- Changed `tests/test_board.py` to register fixtures through `harness-target.sh` and use the returned ID; startup asserts that registration is reused.
- Scope: test fixture only; no runtime behavior, secret, or fleet changes. Preserved pre-existing untracked files.
- Checks: pre-fix targeted reproduction failed with a valid board under the canonical ID; post-fix `sh tests/board.sh` passed all 15 tests; the original demo-0 ordering reproduction also passed. `harness-runtime exec scripts/verify.sh --project .` passed (ran=3, skipped=3, failures=0).
- Acceptance: `harness build done` passed both criteria, including a second full verification. The run completed because the existing human-controlled `.harness-db/review-off` disables review; no independent review ran.
- Delivery: commit the verified fixture correction to master, require passing GitHub CI on that commit, and publish annotated tag v0.2.5 at that exact commit. Check the remote tag and active local runtime before handoff.
- Cleanup: deleted fully merged branches restore-features and review-switch; ran safe worktree cleanup and git worktree prune. Retained six dirty worktrees and one with unique commits; no task worktrees were created. Unmerged historical branches and pre-existing untracked files are preserved.
