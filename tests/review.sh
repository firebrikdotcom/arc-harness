#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEW="$HARNESS_ROOT_UNDER_TEST/scripts/review.sh"
CLI="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-review.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
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
HARNESS_REVIEWER_CMD="$REVIEWER \"\$1\" \"\$2\"" "$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || fail "review with a reviewer failed"
expect_output 'Running the independent reviewer'
expect_output 'OK: fixture-reviewer approved'
grep -q '"verdict":"approve"' "$RECORDS/review-findings.json" || fail "approving findings should be stored"

# A malformed reviewer answer is not stored.
rm -f "$RECORDS/review-findings.json"
# shellcheck disable=SC2016 # "$2" belongs to the generated reviewer script
printf '#!/usr/bin/env sh\nprintf "%%s" "{not json" > "$2"\n' > "$REVIEWER"
HARNESS_REVIEWER_CMD="$REVIEWER \"\$1\" \"\$2\"" "$REVIEW" --project "$PROJECT" > "$OUT" 2>&1 || true
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

printf '%s\n' 'PASS: review writes an independent packet, runs a configured reviewer, and gates done on its verdict'
