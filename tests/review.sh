#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEW="$HARNESS_ROOT_UNDER_TEST/scripts/review.sh"
CLI="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-review.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# review submit asks for a typed confirmation on a terminal; tests answer from a file.
printf 'yes\n' > "$TMP_ROOT/confirm"
HARNESS_CONFIRM_TTY="$TMP_ROOT/confirm"
export HARNESS_CONFIRM_TTY
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_REVIEWER_CMD HARNESS_REVIEW_BASE || true
OUT="$TMP_ROOT/out.txt"
PROJECT="$TMP_ROOT/project"
RECORDS="$HARNESS_DB_ROOT/records"

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
printf 'test:\n\t@true\n' > "$PROJECT/Makefile"
git -C "$PROJECT" add tracked.txt staged.txt Makefile
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m files
printf '%s\n' 'UNSTAGED_EDIT_LINE' >> "$PROJECT/tracked.txt"
printf '%s\n' 'STAGED_EDIT_LINE' >> "$PROJECT/staged.txt"
git -C "$PROJECT" add staged.txt
printf '%s\n' 'UNTRACKED_NEW_LINE' > "$PROJECT/new.txt"
TREE=$(sh "$HARNESS_ROOT_UNDER_TEST/scripts/tree-hash.sh" "$PROJECT")

# Passing verify: review shows every kind of change, writes a packet, and records exit 0.
status=0
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -eq 0 ] || fail "review exited $status with a passing verify"
expect_output '^+UNSTAGED_EDIT_LINE'
expect_output '^+STAGED_EDIT_LINE'
expect_output '^+UNTRACKED_NEW_LINE'
expect_output 'Independent review: hand the packet'
expect_output 'Review finished\.'
grep -q '^EXIT=0$' "$RECORDS/review.state" || fail "review record should show EXIT=0"
grep -q "^TREE_HASH=$TREE\$" "$RECORDS/review.state" || fail "review record should carry the tree hash"
PACKET="$RECORDS/review-packet.md"
[ -f "$PACKET" ] || fail "review should write a packet"
grep -q 'You are an independent reviewer' "$PACKET" || fail "packet should brief the reviewer"
grep -q "Tree hash: $TREE" "$PACKET" || fail "packet should name the tree it covers"
grep -q '^+UNTRACKED_NEW_LINE' "$PACKET" || fail "packet should hold the diff"
grep -q 'Verification summary' "$PACKET" || fail "packet should hold the verification output"
grep -q 'No task contract was recorded' "$PACKET" || fail "packet should say when no contract exists"
expect_output '^==> Acceptance criteria$'
expect_output '^No task contract was recorded for this run'
[ ! -f "$RECORDS/review-findings.json" ] || fail "no findings may exist before a reviewer ran"

# A configured reviewer runs from the packet alone; its approving findings are stored.
REVIEWER="$TMP_ROOT/reviewer.sh"
cat > "$REVIEWER" <<'SH'
#!/usr/bin/env sh
# $1 packet, $2 findings output. Reads only the packet.
tree=$(sed -n 's/^Tree hash: //p' "$1")
printf '{"tree_hash":"%s","reviewer":"fixture-reviewer","verdict":"approve","findings":[]}\n' "$tree" > "$2"
SH
chmod +x "$REVIEWER"
# A person configures the reviewer in the database; "$1" and "$2" are its arguments.
# shellcheck disable=SC2016
printf '%s "$1" "$2"\n' "$REVIEWER" > "$HARNESS_DB_ROOT/reviewer"
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || fail "review with a reviewer failed"
expect_output 'Running the independent reviewer'
expect_output 'OK: fixture-reviewer approved'
grep -q '"verdict":"approve"' "$RECORDS/review-findings.json" || fail "approving findings should be stored"

