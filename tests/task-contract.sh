#!/usr/bin/env sh
# A run is a bounded contract: plan done needs one (or a recorded waiver), build
# done runs its acceptance commands, review done refuses non-goal paths, and the
# reviewer sees it in the packet.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-contract.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# review submit asks for a typed confirmation on a terminal; tests answer from a file.
printf 'yes\n' > "$TMP_ROOT/confirm"
HARNESS_CONFIRM_TTY="$TMP_ROOT/confirm"
export HARNESS_CONFIRM_TTY
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_ROOT HARNESS_REVIEWER_CMD HARNESS_REVIEW_BASE || true
PROJECT="$TMP_ROOT/project"
OUT="$TMP_ROOT/out.txt"
CLI="$ROOT/scripts/harness"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

# h EXPECTED_EXIT ARGS...  Run the CLI from the project.
h() {
  expected=$1
  shift
  status=0
  (cd "$PROJECT" && "$CLI" "$@") > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "harness $* exited $status, expected $expected"
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

mkdir -p "$PROJECT/src" "$PROJECT/vendor"
git -C "$PROJECT" init -q
printf 'test:\n\t@true\n' > "$PROJECT/Makefile"
printf '%s\n' 'one' > "$PROJECT/src/a.txt"
printf '%s\n' 'lib' > "$PROJECT/vendor/lib.txt"
git -C "$PROJECT" add -A
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m init

h 0 plan start
h 4 plan "done"
expect_output "without a task contract"

# Malformed contracts are refused with the reason.
printf '%s\n' '{"goal":"x","deliverables":[],"non_goals":[],"acceptance":[]}' > "$TMP_ROOT/bad.json"
h 4 contract set "$TMP_ROOT/bad.json"
expect_output "deliverables must be a list with at least 1"
printf '%s\n' '{"goal":"x","deliverables":["src/a.txt"],"non_goals":[],"acceptance":[{"id":"a","criterion":"looks right"}]}' > "$TMP_ROOT/bad.json"
h 4 contract set "$TMP_ROOT/bad.json"
expect_output "at least one acceptance criterion needs a command"

# A valid contract: one criterion reads a marker the build must create.
cat > "$TMP_ROOT/task.json" <<'JSON'
{
  "goal": "Change src/a.txt to say two.",
  "deliverables": ["src/a.txt"],
  "constraints": ["No new files outside src/."],
  "non_goals": [{"description": "Vendored code stays untouched.", "paths": ["vendor/"]}],
  "acceptance": [
    {"id": "content", "criterion": "src/a.txt says two", "command": ["grep", "-qx", "two", "src/a.txt"]},
    {"id": "tone", "criterion": "The wording reads naturally"}
  ]
}
JSON
h 0 contract set "$TMP_ROOT/task.json"
h 0 contract show
expect_output '"goal": "Change src/a.txt to say two."'
h 0 plan "done"
h 4 contract set "$TMP_ROOT/task.json"
expect_output "set while planning"
h 0 build start

# build done runs the acceptance commands: unmet first, met after the change.
"$ROOT/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
h 4 build "done"
expect_output "FAIL: content: src/a.txt says two"
expect_output "REVIEW: tone"
printf '%s\n' 'two' > "$PROJECT/src/a.txt"
"$ROOT/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
h 0 build "done"
expect_output "PASS: content"

# The reviewer sees the contract, and review done refuses a non-goal path.
h 0 review start
printf '%s\n' 'patched' >> "$PROJECT/vendor/lib.txt"
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "review failed"
grep -q 'Change src/a.txt to say two.' "$HARNESS_DB_ROOT/records/review-packet.md" || fail "the packet should hold the contract"
printf '{"tree_hash":"%s","reviewer":"fixture","verdict":"approve","findings":[]}\n' "$(sh "$ROOT/scripts/tree-hash.sh" "$PROJECT")" > "$TMP_ROOT/findings.json"
h 0 review submit "$TMP_ROOT/findings.json"
h 4 review "done"
expect_output "vendor/lib.txt matches non-goal vendor/"
git -C "$PROJECT" checkout -q -- vendor/lib.txt
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "review failed"
printf '{"tree_hash":"%s","reviewer":"fixture","verdict":"approve","findings":[]}\n' "$(sh "$ROOT/scripts/tree-hash.sh" "$PROJECT")" > "$TMP_ROOT/findings.json"
h 0 review submit "$TMP_ROOT/findings.json"
h 0 review "done"
expect_output "none in a non-goal path"

# An acceptance command the denylist refuses never runs.
h 0 plan start
printf '%s\n' '{"goal":"x","deliverables":["y"],"non_goals":[],"acceptance":[{"id":"evil","criterion":"x","command":["rm","-rf","/"]}]}' > "$TMP_ROOT/evil.json"
h 0 contract set "$TMP_ROOT/evil.json"
h 0 plan "done"
h 0 build start
"$ROOT/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
h 4 build "done"
expect_output "refused by the denylist"

# A criterion whose program is missing, or that hangs, fails cleanly.
rm -rf "$HARNESS_DB_ROOT"
h 0 plan start
printf '%s\n' '{"goal":"x","deliverables":["y"],"non_goals":[],"acceptance":[{"id":"missing","criterion":"x","command":["no-such-program-zz"]},{"id":"slow","criterion":"y","command":["sleep","5"]}]}' > "$TMP_ROOT/odd.json"
h 0 contract set "$TMP_ROOT/odd.json"
h 0 plan "done"
h 0 build start
"$ROOT/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
status=0
(cd "$PROJECT" && HARNESS_CRITERION_TIMEOUT=1 "$CLI" build "done") > "$OUT" 2>&1 || status=$?
[ "$status" -eq 4 ] || fail "missing or hung criteria should fail build done, exited $status"
expect_output "FAIL: missing: .*did not run: FileNotFoundError"
expect_output "FAIL: slow: .*did not run: TimeoutExpired"
if grep -q Traceback "$OUT"; then fail "no traceback for a missing criterion program"; fi

# A waiver is explicit and travels to the reviewer.
rm -rf "$HARNESS_DB_ROOT"
h 0 plan start
h 0 contract waive "Typo fix in a comment; nothing to accept."
h 0 plan "done"
h 0 build start
"$ROOT/scripts/verify.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "verify failed"
h 0 build "done"
expect_output "Contract waived: Typo fix"
h 0 review start
"$ROOT/scripts/review.sh" --project "$PROJECT" > "$OUT" 2>&1 || fail "review failed"
grep -q '"waived": "Typo fix' "$HARNESS_DB_ROOT/records/review-packet.md" || fail "the packet should show the waiver"

printf '%s\n' 'PASS: contracts bound plan, build, and review; waivers are explicit'
