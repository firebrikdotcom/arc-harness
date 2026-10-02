#!/usr/bin/env sh
set -eu

# The fresh-session review: scripts/review.sh --reviewer hands a packet of the
# acceptance criteria and the target diff to a separate reviewer process. A fake
# reviewer records what it received; no real reviewer or network is used.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
REVIEW="$HARNESS_ROOT_UNDER_TEST/scripts/review.sh"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-fresh-review.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
# Nothing from the caller's session may decide these cases.
unset HARNESS_TASK HARNESS_REVIEWER_COMMAND HARNESS_REVIEWER_ENV HARNESS_REVIEWER_TIMEOUT \
  HARNESS_REVIEWER_MAX_BYTES HARNESS_REQUIRED_CHECKS
HARNESS_JEV_CHECKPOINTS=0
export HARNESS_JEV_CHECKPOINTS
OUT="$TMP_ROOT/out.txt"
PROJECT="$TMP_ROOT/project"
CAP="$TMP_ROOT/capture"

if ! command -v python3 >/dev/null 2>&1; then
  printf '%s\n' 'SKIP: fresh-session review tests need python3'
  exit 0
fi

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

expect_output() {
  grep -q -e "$1" "$OUT" || fail "expected output to match: $1"
}

record_value() {
  sed -n "s/^$1=//p" "$HARNESS_DB_ROOT/records/review.state" | tail -n 1
}

# A git project with a committed file, an unstaged edit, an untracked file,
# and an ignored private file.
mkdir -p "$PROJECT"
git -C "$PROJECT" init -q
printf '%s\n' 'private.txt' > "$PROJECT/.gitignore"
printf '%s\n' 'tracked original' > "$PROJECT/tracked.txt"
git -C "$PROJECT" add .gitignore tracked.txt
git -C "$PROJECT" -c user.email=t@example.com -c user.name=t commit -q -m files
printf '%s\n' 'UNSTAGED_EDIT_LINE' >> "$PROJECT/tracked.txt"
printf '%s\n' 'UNTRACKED_NEW_LINE' > "$PROJECT/new.txt"
printf '%s\n' 'IGNORED_PRIVATE_LINE' > "$PROJECT/private.txt"

# The task lives outside the project; only its criteria may reach the reviewer.
TASK="$TMP_ROOT/task.md"
cat > "$TASK" <<'EOF'
# Task: sample

## Goal

GOAL_TEXT_NOT_SENT

## Acceptance Criteria

- [ ] First criterion.
- [ ] Second criterion.

## Constraints

- NOT_A_CRITERION
EOF

# The fake reviewer saves its argv, working directory, directory listing,
# environment, and stdin, then answers as MODE says.
FAKE="$TMP_ROOT/fake-reviewer.sh"
cat > "$FAKE" <<'EOF'
#!/usr/bin/env sh
cap=$1
mode=$2
mkdir -p "$cap"
for arg in "$@"; do printf '[%s]\n' "$arg"; done > "$cap/argv"
pwd -P > "$cap/cwd"
ls -A > "$cap/ls"
env > "$cap/env"
cat > "$cap/stdin"
case "$mode" in
  pass) printf '%s\n' 'AC1: met - tracked.txt' 'AC2: met - new.txt' 'No defects found.' '**VERDICT: PASS**' ;;
  fail) printf '%s\n' 'AC1: not met - missing' 'VERDICT: FAIL' ;;
  last) printf '%s\n' 'VERDICT: PASS' 'On reflection:' 'VERDICT: FAIL' ;;
  none) printf '%s\n' 'I have no opinion.' ;;
  crash) printf '%s\n' 'VERDICT: PASS'; exit 3 ;;
  sleep) sleep 30 ;;
esac
EOF

