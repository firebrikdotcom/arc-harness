#!/usr/bin/env sh
# Retrieval reminder: the Grep/Glob PreToolUse hook and its installer entry.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-reminder.XXXXXX")
# Physical path: the registry stores resolved roots, and /var is a symlink on macOS.
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
trap 'chmod -R u+rwx "${TMP_ROOT:?}" 2>/dev/null; rm -rf "${TMP_ROOT:?}"' EXIT HUP INT TERM
OUT="$TMP_ROOT/out.txt"
HOOK="$ROOT/scripts/retrieval-reminder.sh"

fail() {
  printf 'FAIL: %s\n' "$*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

command -v python3 >/dev/null 2>&1 || { printf 'SKIP: python3 is unavailable\n'; exit 0; }
# Installed hooks run by path, so the executable bit is part of the contract.
[ -x "$HOOK" ] || fail "hook is not executable: $HOOK"

# payload TOOL CWD SESSION  Hook payload as Claude Code sends it.
payload() {
  python3 -c 'import json, sys
tool, cwd, session = sys.argv[1:4]
print(json.dumps({"hook_event_name": "PreToolUse", "session_id": session, "cwd": cwd,
                  "tool_name": tool, "tool_input": {"pattern": "needle"}}))' "$@"
}

# run TOOL CWD SESSION  Runs the hook; fails unless it exits 0.
run() {
  payload "$@" | "$HOOK" > "$OUT" 2>&1 || fail "hook exited non-zero for $*"
}

reminded() {
  python3 - "$OUT" <<'PY'
import json, sys
text = open(sys.argv[1]).read().strip()
if not text:
    sys.exit(1)
lines = text.splitlines()
assert len(lines) == 1, lines
data = json.loads(lines[0])["hookSpecificOutput"]
assert data["hookEventName"] == "PreToolUse", data
assert "permissionDecision" not in data, data
context = data["additionalContext"]
assert "\n" not in context and "scripts/jg.sh" in context and "--project" in context, context
PY
}

silent() {
  [ ! -s "$OUT" ] || fail "expected no output: $*"
}

export HARNESS_ROOT="$ROOT"
export HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_JEV_CHECKPOINTS=1

PROJECT="$TMP_ROOT/project"
OTHER="$TMP_ROOT/other"
mkdir -p "$PROJECT/sub" "$OTHER"
tdir=$(scripts/harness-target.sh register "$PROJECT")
DB="$tdir/db"

# 1. Disabled checkpoints: silent even on the first Grep.
(HARNESS_JEV_CHECKPOINTS=0; run Grep "$PROJECT" s-off)
silent "HARNESS_JEV_CHECKPOINTS=0"
[ ! -d "$DB/retrieval-reminders" ] || fail "disabled hook wrote state"

# 2. First Grep of a session in a registered target without retrieval: one line.
run Grep "$PROJECT/sub" s-one
reminded || fail "first Grep did not print one context line"
grep -qF "$PROJECT" "$OUT" || fail "reminder should name the target root"
# Later Grep or Glob calls in the same session stay silent.
run Glob "$PROJECT" s-one
silent "second call in the same session"
run Grep "$PROJECT" s-one
silent "third call in the same session"
# The marker stores a checksum, never the session id.
[ "$(find "$DB/retrieval-reminders" -type f | wc -l | tr -d ' ')" -eq 1 ] || fail "expected one session marker"
if grep -rq 's-one' "$DB/retrieval-reminders"; then fail "session id stored in clear"; fi
[ -z "$(find "$DB/retrieval-reminders" -name '*s-one*')" ] || fail "session id used as a file name"

# 3. A new session gets its own first reminder; Glob counts as well.
run Glob "$PROJECT" s-two
reminded || fail "first Glob of a new session did not remind"

# 4. Other tools and unregistered directories are ignored.
run Bash "$PROJECT" s-bash
silent "Bash tool"
run Grep "$OTHER" s-other
silent "unregistered directory"
run Grep "$TMP_ROOT/missing" s-missing
silent "missing directory"
printf 'not json' | "$HOOK" > "$OUT" 2>&1 || fail "malformed payload exited non-zero"
silent "malformed payload"

# 5. A retrieval record for the live run suppresses the reminder; one from another run does not.
mkdir -p "$DB/runs/run-1" "$DB/retrieval"
printf 'run-1\n' > "$DB/runs/current"
printf 'RUN_ID=run-1\nRUN_STATUS=active\n' > "$DB/runs/run-1/state"
printf 'RECORD_KIND=jevgrep\nRUN_ID=run-0\n' > "$DB/retrieval/old.state"
run Grep "$PROJECT" s-three
reminded || fail "a retrieval from an earlier run should not suppress the reminder"
printf 'RECORD_KIND=jevgrep\nRUN_ID=run-1\n' > "$DB/retrieval/now.state"
run Grep "$PROJECT" s-four
silent "retrieval already recorded for the current run"
# A budget pause keeps the same run, so its retrieval still counts.
printf 'RUN_ID=run-1\nRUN_STATUS=paused\n' > "$DB/runs/run-1/state"
run Grep "$PROJECT" s-paused
silent "retrieval recorded for a paused run"
# A finished run is not the current run: its retrieval does not count.
printf 'RUN_ID=run-1\nRUN_STATUS=complete\n' > "$DB/runs/run-1/state"
run Grep "$PROJECT" s-complete
reminded || fail "a retrieval from a completed run should not suppress the reminder"
rm -f "$DB/retrieval/now.state"

# 6. A .harness-no-upload target is never nudged toward retrieval.
: > "$PROJECT/.harness-no-upload"
run Grep "$PROJECT/sub" s-five
silent ".harness-no-upload target"
rm -f "$PROJECT/.harness-no-upload"

# 7. Unwritable reminder state never blocks the tool call and stays silent.
if [ "$(id -u)" -ne 0 ]; then
  chmod 500 "$DB/retrieval-reminders"
  run Grep "$PROJECT" s-readonly
  silent "unwritable marker directory"
  chmod 700 "$DB/retrieval-reminders"
fi
mv "$DB/retrieval-reminders" "$DB/reminders.saved"
: > "$DB/retrieval-reminders"
run Grep "$PROJECT" s-blocked
silent "marker path is a regular file"
rm -f "$DB/retrieval-reminders"
mv "$DB/reminders.saved" "$DB/retrieval-reminders"

# 7b. Without a session id all calls share one key: remind once, then stay silent.
run Grep "$PROJECT" ""
reminded || fail "first call without a session id should remind"
run Grep "$PROJECT" ""
silent "second call without a session id"

# 7c. The harness root itself counts as a target even when it is not registered.
run Grep "$ROOT/docs" s-root
reminded || fail "harness root should be treated as a target"
[ -d "$HARNESS_DB_ROOT/retrieval-reminders" ] || fail "harness root marker should live in the shared database"

# 8. The installer registers the reminder for Claude Code only, idempotently, and removes it on uninstall.
CLAUDE="$TMP_ROOT/claude-settings.json"
CODEX="$TMP_ROOT/codex-hooks.json"
printf '{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"type":"command","command":"user-hook"}]}]}}\n' > "$CLAUDE"
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" > /dev/null
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" > /dev/null
python3 - "$CLAUDE" "$CODEX" "$HOOK" <<'PY' || fail "installer entries are wrong"
import json, sys
claude, codex, hook = sys.argv[1:]
groups = json.load(open(claude))["hooks"]["PreToolUse"]
ours = [g for g in groups if any("retrieval-reminder.sh" in h["command"] for h in g["hooks"])]
assert len(ours) == 1, groups
assert ours[0]["matcher"] == "Grep|Glob", ours
assert ours[0]["hooks"][0]["command"] == f'"{hook}"', ours
assert any(h["command"] == "user-hook" for g in groups for h in g["hooks"]), groups
assert any(g["matcher"] == "Bash" for g in groups), groups
codex_groups = json.load(open(codex))["hooks"]["PreToolUse"]
assert not any("retrieval-reminder.sh" in h["command"] for g in codex_groups for h in g["hooks"]), codex_groups
PY
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" --uninstall > /dev/null
if grep -q 'retrieval-reminder.sh' "$CLAUDE"; then fail "uninstall left the reminder"; fi
grep -q 'user-hook' "$CLAUDE" || fail "uninstall removed a user hook"

printf '%s\n' 'PASS: retrieval reminder nudges once per session, stays silent where it must, and installs for Claude Code only'