# The reviewer is a fresh process: it runs from an empty scratch directory outside
# the project and the harness, gets the packet on stdin, and inherits no
# agent-session or harness variables, while other variables pass through. The
# command is read before verification runs project code, so a check that rewrites
# it does not change what runs, and the record carries the hash of what ran.
SEEN="$TMP_ROOT/seen"
cat > "$REVIEWER" <<SH
#!/usr/bin/env sh
pwd -P > "$SEEN.pwd"
ls -A > "$SEEN.ls"
env > "$SEEN.env"
cat > "$SEEN.stdin"
tree=\$(sed -n 's/^Tree hash: //p' "\$1")
printf '{"tree_hash":"%s","reviewer":"isolated-reviewer","verdict":"approve","findings":[]}\n' "\$tree" > "\$2"
SH
reviewer_sha=$(head -n 1 "$HARNESS_DB_ROOT/reviewer" | python3 -c 'import hashlib, sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')
cp "$PROJECT/Makefile" "$TMP_ROOT/Makefile.orig"
printf 'test:\n\t@echo false > %s\n' "$HARNESS_DB_ROOT/reviewer" > "$PROJECT/Makefile"
CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli CODEX_FIXTURE=1 HARNESS_FIXTURE_STATE=1 REVIEW_FIXTURE_KEEP=kept \
  "$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || fail "review with an isolated reviewer failed"
cp "$TMP_ROOT/Makefile.orig" "$PROJECT/Makefile"
expect_output 'OK: isolated-reviewer approved'
grep -q '^false$' "$HARNESS_DB_ROOT/reviewer" || fail "the fixture check should have rewritten the reviewer file"
grep -q "^REVIEWER_COMMAND_SHA256=$reviewer_sha\$" "$RECORDS/review.state" || fail "the record should carry the hash of the reviewer that ran"
scratch=$(cat "$SEEN.pwd")
case "$scratch/" in
  "$PROJECT"/*|"$HARNESS_ROOT_UNDER_TEST"/*) fail "the reviewer should run outside the project and harness, ran in $scratch" ;;
esac
case "$scratch" in
  */harness-reviewer.*) ;;
  *) fail "the reviewer should run in its own scratch directory, ran in $scratch" ;;
esac
[ ! -e "$scratch" ] || fail "the scratch directory should be removed after the review"
[ "$(cat "$SEEN.ls")" = "REVIEW.md" ] || fail "the scratch directory should hold only the packet, held: $(cat "$SEEN.ls")"
cmp -s "$SEEN.stdin" "$RECORDS/review-packet.md" || fail "the reviewer should get the packet on stdin"
for leaked in '^CLAUDECODE=' '^CLAUDE_CODE_ENTRYPOINT=' '^CODEX_FIXTURE=' '^HARNESS_FIXTURE_STATE=' '^HARNESS_DB_ROOT=' '^HARNESS_CONFIRM_TTY='; do
  if grep -q -e "$leaked" "$SEEN.env"; then fail "the reviewer should not inherit $leaked"; fi
done
grep -q '^REVIEW_FIXTURE_KEEP=kept$' "$SEEN.env" || fail "other variables should reach the reviewer"
grep -q '^HARNESS_AUTO_INIT=0$' "$SEEN.env" || fail "the reviewer should not auto-init its scratch directory"
# shellcheck disable=SC2016
printf '%s "$1" "$2"\n' "$REVIEWER" > "$HARNESS_DB_ROOT/reviewer"

# A malformed reviewer answer is not stored.
rm -f "$RECORDS/review-findings.json"
# shellcheck disable=SC2016 # "$2" belongs to the generated reviewer script
printf '#!/usr/bin/env sh\nprintf "%%s" "{not json" > "$2"\n' > "$REVIEWER"
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || true
rm -f "$HARNESS_DB_ROOT/reviewer"
expect_output 'malformed'
[ ! -f "$RECORDS/review-findings.json" ] || fail "malformed findings must not be stored"

# Failing verify: review keeps going, still shows the patch, exits non-zero, records the failure.
printf '%s\n' 'lint' > "$PROJECT/.harness-required-checks"
status=0
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "review should exit non-zero when verify fails"
expect_output 'WARN: verification failed'
expect_output '^+UNSTAGED_EDIT_LINE'
expect_output '^+UNTRACKED_NEW_LINE'
grep -q '^EXIT=1$' "$RECORDS/review.state" || fail "review record should show EXIT=1"
rm "$PROJECT/.harness-required-checks"