# command_file NAME MODE [ARG...]  Writes a JSON argv file for the fake reviewer.
command_file() {
  _name=$1
  shift
  python3 -c 'import json, sys; print(json.dumps(sys.argv[1:]))' sh "$FAKE" "$CAP" "$@" > "$TMP_ROOT/$_name.json"
  printf '%s\n' "$TMP_ROOT/$_name.json"
}

# review EXPECTED_EXIT ARGS...  Runs review.sh on the project with a fresh capture.
review() {
  _expected=$1
  shift
  rm -rf "$CAP"
  _status=0
  "$REVIEW" --project "$PROJECT" "$@" > "$OUT" 2>&1 || _status=$?
  [ "$_status" -eq "$_expected" ] || fail "review.sh $* exited $_status, expected $_expected"
}

# Capture the exact environment passed to Popen before a shell can add its own
# local variables. Unlisted parent values cannot be injected by the runner.
python3 - "$HARNESS_ROOT_UNDER_TEST/scripts/fresh_review.py" "$TMP_ROOT" "$PROJECT" <<'PY_ENV' || fail "the initial process environment must exactly follow the allowlist"
import importlib.util, os, pathlib, sys
from unittest.mock import patch
spec = importlib.util.spec_from_file_location("review_runner", sys.argv[1])
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
parent = {"PATH": "/fixture/bin", "HOME": "/fixture/home", "USER": "fixture", "LANG": "C", "PASSED_VAR": "explicit", "PWD": "/parent/session", "HARNESS_TASK": "parent-task", "CODEX_THREAD_ID": "parent-session", "SECRET_MARKER": "private"}
expected = {"PATH": "/fixture/bin", "HOME": "/fixture/home", "USER": "fixture", "LANG": "C", "PASSED_VAR": "explicit", "HARNESS_AUTO_INIT": "0"}
class Reviewer:
    returncode = 0
    def __init__(self, command, **kwargs):
        assert kwargs["env"] == expected, kwargs["env"]
        kwargs["stdout"].write(b"VERDICT: PASS\n")
    def communicate(self, **kwargs):
        pass
with patch.dict(os.environ, parent, clear=True), patch.object(runner.subprocess, "Popen", Reviewer):
    result = runner.run_review(["fixture"], b"criteria and diff", pathlib.Path(sys.argv[2]) / "env-review", 10, 1000, "PASSED_VAR", [pathlib.Path(sys.argv[3])])
    assert result == 0
PY_ENV

# --- a passing fresh review ------------------------------------------------------

PASS_CMD=$(command_file pass pass '' 'two words')
export CLAUDE_CODE_SESSION_ID=parent-session-marker CLAUDECODE=1 SECRET_TOKEN_MARKER=should-not-pass \
  PASSED_VAR=passed-through
