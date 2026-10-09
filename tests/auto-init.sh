#!/usr/bin/env sh
# Automatic target initialisation: the registry helper, init --auto, the
# SessionStart hook, per-target harness state, and the hook installer.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-auto-init.XXXXXX")
# Physical path: the registry stores resolved roots, and /var is a symlink on macOS.
TMP_ROOT=$(CDPATH='' cd "$TMP_ROOT" && pwd -P)
trap 'rm -rf "${TMP_ROOT:?}"' EXIT HUP INT TERM
OUT="$TMP_ROOT/out.txt"

fail() {
  printf 'FAIL: %s\n' "$*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}

for tool in git make python3; do
  command -v "$tool" >/dev/null 2>&1 || { printf 'SKIP: %s is unavailable; auto-init test needs it\n' "$tool"; exit 0; }
done

# payload EVENT CWD TOOL COMMAND  Hook payload as Claude Code and Codex send it.
payload() {
  python3 -c 'import json, sys
event, cwd, tool, command = sys.argv[1:5]
data = {"hook_event_name": event, "session_id": "test", "cwd": cwd}
if tool:
    data["tool_name"] = tool
    data["tool_input"] = {"command": command}
print(json.dumps(data))' "$@"
}

count() {
  [ -d "$1" ] || { printf '0\n'; return 0; }
  find "$1" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' '
}

lines() {
  [ -f "$1" ] || { printf '0\n'; return 0; }
  wc -l < "$1" | tr -d ' '
}

export HARNESS_ROOT="$ROOT"
export HARNESS_DB_ROOT="$TMP_ROOT/db"
export HARNESS_AUTO_INIT_SYNC=1
export HARNESS_JEV_CHECKPOINTS=0
export HARNESS_AUDIT_ENABLED=0
unset HARNESS_AUTO_INIT HARNESS_AUTO_INIT_WORKER HARNESS_TARGET_ROOT HARNESS_INIT_YES || true
unset HARNESS_BUDGET_STEPS HARNESS_BUDGET_TIME_MIN HARNESS_BUDGET_LOOPS HARNESS_BUDGET_TOKENS || true

TARGET="$ROOT/scripts/harness-target.sh"
INIT="$ROOT/scripts/init.sh"
HOOK="$ROOT/scripts/hooks/auto-init.sh"
OBSERVE="$ROOT/scripts/hooks/jev-observe.sh"
CLI="$ROOT/scripts/harness"

# A main checkout with a setup target, a linked worktree of it, and a plain directory.
REPO="$TMP_ROOT/repo"
git init -q "$REPO"
printf 'init:\n\t@echo run >> RAN_SETUP\n' > "$REPO/Makefile"
git -C "$REPO" add Makefile
git -C "$REPO" -c user.name=t -c user.email=t@example.invalid commit -q -m init
WT="$TMP_ROOT/wt"
git -C "$REPO" worktree add -q "$WT" -b auto-init-test
mkdir -p "$WT/sub"
PLAIN="$TMP_ROOT/plain"
mkdir -p "$PLAIN"

# 1. Root resolution and the refusal list.
[ "$("$TARGET" root "$WT/sub")" = "$WT" ] || fail "worktree subdirectory should resolve to the worktree root"
[ "$("$TARGET" root "$PLAIN")" = "$PLAIN" ] || fail "plain directory should resolve to itself"
if "$TARGET" register "$HOME" >/dev/null 2>&1; then fail "home directory must not be registrable"; fi
if "$TARGET" register "$ROOT" >/dev/null 2>&1; then fail "harness root must not be registrable"; fi
if "$TARGET" lookup "$WT" >/dev/null 2>&1; then fail "lookup should find nothing before registration"; fi

# 2. First session start in the worktree registers it and runs the bootstrap once.
payload SessionStart "$WT/sub" "" "" | sh "$HOOK" > "$OUT" 2>&1
grep -q "^Harness auto-init: target $WT registered" "$OUT" || fail "hook did not report the registration"
grep -q 'bootstrap completed' "$OUT" || fail "hook did not report the bootstrap"
TDIR=$("$TARGET" lookup "$WT/sub") || fail "worktree is not registered"
grep -q '^TARGET_KIND=worktree$' "$TDIR/target.state" || fail "worktree kind not recorded"
[ "$(lines "$WT/RAN_SETUP")" -eq 1 ] || fail "project setup should have run exactly once"
grep -q '^STATUS=ok$' "$TDIR/bootstrap.state" || fail "bootstrap state is not ok"
[ -f "$TDIR/bootstrap.log" ] || fail "bootstrap log missing"
[ ! -d "$WT/.harness-db" ] || fail "nothing may be written inside the target"

# 3. Unchanged manifests: no second bootstrap.
payload SessionStart "$WT" "" "" | sh "$HOOK" > "$OUT" 2>&1
grep -q 'bootstrap current' "$OUT" || fail "unchanged fingerprint should skip the bootstrap"
[ "$(lines "$WT/RAN_SETUP")" -eq 1 ] || fail "bootstrap reran with an unchanged fingerprint"

# 4. A manifest change reruns it.
printf '# dependency bump\n' >> "$WT/Makefile"
payload SessionStart "$WT" "" "" | sh "$HOOK" > "$OUT" 2>&1
grep -q 'bootstrap completed' "$OUT" || fail "changed fingerprint should rerun the bootstrap"
[ "$(lines "$WT/RAN_SETUP")" -eq 2 ] || fail "bootstrap did not rerun after the manifest change"

# 5. A failing bootstrap is recorded once and not retried on the next start (make exits 2 on a recipe error).
printf 'init:\n\t@echo run >> RAN_SETUP; exit 7\n' > "$WT/Makefile"
payload SessionStart "$WT" "" "" | sh "$HOOK" > "$OUT" 2>&1
grep -q 'bootstrap FAILED (exit 2)' "$OUT" || fail "failure not reported"
grep -q '^STATUS=failed$' "$TDIR/bootstrap.state" || fail "failed state not recorded"
grep -q '^EXIT=2$' "$TDIR/bootstrap.state" || fail "exit code not recorded"
payload SessionStart "$WT" "" "" | sh "$HOOK" > "$OUT" 2>&1
grep -q 'bootstrap failed earlier (exit 2)' "$OUT" || fail "earlier failure not reported"
[ "$(lines "$WT/RAN_SETUP")" -eq 3 ] || fail "failed bootstrap was retried without a change"

# 6. Disabled: silent and no registration.
out=$(payload SessionStart "$PLAIN" "" "" | HARNESS_AUTO_INIT=0 sh "$HOOK")
[ -z "$out" ] || fail "disabled hook printed: $out"
if "$TARGET" lookup "$PLAIN" >/dev/null 2>&1; then fail "disabled hook registered a target"; fi

# 7. A plain directory without manifests is registered with nothing to run.
payload SessionStart "$PLAIN" "" "" | sh "$HOOK" > "$OUT" 2>&1
grep -q 'no project-owned setup commands' "$OUT" || fail "plain directory not handled"
PDIR=$("$TARGET" lookup "$PLAIN") || fail "plain directory not registered"
grep -q '^TARGET_KIND=dir$' "$PDIR/target.state" || fail "plain kind not recorded"

# 8. The harness root itself stays silent.
out=$(payload SessionStart "$ROOT" "" "" | sh "$HOOK")
[ -z "$out" ] || fail "harness root should print nothing: $out"

# 9. Manual init registers too.
"$INIT" --project "$PLAIN" > "$OUT" 2>&1 < /dev/null || fail "manual init failed"
grep -q '^Registered harness target: ' "$OUT" || fail "manual init did not report the registration"

# 10. The CLI, verify, and gates use the target's own database from inside the worktree.
(cd "$WT" && "$CLI" plan start) > "$OUT" 2>&1 || fail "plan start failed from the worktree"
[ -f "$TDIR/db/runs/current" ] || fail "run was not created in the target database"
[ ! -e "$HARNESS_DB_ROOT/runs/current" ] || fail "run leaked into the shared database"
(cd "$WT" && "$CLI" contract waive "fixture task" && "$CLI" plan "done" && "$CLI" build start) > "$OUT" 2>&1 || fail "plan done / build start failed"
# The fixture worktree has no checks; its database accepts an empty verification.
printf '%s\n' allow-empty > "$TDIR/db/required-checks"
"$ROOT/scripts/verify.sh" --project "$WT" > "$OUT" 2>&1 || fail "verify failed on the worktree"
[ -f "$TDIR/db/records/verify.state" ] || fail "verify record not written to the target database"
(cd "$WT/sub" && "$CLI" build "done") > "$OUT" 2>&1 || fail "build done did not accept the target's verify record"
[ ! -e "$PDIR/db/runs/current" ] || fail "the plain target must not share the worktree's run"

# 11. With checkpoints on, the chained route and the observer write to the target database.
export HARNESS_JEV_CHECKPOINTS=1
export HARNESS_TYPESAFE_ROUTER="$ROOT/tests/fake_router.py"
export FAKE_ROUTER_LOG_DIR="$TMP_ROOT/logs"
payload SessionStart "$WT" "" "" | sh "$HOOK" > "$OUT" 2>&1
[ "$(count "$TDIR/db/routes")" -ge 1 ] || fail "chained session route did not record in the target database"
python3 - "$TDIR/db/routes" "$TMP_ROOT" <<'PY' || fail "session route did not carry enum-only target history"
import json, sys
from pathlib import Path
directory, private = sys.argv[1:]
records = [json.loads(path.read_text()) for path in Path(directory).glob("*.json")]
routed = [record for record in records if record.get("source") != "deterministic"]
assert routed, records
enums = {"none", "one", "few", "many", "passed", "failed"}
for record in routed:
    history = record["history"]
    assert len(history) == 7 and set(history.values()) <= enums, history
    assert private not in json.dumps(history), history
    # Section 10 left one run in the target database with a verified build.
    assert (history["prior_runs"], history["verified_builds"], history["last_verify"]) == ("one", "one", "passed"), history
PY
before=$(count "$TDIR/db/advice")
for _ in 1 2 3; do
  payload PreToolUse "$WT/sub" Bash "make test" | sh "$OBSERVE" >/dev/null
done
[ "$(count "$TDIR/db/advice")" -eq $((before + 1)) ] || fail "observer did not record one checkpoint in the target database"
[ "$(count "$HARNESS_DB_ROOT/advice")" -eq 0 ] || fail "observer wrote to the shared database"
[ "$(count "$HARNESS_DB_ROOT/routes")" -eq 0 ] || fail "route wrote to the shared database"
export HARNESS_JEV_CHECKPOINTS=0

# 12. The installer registers auto-init on SessionStart for startup, resume, and clear.
CLAUDE="$TMP_ROOT/claude-settings.json"
CODEX="$TMP_ROOT/codex-hooks.json"
scripts/install-hooks.sh --claude "$CLAUDE" --codex "$CODEX" > /dev/null
python3 - "$CLAUDE" "$CODEX" <<'PY' || fail "installer entries are wrong"
import json, sys
for path in sys.argv[1:]:
    data = json.load(open(path))
    groups = [g for g in data["hooks"]["SessionStart"] if any("auto-init.sh" in h["command"] for h in g["hooks"])]
    assert len(groups) == 1, groups
    assert groups[0]["matcher"] == "startup|resume|clear", groups[0]
    assert not any("session-route.sh" in h["command"] for g in data["hooks"]["SessionStart"] for h in g["hooks"])
PY

printf '%s\n' 'PASS: session start registers the project, bootstraps once per fingerprint, and keeps per-target harness state'