# review done needs an approving independent verdict for the current files.
(cd "$PROJECT" && "$CLI" plan start && "$CLI" contract waive "fixture task" && "$CLI" plan "done" && "$CLI" build start) > "$OUT" 2>&1 || fail "phases failed"
"$HARNESS_ROOT_UNDER_TEST/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
(cd "$PROJECT" && "$CLI" build "done" && "$CLI" review start) > "$OUT" 2>&1 || fail "build done / review start failed"
# Work committed after the plan is part of the review, not only the dirty tree.
printf '%s\n' 'COMMITTED_AFTER_PLAN' > "$PROJECT/committed.txt"
git -C "$PROJECT" add committed.txt
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m "after plan"
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || fail "review failed"
grep -q '^+COMMITTED_AFTER_PLAN' "$RECORDS/review-packet.md" || fail "the packet should hold commits made since the plan"
grep -q 'commits since .*' "$RECORDS/review-packet.md" || fail "the packet should list the commits since the plan"
TREE=$(sh "$HARNESS_ROOT_UNDER_TEST/scripts/tree-hash.sh" "$PROJECT")
status=0
(cd "$PROJECT" && "$CLI" review "done") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 4 ] || fail "review done without findings should exit 4, got $status"
expect_output 'no review findings'
printf '{"tree_hash":"%s","reviewer":"subagent","verdict":"block","findings":[{"severity":"major","file":"tracked.txt","line":2,"claim":"Edit is unexplained","evidence":"diff line 2"}]}\n' "$TREE" > "$TMP_ROOT/block.json"
(cd "$PROJECT" && "$CLI" review submit "$TMP_ROOT/block.json") > "$OUT" 2>&1 || fail "a blocking review should still be submittable"
status=0
(cd "$PROJECT" && "$CLI" review "done") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 4 ] || fail "a blocking verdict must stop review done, got $status"
expect_output '\[major\] tracked.txt:2: Edit is unexplained'
printf '{"tree_hash":"%s","reviewer":"subagent","verdict":"approve","findings":[{"severity":"major","file":"tracked.txt","line":2,"claim":"Edit is unexplained","evidence":"explained in commit","status":"fixed"}]}\n' "$TREE" > "$TMP_ROOT/approve.json"
(cd "$PROJECT" && "$CLI" review submit "$TMP_ROOT/approve.json") > "$OUT" 2>&1 || fail "approve submit failed"
# An edit after the review voids it.
printf '%s\n' 'late' >> "$PROJECT/new.txt"
status=0
(cd "$PROJECT" && "$CLI" review "done") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 4 ] || fail "review done must refuse after a later edit, got $status"
expect_output 'project files changed after the last review run'
sed -i.bak '$d' "$PROJECT/new.txt" && rm -f "$PROJECT/new.txt.bak"
(cd "$PROJECT" && "$CLI" review "done") > "$OUT" 2>&1 || fail "review done should pass with an approving review of the current files"
expect_output 'Run complete'

# --- acceptance criteria ------------------------------------------------------

# A waived contract reaches the reviewer as its reason.
grep -q '^Contract waived: fixture task$' "$RECORDS/review-packet.md" || fail "the packet should carry the waiver reason as the criteria"

# Each criterion of the run's contract is printed, in the terminal and the packet,
# for the reviewer to answer; the ones without a command are marked as checked
# by the reviewer alone. Criterion text is data: control characters are dropped.
ESC=$(printf '\033')
CR=$(printf '\r')
cat > "$TMP_ROOT/contract.json" <<JSON
{"goal": "Sample", "deliverables": ["tracked.txt"], "non_goals": [],
 "acceptance": [
  {"id": "tests", "criterion": "The suite passes.", "command": ["make", "test"]},
  {"id": "docs", "criterion": "Docs explain \u001b[31mthe change.\rOverwritten?"},
  {"id": "scope", "criterion": "Nothing outside tracked.txt changes."}
 ]}
JSON
(cd "$PROJECT" && "$CLI" plan start && "$CLI" contract set "$TMP_ROOT/contract.json") > "$OUT" 2>&1 || fail "contract set failed"
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || fail "review with a contract failed"
expect_output '^- tests: The suite passes\. \[command passed at build done: make test; check that it tests this\]$'
expect_output '^- docs: Docs explain \[31mthe change\. Overwritten? \[no command: only the reviewer checks this\]$'
expect_output '^- scope: Nothing outside tracked\.txt changes\. \[no command: only the reviewer checks this\]$'
expect_output '^Answer each criterion as met, not_met, or not_applicable'
# The criteria section ends where the packet path is printed; no control character may reach it.
if sed -n '/^==> Acceptance criteria$/,/^Review packet:/p' "$OUT" | grep -q -e "$ESC" -e "$CR"; then
  fail "criteria output must not carry control characters"
