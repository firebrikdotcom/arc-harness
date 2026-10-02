#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEW="$HARNESS_ROOT_UNDER_TEST/scripts/review.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-review.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
# The no-task cases must not inherit a task named by the caller's session, and
# the real `plan start --task` below must not inherit the caller's budget caps.
# These are self reviews, so a caller's reviewer command must not reach them.
unset HARNESS_TASK HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS \
  HARNESS_BUDGET_TOKENS HARNESS_BUDGET_CONTINUES HARNESS_REVIEWER_COMMAND
HARNESS_JEV_CHECKPOINTS=0
export HARNESS_JEV_CHECKPOINTS
OUT="$TMP_ROOT/out.txt"
PROJECT="$TMP_ROOT/project"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

# A git project with a staged change, an unstaged change, and an untracked file.
mkdir -p "$PROJECT/scripts"
git -C "$PROJECT" init -q
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q --allow-empty -m init
printf '%s\n' 'tracked original' > "$PROJECT/tracked.txt"
printf '%s\n' 'staged original' > "$PROJECT/staged.txt"
git -C "$PROJECT" add tracked.txt staged.txt
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m files
printf '%s\n' 'UNSTAGED_EDIT_LINE' >> "$PROJECT/tracked.txt"
printf '%s\n' 'STAGED_EDIT_LINE' >> "$PROJECT/staged.txt"
git -C "$PROJECT" add staged.txt
printf '%s\n' 'UNTRACKED_NEW_LINE' > "$PROJECT/new.txt"

# Passing verify: review shows every kind of change and records exit 0.
status=0
"$REVIEW" --self --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "review exited $status with a passing verify"
expect_output '^+UNSTAGED_EDIT_LINE'
expect_output '^+STAGED_EDIT_LINE'
expect_output '^+UNTRACKED_NEW_LINE'
expect_output '^Review finished (self inspection only)'
grep -q '^EXIT=0$' "$HARNESS_DB_ROOT/records/review.state" || fail "review record should show EXIT=0"
expect_output '^Acceptance criteria: no active task file\.'
expect_output '^1\. Does it satisfy acceptance criteria?$'
grep -q '^REVIEW_MODE=self$' "$HARNESS_DB_ROOT/records/review.state" || fail "a --self review should record REVIEW_MODE=self"

# Without --self or a configured reviewer the review is misconfigured: the
# building session may not review its own change.
rm -f "$HARNESS_DB_ROOT/records/review.state"
status=0
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "review without a reviewer should exit 2, got $status"
expect_output '^FAIL: no fresh-session reviewer is configured\.'
expect_output 'pass --reviewer FILE or set HARNESS_REVIEWER_COMMAND=FILE'
if grep -q 'Review started' "$OUT"; then fail "a misconfigured review should stop before it starts"; fi
[ ! -f "$HARNESS_DB_ROOT/records/review.state" ] || fail "a misconfigured review must not write a record"

# The printed patch can itself contain these markers (this test file is part of
# the harness diff), so absence is checked only after the patch ends.
expect_no_output() {
  if awk '/^Acceptance criteria|^WARN: .*[Aa]cceptance [Cc]riteria|^FAIL: /{on=1} on' "$OUT" | grep -q -e "$1"; then
    fail "expected output after the patch not to match: $1"
  fi
}

# The active task's acceptance criteria are printed one per line for the reviewer.
TASK="$TMP_ROOT/task.md"
ESC=$(printf '\033')
CR=$(printf '\r')
cat > "$TASK" <<EOF
# Task: sample

