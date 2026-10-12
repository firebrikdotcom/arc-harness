#!/usr/bin/env sh
# The same command failing the same way twice in a row pauses the run until a
# continue note states a new approach; different errors and successes reset it.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-failures.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_DB_ROOT
unset HARNESS_ROOT HARNESS_BUDGET_REPEAT_FAILURES || true
PROJECT="$TMP_ROOT/project"
mkdir -p "$PROJECT"
OUT="$TMP_ROOT/out.txt"
CLI="$ROOT/scripts/harness"
HOOK="$ROOT/scripts/failure_budget.py"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

# post EXPECTED_EXIT EVENT COMMAND [ERROR] [EXIT_CODE]
post() {
  payload=$(python3 -c 'import json,sys
event, command, error, code = sys.argv[1:5]
data = {"hook_event_name": event, "tool_name": "Bash", "tool_input": {"command": command}, "cwd": sys.argv[5]}
if event == "PostToolUseFailure":
    data["error"] = error
else:
    data["tool_response"] = {"exit_code": int(code), "stderr": error}
print(json.dumps(data))' "$2" "$3" "${4:-}" "${5:-0}" "$PROJECT")
  status=0
  printf '%s' "$payload" | python3 "$HOOK" > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$1" ] || fail "hook exited $status, expected $1 for $2 $3"
}

run_status() {
  (cd "$PROJECT" && "$CLI" status) | sed -n 's/^Run:[[:space:]]*.*(\([a-z]*\))$/\1/p'
}

(cd "$PROJECT" && "$CLI" plan start) > "$OUT" 2>&1 || fail "plan start failed"

# First failure: recorded, run continues.
post 0 PostToolUseFailure "make test" "Exit code 2
FAIL test_parse at line 12 (/tmp/tmp.ab12CD/x.py) after 0.31s"
[ "$(run_status)" = "active" ] || fail "one failure must not pause"

# The same error again (only volatile numbers and temp paths differ): paused.
post 2 PostToolUseFailure "make  test" "Exit code 2
FAIL test_parse at line 12 (/tmp/tmp.zz99QQ/x.py) after 0.47s"
grep -q 'same failure happened 2 times' "$OUT" || fail "the pause notice should reach the agent"
grep -q 'Exit code N' "$OUT" && fail "the detail should be the raw first line, not the normalized one"
[ "$(run_status)" = "paused" ] || fail "two identical failures should pause the run"
(cd "$PROJECT" && "$CLI" status) > "$OUT"
grep -q 'Paused on:    repeat_failure' "$OUT" || fail "status should name the repeat_failure pause"
RUN_DIR="$HARNESS_DB_ROOT/runs/$(head -n 1 "$HARNESS_DB_ROOT/runs/current")"
grep -q '"budget": "repeat_failure"' "$RUN_DIR/pauses/001.json" || fail "pause record should name the budget"
grep -q '"detail": "Exit code 2"' "$RUN_DIR/pauses/001.json" || fail "pause record should carry the first error line"
grep -q 'make test' "$RUN_DIR/failures" && fail "the command text must not be stored"

# Continue with a new approach clears the count.
(cd "$PROJECT" && "$CLI" continue "Parser fails on CRLF input; switching to splitlines().") > "$OUT" 2>&1 || fail "continue failed"
grep -q 'Repeated-failure count cleared' "$OUT" || fail "continue should clear the count"
post 0 PostToolUseFailure "make test" "Exit code 2
FAIL test_parse at line 12"
[ "$(run_status)" = "active" ] || fail "the first failure after continue must not pause"

# A different error for the same command restarts the count; a success clears it.
post 0 PostToolUseFailure "make test" "Exit code 2
FAIL test_other at line 40"
[ "$(run_status)" = "active" ] || fail "a different error must not pause"
post 0 PostToolUse "make test" "" 0
post 0 PostToolUseFailure "make test" "Exit code 2
FAIL test_other at line 40"
[ "$(run_status)" = "active" ] || fail "a success must reset the count"

# Codex-style payloads report failure through the exit code.
post 0 PostToolUse "cargo build" "error[E0308]: mismatched types" 101
post 2 PostToolUse "cargo build" "error[E0308]: mismatched types" 101
[ "$(run_status)" = "paused" ] || fail "exit-code failures should count too"

# Harness bookkeeping never counts, and a zero cap disables the stop.
rm -rf "$HARNESS_DB_ROOT"
HARNESS_BUDGET_REPEAT_FAILURES=0
export HARNESS_BUDGET_REPEAT_FAILURES
(cd "$PROJECT" && "$CLI" plan start) > "$OUT" 2>&1 || fail "plan start failed"
post 0 PostToolUseFailure "make test" "FAIL"
post 0 PostToolUseFailure "make test" "FAIL"
post 0 PostToolUseFailure "scripts/harness status" "boom"
[ "$(run_status)" = "active" ] || fail "a zero cap should disable the stop"
unset HARNESS_BUDGET_REPEAT_FAILURES

# The installer registers the hook for both result events.
"$ROOT/scripts/install-hooks.sh" --dry-run --claude "$TMP_ROOT/claude.json" --codex "$TMP_ROOT/codex.json" --root-file "$TMP_ROOT/root" > "$OUT" 2>&1 \
  || fail "install-hooks dry run failed"
python3 - "$OUT" <<'PY' || fail "install-hooks should register failure_budget.py for PostToolUse and PostToolUseFailure"
import json, sys
text = open(sys.argv[1]).read()
claude = json.loads(text.split("(install, dry run)\n", 2)[1].split("\n---", 1)[0])
events = {event for event, groups in claude["hooks"].items()
          for group in groups for hook in group["hooks"] if "failure_budget.py" in hook["command"]}
assert events == {"PostToolUse", "PostToolUseFailure"}, events
PY

printf '%s\n' 'PASS: two identical failures pause the run; new approaches, new errors, and successes reset it'