fi
PACKET="$RECORDS/review-packet.md"
grep -q '^## Acceptance criteria$' "$PACKET" || fail "the packet should list the acceptance criteria"
grep -q '^- scope: Nothing outside tracked\.txt changes\.' "$PACKET" || fail "the packet should list each criterion"
grep -q 'any acceptance criterion' "$PACKET" || fail "the brief should block on an unmet criterion"
grep -q '"id": "docs"' "$PACKET" || fail "the findings template should hold one answer per criterion"
run_id=$(head -n 1 "$(sh "$HARNESS_ROOT_UNDER_TEST/scripts/run-paths.sh" current "$HARNESS_DB_ROOT")")
CONTRACT="$HARNESS_DB_ROOT/runs/$run_id/task.json"
[ -f "$CONTRACT" ] || fail "the run should store the contract"

# Answers are optional, but each is well formed, and a not_met answer blocks
# even an approving verdict.
TREE=$(sh "$HARNESS_ROOT_UNDER_TEST/scripts/tree-hash.sh" "$PROJECT")
answers() {
  printf '{"tree_hash":"%s","reviewer":"subagent","verdict":"approve","findings":[],"criteria":[%s]}\n' "$TREE" "$1" > "$TMP_ROOT/answers.json"
}
check() {
  expected=$1
  shift
  status=0
  python3 "$HARNESS_ROOT_UNDER_TEST/scripts/review_findings.py" check --findings "$TMP_ROOT/answers.json" --project "$PROJECT" "$@" > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "review_findings check exited $status, expected $expected"
}
answers '{"id":"tests","answer":"met","evidence":"make test passed"},{"id":"docs","answer":"not_applicable","evidence":"no docs exist"},{"id":"scope","answer":"met","evidence":"diff touches tracked.txt only"}'
check 0 --contract "$CONTRACT"
expect_output 'OK: subagent approved .*3 criterion answer'
if grep -q WARN "$OUT"; then fail "a complete set of answers should not warn"; fi
answers '{"id":"tests","answer":"met","evidence":"make test passed"},{"id":"scope","answer":"not_met","evidence":"new.txt is outside the scope"}'
check 1 --contract "$CONTRACT"
expect_output '\[not met\] scope: new.txt is outside the scope'
expect_output 'WARN: the reviewer did not answer criteria: docs'
answers '{"id":"tests","answer":"met","evidence":"ok"},{"id":"typo","answer":"met","evidence":"ok"}'
check 0 --contract "$CONTRACT"
expect_output 'answered criteria the contract does not have: typo'
answers '{"id":"tests","answer":"maybe","evidence":"ok"}'
check 2
expect_output 'answer must be one of met, not_met, not_applicable'
answers '{"id":"tests","answer":"met","evidence":"ok"},{"id":"tests","answer":"met","evidence":"again"}'
check 2
expect_output 'criterion tests is answered twice'
answers '{"id":"tests","answer":"met"}'
check 2
expect_output 'evidence must be a nonempty string'
# Findings without answers pass as before; the contract only adds a warning.
printf '{"tree_hash":"%s","reviewer":"subagent","verdict":"approve","findings":[]}\n' "$TREE" > "$TMP_ROOT/answers.json"
check 0 --contract "$CONTRACT"
expect_output 'WARN: the reviewer did not answer criteria: tests, docs, scope'
check 0

# With the review-off switch, build done completes the run and review.sh starts no reviewer.
HARNESS_DB_ROOT="$TMP_ROOT/db-off"
mkdir -p "$HARNESS_DB_ROOT"
: > "$HARNESS_DB_ROOT/review-off"
printf '%s\n' "touch $TMP_ROOT/reviewer-ran" > "$HARNESS_DB_ROOT/reviewer"
(cd "$PROJECT" && "$CLI" plan start && "$CLI" contract waive "fixture task" && "$CLI" plan "done" && "$CLI" build start) > "$OUT" 2>&1 || fail "phases failed with review off"
"$HARNESS_ROOT_UNDER_TEST/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed with review off"
(cd "$PROJECT" && "$CLI" build "done") > "$OUT" 2>&1 || fail "build done failed with review off"
expect_output 'Review is off'
(cd "$PROJECT" && "$CLI" status) > "$OUT" 2>&1 || :
grep -qi 'complete' "$OUT" || fail "run is not complete after build done with review off"
"$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || fail "review.sh failed with review off"
expect_output 'Review is off'
[ ! -e "$TMP_ROOT/reviewer-ran" ] || fail "review.sh started the reviewer with review off"

printf '%s\n' 'PASS: review writes an independent packet, lists the acceptance criteria, runs a configured reviewer, and gates done on its verdict'