\`\`\`md
## Acceptance Criteria
- FENCED_BEFORE_SECTION
\`\`\`

## Acceptance criteria:

Prose that introduces the list is not a criterion.

- [ ] First criterion.
- [x] Second criterion wraps
  onto a second line.
* Third ${ESC}[31mcriterion.
\`\`\`
- FENCED_INSIDE_SECTION
\`\`\`
### Details
1. Fourth criterion under a sub-heading.
2) Fifth criterion.
- CR_BEFORE${CR}CR_AFTER overwrites nothing.
- Seventh criterion ends in CRLF.${CR}
- [ ]

## Constraints

- NOT_A_CRITERION
EOF
status=0
"$REVIEW" --self --project "$PROJECT" --task "$TASK" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "review with a task exited $status"
expect_output "^Acceptance criteria from $TASK:\$"
expect_output '^AC1\. First criterion\.$'
expect_output '^AC2\. Second criterion wraps onto a second line\.$'
expect_output '^AC3\. Third \[31mcriterion\.$'
expect_output '^AC4\. Fourth criterion under a sub-heading\.$'
expect_output '^AC5\. Fifth criterion\.$'
expect_output '^AC6\. CR_BEFORE CR_AFTER overwrites nothing\.$'
expect_output '^AC7\. Seventh criterion ends in CRLF\.$'
expect_no_output '^AC8\.'
expect_no_output "$CR"
expect_no_output 'FENCED_BEFORE_SECTION'
expect_no_output 'FENCED_INSIDE_SECTION'
expect_no_output 'NOT_A_CRITERION'
expect_no_output 'Prose that introduces'
expect_no_output "$ESC"
expect_output '^Answer each criterion: met, not met, or not applicable'
expect_output '^1\. Does it satisfy every acceptance criterion listed above?$'
expect_output '^Review finished (self inspection only)'

# HARNESS_TASK names the task too, and --task wins over it.
status=0
HARNESS_TASK="$TASK" "$REVIEW" --self --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "review with HARNESS_TASK exited $status"
expect_output '^AC5\. Fifth criterion\.$'
status=0
HARNESS_TASK="$TMP_ROOT/missing.md" "$REVIEW" --self --project "$PROJECT" --task "$TASK" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "--task should win over a missing HARNESS_TASK (exit $status)"
expect_output '^AC1\. First criterion\.$'

# Without --task or HARNESS_TASK, review uses the task the current run
# recorded with `harness plan start --task`; both explicit forms still win.
CLI="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
cp "$TASK" "$TMP_ROOT/run-task.md"
(cd "$TMP_ROOT" && "$CLI" plan start --task run-task.md) > "$OUT" 2>&1 || fail "plan start --task failed"
run_id=$(head -n 1 "$HARNESS_DB_ROOT/runs/current")
RUN_TASK=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)/run-task.md
status=0
"$REVIEW" --self --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "review with a run task exited $status"
expect_output "^Acceptance criteria from $RUN_TASK (run $run_id task):\$"
expect_output '^AC7\. Seventh criterion ends in CRLF\.$'
expect_output '^1\. Does it satisfy every acceptance criterion listed above?$'
status=0
HARNESS_TASK="$TMP_ROOT/no-heading-env.md" "$REVIEW" --self --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "HARNESS_TASK should win over the run task (exit $status)"
expect_output "FAIL: task file does not exist or is not readable: $TMP_ROOT/no-heading-env.md"
# A recorded task that has gone missing fails like a named one.
rm "$TMP_ROOT/run-task.md"
status=0
"$REVIEW" --self --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "a missing run task should exit 2, got $status"
expect_output "FAIL: the task file recorded by run $run_id does not exist or is not readable: $RUN_TASK"
status=0
"$REVIEW" --self --project "$PROJECT" --task "$TASK" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "--task should win over a missing run task (exit $status)"
expect_output "^Acceptance criteria from $TASK:\$"
rm -rf "$HARNESS_DB_ROOT/runs"

# A named task that cannot be read fails before verification runs.
for bad in "$TMP_ROOT/missing.md" "$TMP_ROOT"; do
  status=0
  "$REVIEW" --self --project "$PROJECT" --task "$bad" > "$OUT" 2>&1 || status=$?
  [ "$status" -eq 2 ] || fail "unreadable task $bad should exit 2, got $status"
  expect_output 'FAIL: task file does not exist or is not readable'
  if grep -q 'Review started' "$OUT"; then fail "unreadable task $bad should stop before the review starts"; fi
done
status=0
"$REVIEW" --self --project "$PROJECT" --task > "$OUT" 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "--task without a path should exit 2, got $status"
expect_output 'FAIL: --task requires a path\.'

# A task without criteria warns and the review still completes.
printf '%s\n' '# Task: none' '' '## Goal' '' '- NOT_A_CRITERION' > "$TMP_ROOT/no-heading.md"
status=0
"$REVIEW" --self --project "$PROJECT" --task "$TMP_ROOT/no-heading.md" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "task without criteria exited $status"
expect_output 'WARN: .*no-heading\.md has no Acceptance Criteria heading'
expect_no_output 'NOT_A_CRITERION'
expect_output '^1\. Does it satisfy acceptance criteria?$'
expect_output '^Review finished (self inspection only)'
printf '%s\n' '# Task: empty' '' '## Acceptance Criteria' '' 'None yet.' '' '## Goal' '- NOT_A_CRITERION' > "$TMP_ROOT/empty.md"
status=0
"$REVIEW" --self --project "$PROJECT" --task "$TMP_ROOT/empty.md" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "task with an empty criteria section exited $status"
expect_output 'WARN: the Acceptance Criteria section of .*empty\.md lists no items'
expect_no_output 'NOT_A_CRITERION'

# Failing verify: review keeps going, still shows the patch, exits non-zero, records the failure.
printf '%s\n' 'lint' > "$PROJECT/.harness-required-checks"
status=0
"$REVIEW" --self --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "review should exit non-zero when verify fails"
expect_output 'WARN: verification failed'
expect_output '^+UNSTAGED_EDIT_LINE'
expect_output '^+UNTRACKED_NEW_LINE'
expect_output 'Review questions:'
grep -q '^EXIT=1$' "$HARNESS_DB_ROOT/records/review.state" || fail "review record should show EXIT=1"

printf '%s\n' 'PASS: review shows the full patch, prints the task acceptance criteria, and continues after a failing verify'
