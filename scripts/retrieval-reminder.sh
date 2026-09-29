#!/usr/bin/env sh
# PreToolUse hook (Grep, Glob) for Claude Code.
# On the first Grep or Glob of a session inside a registered harness target
# whose current run has no scripts/jg.sh retrieval record, it adds one line of
# context suggesting a semantic retrieval first. It stays silent for targets
# carrying a .harness-no-upload marker, stores only a checksum of the session
# id, never blocks, and always exits 0.
set -u

if [ "${HARNESS_JEV_CHECKPOINTS:-0}" != "1" ]; then
  cat >/dev/null 2>&1 || :
  exit 0
fi

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=${HARNESS_ROOT:-$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)}
DB_ROOT=${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}

command -v python3 >/dev/null 2>&1 || exit 0

payload=$(cat 2>/dev/null || :)
parsed=$(printf '%s' "$payload" | python3 -c 'import json,sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("\t\t"); sys.exit(0)
clean = lambda value: str(value or "").replace("\t", " ").replace("\n", " ")
print(clean(data.get("tool_name")) + "\t" + clean(data.get("cwd")) + "\t" + clean(data.get("session_id")))' 2>/dev/null)
tab=$(printf '\t')
tool_name=${parsed%%"$tab"*}
rest=${parsed#*"$tab"}
cwd=${rest%%"$tab"*}
session=${rest#*"$tab"}
case "$tool_name" in
  Grep|Glob) ;;
  *) exit 0 ;;
esac
[ -n "$cwd" ] && [ -d "$cwd" ] || exit 0
cwd=$(CDPATH='' cd "$cwd" && pwd -P) || exit 0

# Only registered targets (scripts/harness-target.sh) and the harness root itself.
PROJECT_ROOT=""
if [ -x "$HARNESS_ROOT/scripts/harness-target.sh" ]; then
  target_dir=$(HARNESS_DB_ROOT=$DB_ROOT "$HARNESS_ROOT/scripts/harness-target.sh" lookup "$cwd" 2>/dev/null || :)
  if [ -n "$target_dir" ] && [ -f "$target_dir/target.state" ]; then
    DB_ROOT=$target_dir/db
    PROJECT_ROOT=$(sed -n 's/^TARGET_ROOT=//p' "$target_dir/target.state" | head -n 1)
  fi
fi
if [ -z "$PROJECT_ROOT" ]; then
  case "$cwd" in
    "$HARNESS_ROOT"|"$HARNESS_ROOT"/*) PROJECT_ROOT=$HARNESS_ROOT ;;
    *) exit 0 ;;
  esac
fi
[ -d "$PROJECT_ROOT" ] || exit 0
[ ! -e "$PROJECT_ROOT/.harness-no-upload" ] || exit 0

# Once per session: the first Grep or Glob decides, later ones stay silent.
key=$(printf '%s' "${session:-no-session}" | cksum | cut -d' ' -f1)
marks="$DB_ROOT/retrieval-reminders"
mkdir -p "$marks" 2>/dev/null || exit 0
[ ! -e "$marks/$key" ] || exit 0
# touch, not a `:` redirection: dash exits the whole shell when a redirection
# on a special built-in fails, which would turn a silent no-op into an error.
touch "$marks/$key" 2>/dev/null || exit 0

run_id=""
if [ -f "$DB_ROOT/runs/current" ]; then
  run_id=$(head -n 1 "$DB_ROOT/runs/current" 2>/dev/null || :)
  # A budget pause keeps the same live run; jg.sh records its id either way.
  if [ -n "$run_id" ] && ! grep -Eq '^RUN_STATUS=(active|paused)$' "$DB_ROOT/runs/$run_id/state" 2>/dev/null; then
    run_id=""
  fi
fi
if [ -n "$run_id" ] && [ -d "$DB_ROOT/retrieval" ]; then
  for record in "$DB_ROOT/retrieval"/*.state; do
    [ -f "$record" ] || continue
    if grep -q '^RECORD_KIND=jevgrep$' "$record" 2>/dev/null && grep -qxF "RUN_ID=$run_id" "$record" 2>/dev/null; then
      exit 0
    fi
  done
fi

python3 - "$HARNESS_ROOT/scripts/jg.sh" "$PROJECT_ROOT" <<'PY' 2>/dev/null || :
import json, shlex, sys
wrapper, project = sys.argv[1:]
line = (f"Harness: no semantic retrieval yet in this run. For an unfamiliar question, run one first: "
        f"{shlex.quote(wrapper)} --project {shlex.quote(project)} \"question\" (shown once per session).")
print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": line}}))
PY
exit 0
