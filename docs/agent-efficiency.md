## Efficient agent work

Every tool call re-sends the whole conversation, so cost and latency grow with context size times
the number of calls. These rules apply to every session and every worker, in every project.

**Orchestration**
- For work with several tasks, the main session only coordinates: it plans, writes briefs, merges
  results and runs the final checks. It does not edit, build or test itself, and it keeps its own
  context small (narrow line ranges, filtered output).
- Keep the state in a handoff file (`progress.md`): what is done and the command that verified it;
  the remaining tasks, each with goal, owned files, dependencies, check command and completion
  criterion; decisions; branches and worktrees. Update it at each milestone.
- At about 150k context tokens, or when a milestone completes, update the handoff, commit, and end
  the message with: `>>> READY TO CLEAR — start a new session from progress.md <<<`.

**Workers**
- At most three at once. Start each one fresh, with no inherited history (Codex
  `fork_turns: "none"`), and a self-contained brief: goal, owned files, files it must not touch,
  check command, completion criterion, scope freeze.
- One worker per task or feedback round. Never give a finished worker a new task or round (no
  follow-up to a used worker); start a new one with a fresh brief.
- Each editing worker works in its own Git worktree and branch. Owned files never overlap; tasks
  that share files run one after another.
- Workers send no progress messages, only one final report of at most 10 lines: commit, check
  results, open problems. They ask only when truly blocked.
- Use the cheapest model and reasoning effort that fits the task.

**Worktree cleanup**
- Merge a finished task branch into the integration branch promptly.
- Once a task branch is merged and its checks pass on the integration branch, remove its worktree
  (`git worktree remove PATH`), then its build output if it lives elsewhere, and run
  `git worktree prune`. Delete the task branch once it is merged (`git branch -d`, never `-D`).
- Never remove a worktree with uncommitted changes, unmerged commits, or a process still using it;
  report it instead. Record the cleanup in the handoff file.
- The harness also removes such worktrees by itself, at session start and after a merge; it
  keeps any worktree that is dirty, unmerged, not started, recently used, or in use.

**Waiting and commands**
- Wait on workers and long commands with one long blocking wait (30 minutes or more). Never poll
  at short intervals, sleep-check, or poll idle terminals. Codex enforces a 30-minute minimum wait
  for helpers; a wait still ends as soon as a helper reports or the user types.
- Filter command output: quiet flags, `2>&1 | tail -30`, `grep -E 'error|FAIL'`. Never print whole
  files or logs.
- Build and test once per commit, not after every edit. Use the project's scoped checks when it has
  them; run the full checks before merging into the main branch.

**UI and computer use**
- Batch one interaction (several clicks, keys, a paste) into one call, then read the state once.
- Prefer text or accessibility state. Take screenshots only at final comparison points, at most
  three per task.
- Compare against reference designs with a script on saved files and read only its summary unless
  it flags a problem. View each reference image inline at most once and write down what it shows.

**Scope, review, communication**
- Freeze scope to the listed tasks; new ideas go under Follow-ups in the handoff file.
- One review pass per finished milestone. Fix right away only correctness, data-loss and security
  findings; record the rest as follow-ups.
- Reuse existing fixtures instead of building new test infrastructure.
- One short status checklist (at most five lines) per completed task. Batch questions for the user
  into one request per milestone, including design feedback.

**Real fixes**
- Fix the underlying cause and read configuration from its real source.
- If a dependency is missing, implement it or clearly report the blocker; never substitute a
  hardcoded approximation.
- Verify the reported behavior in the actual environment before claiming it works.

User and project instructions take precedence over these rules.