export HARNESS_REVIEWER_ENV=PASSED_VAR
review 0 --task "$TASK" --reviewer "$PASS_CMD"
unset HARNESS_REVIEWER_ENV
expect_output '^==> fresh-session review$'
expect_output '^\*\*VERDICT: PASS\*\*$'
expect_output '^Reviewer verdict: pass (reviewer exit 0)'
expect_output 'Review finished\.$'
[ "$(record_value REVIEW_MODE)" = fresh ] || fail "record should say REVIEW_MODE=fresh"
[ "$(record_value REVIEWER_VERDICT)" = pass ] || fail "record should say REVIEWER_VERDICT=pass"
[ "$(record_value REVIEWER_EXIT)" = 0 ] || fail "record should say REVIEWER_EXIT=0"
[ "$(record_value REVIEWER_PROGRAM)" = sh ] || fail "record should name the reviewer program"
cmd_sha=$(python3 -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$PASS_CMD")
[ "$(record_value REVIEWER_COMMAND_SHA256)" = "$cmd_sha" ] || fail "record should hash the reviewer command file"
[ "$(record_value EXIT)" = 0 ] || fail "record should say EXIT=0"

# The argv ran exactly as configured, empty and spaced arguments included.
printf '%s\n' "[$CAP]" '[pass]' '[]' '[two words]' > "$TMP_ROOT/argv.expected"
cmp -s "$CAP/argv" "$TMP_ROOT/argv.expected" || fail "reviewer argv differs from the command file: $(cat "$CAP/argv")"

# It started in an empty scratch directory outside the project holding only the
# packet, and that directory is gone afterwards.
workdir=$(cat "$CAP/cwd")
case "$workdir" in
  "$PROJECT"|"$PROJECT"/*|"$HARNESS_ROOT_UNDER_TEST"|"$HARNESS_ROOT_UNDER_TEST"/*) fail "reviewer ran inside the project or harness: $workdir" ;;
esac
[ "$(cat "$CAP/ls")" = "REVIEW.md" ] || fail "reviewer directory should hold only REVIEW.md, got: $(cat "$CAP/ls")"
[ ! -e "$workdir" ] || fail "reviewer scratch directory should be removed: $workdir"

# The same packet arrived on stdin, as REVIEW.md, and in the private review dir.
review_dir=$(sed -n 's/^Reviewer verdict: .* kept in //p' "$OUT")
[ -f "$review_dir/packet.md" ] || fail "packet should be kept in the review directory"
case "$review_dir" in "$HARNESS_DB_ROOT/reviews/"*) ;; *) fail "review directory should be under the database: $review_dir" ;; esac
cmp -s "$CAP/stdin" "$review_dir/packet.md" || fail "reviewer stdin differs from the kept packet"
sha=$(python3 -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$CAP/stdin")
[ "$(record_value REVIEW_PACKET_SHA256)" = "$sha" ] || fail "record packet hash differs from the packet the reviewer read"
grep -q '^PACKET_BYTES=' "$review_dir/result.state" || fail "runner result should record the packet size"

# The packet holds the criteria, the verification exit, the target diff, and
# the answer format, and nothing else from the task, the run, or the session.
packet="$CAP/stdin"
for want in '^AC1\. First criterion\.$' '^AC2\. Second criterion\.$' '^scripts/verify\.sh exit: 0$' \
  '^+UNSTAGED_EDIT_LINE$' '^+UNTRACKED_NEW_LINE$' '^----- BEGIN DIFF -----$' "VERDICT: PASS"; do
  grep -q -e "$want" "$packet" || fail "packet should contain: $want"
done
for unwanted in GOAL_TEXT_NOT_SENT NOT_A_CRITERION IGNORED_PRIVATE_LINE parent-session-marker \
  "$TASK" "$HARNESS_DB_ROOT" "$HARNESS_ROOT_UNDER_TEST"; do
  if grep -qF -e "$unwanted" "$packet"; then fail "packet must not contain: $unwanted"; fi
done

# Only allowlisted and operator-named variables reached the reviewer.
grep -q '^PASSED_VAR=passed-through$' "$CAP/env" || fail "a variable named in HARNESS_REVIEWER_ENV should pass"
grep -q '^HARNESS_AUTO_INIT=0$' "$CAP/env" || fail "the reviewer should run with HARNESS_AUTO_INIT=0"
grep -q '^PATH=' "$CAP/env" || fail "PATH should reach the reviewer"
for unwanted in CLAUDE_CODE_SESSION_ID CLAUDECODE SECRET_TOKEN_MARKER HARNESS_DB_ROOT HARNESS_JEV_CHECKPOINTS HARNESS_REVIEWER_ENV HARNESS_REVIEW_PACKET; do
  if grep -q "^$unwanted=" "$CAP/env"; then fail "$unwanted must not reach the reviewer"; fi
done
unset CLAUDE_CODE_SESSION_ID CLAUDECODE SECRET_TOKEN_MARKER PASSED_VAR

# An in-project TMPDIR cannot place the fresh reviewer inside either root.
mkdir -p "$PROJECT/tmp"
TMPDIR="$PROJECT/tmp" review 0 --task "$TASK" --reviewer "$PASS_CMD"
workdir=$(cat "$CAP/cwd")
case "$workdir" in "$PROJECT"|"$PROJECT"/*|"$HARNESS_ROOT_UNDER_TEST"|"$HARNESS_ROOT_UNDER_TEST"/*) fail "TMPDIR put the reviewer inside a project: $workdir" ;; esac

# All retained review material is private even under an ordinary umask.
(umask 022; review 0 --task "$TASK" --reviewer "$PASS_CMD")
review_dir=$(sed -n 's/^Reviewer verdict: .* kept in //p' "$OUT")
python3 - "$review_dir" <<'PY_PERMISSIONS' || fail "review artifacts must be private"
import pathlib, stat, sys
root = pathlib.Path(sys.argv[1])
assert stat.S_IMODE(root.stat().st_mode) == 0o700
assert stat.S_IMODE(root.parent.stat().st_mode) == 0o700
for path in root.iterdir():
    assert stat.S_IMODE(path.stat().st_mode) == 0o600, path
PY_PERMISSIONS

# A project check can edit the original command file, but the validated snapshot
# is what runs and what the review record identifies.
if command -v make >/dev/null 2>&1; then
  cp "$PASS_CMD" "$TMP_ROOT/pass-original.json"
  FAIL_CMD=$(command_file snapshot-fail fail)
  printf 'lint:\n\tcp "%s" "%s"\n' "$FAIL_CMD" "$PASS_CMD" > "$PROJECT/Makefile"
  review 0 --task "$TASK" --reviewer "$PASS_CMD"
  [ "$(record_value REVIEWER_COMMAND_SHA256)" = "$cmd_sha" ] || fail "execution must identify the validated command snapshot"
  cmp -s "$PASS_CMD" "$FAIL_CMD" || fail "the check did not edit the original profile"
  cp "$TMP_ROOT/pass-original.json" "$PASS_CMD"
  rm "$PROJECT/Makefile"
  cp "$TASK" "$TMP_ROOT/task-original.md"
  printf '%s\n' '# Task with no criteria after verification' > "$TMP_ROOT/task-replaced.md"
  printf 'lint:\n\tcp "%s" "%s"\n' "$TMP_ROOT/task-replaced.md" "$TASK" > "$PROJECT/Makefile"
  review 0 --task "$TASK" --reviewer "$PASS_CMD"
  grep -q '^AC1\. First criterion\.$' "$CAP/stdin" || fail "review must keep the validated acceptance criteria"
  cmp -s "$TASK" "$TMP_ROOT/task-replaced.md" || fail "the check did not replace the task"
  cp "$TMP_ROOT/task-original.md" "$TASK"
  rm "$PROJECT/Makefile"
else
  printf '%s\n' 'SKIP: command snapshot mutation test needs make'
fi

# HARNESS_REVIEWER_COMMAND names the command as well.
export HARNESS_REVIEWER_COMMAND="$PASS_CMD"
review 0 --task "$TASK"
unset HARNESS_REVIEWER_COMMAND
[ "$(record_value REVIEW_MODE)" = fresh ] || fail "HARNESS_REVIEWER_COMMAND should start a fresh review"

# --- verdicts that do not pass ---------------------------------------------------

for case_spec in "fail:fail" "last:fail" "none:none" "crash:pass"; do
  mode=${case_spec%%:*}
  verdict=${case_spec#*:}
  review 1 --task "$TASK" --reviewer "$(command_file "$mode" "$mode")"
  [ "$(record_value REVIEW_MODE)" = fresh ] || fail "$mode: record should say fresh"
  [ "$(record_value REVIEWER_VERDICT)" = "$verdict" ] || fail "$mode: verdict should be $verdict, got $(record_value REVIEWER_VERDICT)"
  [ "$(record_value EXIT)" = 1 ] || fail "$mode: a review the reviewer did not pass must record EXIT=1"
  expect_output '^Review finished: the fresh-session reviewer did not pass the change'
done
[ "$(record_value REVIEWER_EXIT)" = 3 ] || fail "crash: record should keep the reviewer exit"
expect_output '^FAIL: the reviewer exited 3\.$'

# A reviewer that runs past the timeout is stopped.
started=$(date +%s)
export HARNESS_REVIEWER_TIMEOUT=1
review 1 --task "$TASK" --reviewer "$(command_file sleep sleep)"
unset HARNESS_REVIEWER_TIMEOUT
[ $(( $(date +%s) - started )) -lt 20 ] || fail "timed-out reviewer was not stopped promptly"
[ "$(record_value REVIEWER_VERDICT)" = timeout ] || fail "timeout: verdict should be timeout"
expect_output '^FAIL: the reviewer did not finish within HARNESS_REVIEWER_TIMEOUT=1s'

# A packet over the size cap is refused whole, and no reviewer starts.
export HARNESS_REVIEWER_MAX_BYTES=100
review 1 --task "$TASK" --reviewer "$PASS_CMD"
unset HARNESS_REVIEWER_MAX_BYTES
[ "$(record_value REVIEWER_VERDICT)" = too_large ] || fail "oversized packet: verdict should be too_large"
[ ! -e "$CAP/argv" ] || fail "oversized packet: the reviewer must not start"

# A program that does not exist cannot pass.
printf '%s\n' '["/nonexistent/reviewer"]' > "$TMP_ROOT/missing-program.json"
review 1 --task "$TASK" --reviewer "$TMP_ROOT/missing-program.json"
[ "$(record_value REVIEWER_VERDICT)" = not_started ] || fail "missing program: verdict should be not_started"

# Failing verification: the reviewer is not started and the record says why.
printf '%s\n' 'lint' > "$PROJECT/.harness-required-checks"
status=0
rm -rf "$CAP"
"$REVIEW" --project "$PROJECT" --task "$TASK" --reviewer "$PASS_CMD" > "$OUT" 2>&1 || status=$?
[ "$status" -ne 0 ] || fail "review should fail when verification fails"
[ ! -e "$CAP/argv" ] || fail "the reviewer must not start when verification fails"
[ "$(record_value REVIEWER_VERDICT)" = skipped ] || fail "failed verify: verdict should be skipped"
expect_output '^SKIP: verification failed, so the reviewer was not started\.$'
rm "$PROJECT/.harness-required-checks"

# --- refused before verification -------------------------------------------------

# refused EXPECTED_MESSAGE ARGS...  The review stops with exit 2 before it starts.
refused() {
  _message=$1
  shift
  review 2 "$@"
  expect_output "$_message"
  if grep -q 'Review started' "$OUT"; then fail "review.sh $* should stop before the review starts"; fi
  [ ! -e "$CAP/argv" ] || fail "review.sh $* must not start the reviewer"
}

refused 'needs the task.s acceptance criteria' --reviewer "$PASS_CMD"
printf '%s\n' '# Task' '' '## Goal' '- NOT_A_CRITERION' > "$TMP_ROOT/no-criteria.md"
refused 'lists none under an Acceptance Criteria heading' --task "$TMP_ROOT/no-criteria.md" --reviewer "$PASS_CMD"
refused 'cannot read reviewer command file' --task "$TASK" --reviewer "$TMP_ROOT/absent.json"
printf '%s\n' '[]' > "$TMP_ROOT/empty.json"
refused 'must contain a JSON argv array' --task "$TASK" --reviewer "$TMP_ROOT/empty.json"
printf '%s\n' '"claude -p"' > "$TMP_ROOT/string.json"
refused 'must contain a JSON argv array' --task "$TASK" --reviewer "$TMP_ROOT/string.json"
printf '%s\n' '["", "x"]' > "$TMP_ROOT/blank-program.json"
refused 'must contain a JSON argv array' --task "$TASK" --reviewer "$TMP_ROOT/blank-program.json"
python3 -c 'import json; print(json.dumps(["sh", "bad\0argument"]))' > "$TMP_ROOT/nul-command.json"
refused 'arguments must not contain NUL' --task "$TASK" --reviewer "$TMP_ROOT/nul-command.json"
python3 -c 'import json; print(json.dumps(["sh", "\ud800"]))' > "$TMP_ROOT/unencodable-command.json"
refused 'arguments must be encodable for execution' --task "$TASK" --reviewer "$TMP_ROOT/unencodable-command.json"
refused '--reviewer requires a command file' --task "$TASK" --reviewer
export HARNESS_REVIEWER_ENV='HARNESS_DB_ROOT'
refused 'may not pass harness state' --task "$TASK" --reviewer "$PASS_CMD"
unset HARNESS_REVIEWER_ENV
for session_name in CODEX_THREAD_ID CLAUDE_CODE_SESSION_ID CLAUDECODE; do
  HARNESS_REVIEWER_ENV=$session_name
  export HARNESS_REVIEWER_ENV
  refused 'may not pass agent-session state' --task "$TASK" --reviewer "$PASS_CMD"
done
unset HARNESS_REVIEWER_ENV
export HARNESS_REVIEWER_ENV='BAD-NAME'
refused 'invalid variable name' --task "$TASK" --reviewer "$PASS_CMD"
unset HARNESS_REVIEWER_ENV
export HARNESS_REVIEWER_TIMEOUT=0
refused 'HARNESS_REVIEWER_TIMEOUT must be a positive integer' --task "$TASK" --reviewer "$PASS_CMD"
unset HARNESS_REVIEWER_TIMEOUT

# --- the fresh review is the default ---------------------------------------------

# With no reviewer configured the review is misconfigured and stops.
refused '^FAIL: no fresh-session reviewer is configured\.' --task "$TASK"
expect_output 'pass --reviewer FILE or set HARNESS_REVIEWER_COMMAND=FILE'

# --self is an explicit inspection: the #42 output, no reviewer, and a record
# that says self (harness review done refuses it; see tests/harness-cli.sh).
# It wins over an inherited reviewer command but not over an explicit one.
export HARNESS_REVIEWER_COMMAND="$PASS_CMD"
review 0 --task "$TASK" --self
unset HARNESS_REVIEWER_COMMAND
[ ! -e "$CAP/argv" ] || fail "--self must not start the reviewer"
[ "$(record_value REVIEW_MODE)" = self ] || fail "--self should record REVIEW_MODE=self"
[ "$(record_value REVIEWER_VERDICT)" = none ] || fail "--self should record REVIEWER_VERDICT=none"
[ "$(record_value REVIEWER_PROGRAM)" = none ] || fail "--self should record no reviewer program"
expect_output '^AC1\. First criterion\.$'
expect_output '^Review finished (self inspection only)'
# Anchored: the printed harness patch contains this test file's own text.
if grep -q '^==> fresh-session review$' "$OUT"; then fail "a self inspection should not start a reviewer"; fi
refused 'cannot be combined' --task "$TASK" --self --reviewer "$PASS_CMD"

# A program name that could break the KEY=VALUE record is stored sanitized.
python3 -c 'import json; print(json.dumps(["/nonexistent/re\nviewer"]))' > "$TMP_ROOT/newline-program.json"
review 1 --task "$TASK" --reviewer "$TMP_ROOT/newline-program.json"
[ "$(record_value REVIEWER_PROGRAM)" = "re_viewer" ] || fail "program name should be sanitized, got $(record_value REVIEWER_PROGRAM)"
if grep -q '^viewer' "$HARNESS_DB_ROOT/records/review.state"; then fail "program name injected a record line"; fi

printf '%s\n' 'PASS: fresh-session review isolates the reviewer, sends only criteria and diff, and gates on its verdict'
