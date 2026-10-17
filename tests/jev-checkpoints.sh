#!/usr/bin/env sh
# Jev at harness seams: the session route, quiet phase gates, hook behaviour, and the
# hook installer, all against the offline fake router.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
# The fixtures use the legacy run and record paths and model a human at a terminal: the caller's
# native session must not move them, and its agent markers must not wake the todo gate.
unset HARNESS_SESSION_ID CODEX_THREAD_ID CLAUDE_SESSION_ID CLAUDE_CODE_SESSION_ID CLAUDECODE CODEX_SANDBOX || true
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-jev.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
# review submit asks for a typed confirmation on a terminal; tests answer from a file.
printf 'yes\n' > "$TMP_ROOT/confirm"
HARNESS_CONFIRM_TTY="$TMP_ROOT/confirm"
export HARNESS_CONFIRM_TTY

fail() {
  printf 'FAIL: %s\n' "$*"
  exit 1
}

# count DIR  Number of JSON records directly inside DIR (0 when it is absent).
count() {
  [ -d "$1" ] || { printf '0\n'; return 0; }
  find "$1" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' '
}

python3 -m unittest discover -s tests -p test_phase_checkpoint.py -q
python3 -m unittest discover -s tests -p test_audit_emit.py -q

export HARNESS_TYPESAFE_ROUTER="$ROOT/tests/fake_router.py"
export FAKE_ROUTER_LOG_DIR="$TMP_ROOT/logs"
export HARNESS_AUDIT_ENABLED=0
export HARNESS_ROOT="$ROOT"
export HARNESS_DB_ROOT="$TMP_ROOT/db"
# The delegation switch these cases see is the temp dir's (off), never the machine's.
export HARNESS_CONFIG_HOME="$TMP_ROOT/config"
unset HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS HARNESS_BUDGET_TOKENS HARNESS_JEV_DELEGATION || true

# A harness target: a git worktree whose guide carries the harness block.
TARGET="$TMP_ROOT/target"
mkdir -p "$TARGET"
git -C "$TARGET" init -q
printf '<!-- harness-cli:start -->\n<!-- harness-cli:end -->\n' > "$TARGET/AGENTS.md"
printf 'test:\n\t@true\n' > "$TARGET/Makefile"
git -C "$TARGET" -c user.name=t -c user.email=t@example.invalid add AGENTS.md Makefile
git -C "$TARGET" -c user.name=t -c user.email=t@example.invalid commit -q -m init

payload() {
  printf '{"session_id":"s","hook_event_name":"%s","cwd":"%s","tool_name":"%s","tool_input":{"command":"%s"}}' "$1" "$2" "$3" "$4"
}

# 1. Hooks are silent no-ops unless HARNESS_JEV_CHECKPOINTS=1.
out=$(payload SessionStart "$TARGET" "" "" | HARNESS_JEV_CHECKPOINTS=0 sh scripts/hooks/session-route.sh)
[ -z "$out" ] || fail "session-route.sh printed while disabled: $out"
out=$(payload PreToolUse "$TARGET" Bash "ls" | HARNESS_JEV_CHECKPOINTS=0 sh scripts/hooks/jev-observe.sh)
[ -z "$out" ] || fail "jev-observe.sh printed while disabled: $out"

# 2. SessionStart in a harness target records one shadow route and one context line.
export HARNESS_JEV_CHECKPOINTS=1
out=$(payload SessionStart "$TARGET" "" "" | sh scripts/hooks/session-route.sh)
case "$out" in
  "Jev session route observed in shadow mode, not followed: the main agent decides.") ;;
  *) fail "unexpected session-route output: $out" ;;
esac
[ "$(count "$HARNESS_DB_ROOT/routes")" = "1" ] || fail "expected one route record"

# 3. SessionStart outside a harness target stays silent and writes nothing.
mkdir -p "$TMP_ROOT/plain"
out=$(payload SessionStart "$TMP_ROOT/plain" "" "" | sh scripts/hooks/session-route.sh)
[ -z "$out" ] || fail "session-route.sh printed outside a target: $out"
[ "$(count "$HARNESS_DB_ROOT/routes")" = "1" ] || fail "route written outside a target"

# 4. The observe hook needs an active run; without one it is silent.
payload PreToolUse "$TARGET" Bash "make test" | sh scripts/hooks/jev-observe.sh >/dev/null
[ ! -d "$HARNESS_DB_ROOT/advice" ] || fail "observe hook wrote advice without a run"

# The run belongs to the target project: its CLI calls run there.
hj() { (cd "$TARGET" && "$ROOT/scripts/harness" "$@"); }

# 5. With a run, repeated commands are counted but never ask Jev: the gate is mechanical.
(HARNESS_JEV_CHECKPOINTS=0 hj plan start >/dev/null)
for _ in 1 2 3 4; do
  payload PreToolUse "$TARGET" Bash "make test" | sh scripts/hooks/jev-observe.sh >/dev/null
done
[ ! -d "$HARNESS_DB_ROOT/advice" ] || fail "a repeated command asked Jev"

# 6. Phase gates, verify, and review ask Jev nothing and label nothing; verify keeps its history.
hj contract waive "fixture task" >/dev/null
plan_out=$(hj plan "done")
build_out=$(hj build start)
verify_out=$(scripts/verify.sh --project "$TARGET" 2>&1) || fail "verify failed: $verify_out"
hj build "done" >/dev/null
hj review start >/dev/null
review_out=$(scripts/review.sh --project "$TARGET" 2>&1) || fail "review failed: $review_out"
printf '{"tree_hash":"%s","reviewer":"fixture","verdict":"approve","findings":[]}\n' "$(sh scripts/tree-hash.sh "$TARGET")" > "$TMP_ROOT/findings.json"
hj review submit "$TMP_ROOT/findings.json" >/dev/null
done_out=$(hj review "done")
if printf '%s\n' "$plan_out" "$build_out" "$verify_out" "$review_out" "$done_out" | grep -q '^Jev: '; then
  fail "a phase gate printed a Jev line: $plan_out $build_out $verify_out $review_out $done_out"
fi
[ ! -d "$HARNESS_DB_ROOT/advice" ] || fail "a phase gate asked Jev"
[ ! -d "$HARNESS_DB_ROOT/advice-pending" ] || fail "a phase gate left an oracle to label"
# One history line from verify.sh; review.sh reused that passing run on the same files.
printf '%s\n' "$review_out" | grep -q '^Verification reused: ' || fail "review did not reuse the passing verify run: $review_out"
[ "$(wc -l < "$HARNESS_DB_ROOT/records/verify-history.jsonl" | tr -d ' ')" = "1" ] || fail "expected one verify history line"

# 7. Nested harness tests never emit checkpoints even when the outer shell enables them.
HARNESS_DB_ROOT="$TMP_ROOT/nested" scripts/verify.sh --project "$TARGET" >/dev/null 2>&1 || fail "nested verify failed"

# 8. The installer is idempotent and preserves unrelated hooks.
CLAUDE="$TMP_ROOT/claude-settings.json"
CODEX="$TMP_ROOT/codex-hooks.json"
printf '{"model":"x","hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"other-tool"}]}]}}\n' > "$CLAUDE"
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" >/dev/null
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" >/dev/null
python3 - "$CLAUDE" "$CODEX" <<'PY'
import json, sys
claude = json.load(open(sys.argv[1])); codex = json.load(open(sys.argv[2]))
assert claude["model"] == "x"
commands = [h["command"] for g in claude["hooks"]["SessionStart"] for h in g["hooks"]]
assert commands.count("other-tool") == 1, commands
assert sum("auto-init.sh" in c for c in commands) == 1, commands
assert not any("session-route.sh" in c for c in commands), commands
assert sum("observe_commands.py" in h["command"] for g in claude["hooks"]["PreToolUse"] for h in g["hooks"]) == 1
assert claude["hooks"]["PreToolUse"][0]["matcher"] == "Bash"
assert sum("auto-init.sh" in h["command"] for g in codex["hooks"]["SessionStart"] for h in g["hooks"]) == 1
PY
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" --uninstall >/dev/null
python3 - "$CLAUDE" "$CODEX" <<'PY'
import json, sys
claude = json.load(open(sys.argv[1])); codex = json.load(open(sys.argv[2]))
assert "PreToolUse" not in claude["hooks"]
assert [h["command"] for g in claude["hooks"]["SessionStart"] for h in g["hooks"]] == ["other-tool"]
assert codex == {"hooks": {}}, codex
PY
ls "$TMP_ROOT"/claude-settings.json.bak-jev-* >/dev/null 2>&1 || fail "installer made no backup"

printf '%s\n' 'PASS: Jev session route, quiet phase gates, hooks, and installer'
