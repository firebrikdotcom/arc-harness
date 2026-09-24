#!/usr/bin/env sh
# PreToolUse hook (Bash) for Claude Code and Codex.
# Counts identical shell commands within the active harness run and, on the
# third repeat, emits one shadow progress_assessment checkpoint. It stores
# only a checksum of the command, never the command text, and always exits 0
# so it can never block a tool call.
set -u

if [ "${HARNESS_JEV_CHECKPOINTS:-0}" != "1" ]; then
  cat >/dev/null 2>&1 || :
  exit 0
fi

HOOK_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=${HARNESS_ROOT:-$(CDPATH='' cd "$HOOK_DIR/../.." && pwd -P)}
DB_ROOT=${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}
REPEAT_THRESHOLD=${HARNESS_JEV_REPEAT_THRESHOLD:-3}

command -v python3 >/dev/null 2>&1 || exit 0
[ -f "$DB_ROOT/runs/current" ] || exit 0
run_id=$(head -n 1 "$DB_ROOT/runs/current" 2>/dev/null)
[ -n "$run_id" ] || exit 0
run_dir="$DB_ROOT/runs/$run_id"
[ -f "$run_dir/state" ] || exit 0
grep -q '^RUN_STATUS=active$' "$run_dir/state" 2>/dev/null || exit 0

payload=$(cat 2>/dev/null || :)
parsed=$(printf '%s' "$payload" | python3 -c 'import json,sys
try:
    data = json.load(sys.stdin)
except Exception:
    print("\t"); sys.exit(0)
tool = data.get("tool_name", "")
tool_input = data.get("tool_input") if isinstance(data.get("tool_input"), dict) else {}
command = str(tool_input.get("command", "")).replace("\n", " ").strip()
print(tool + "\t" + command)' 2>/dev/null)
tab=$(printf '\t')
tool_name=${parsed%%"$tab"*}
command=${parsed#*"$tab"}
[ "$tool_name" = "Bash" ] || exit 0
[ -n "$command" ] || exit 0

# Harness bookkeeping commands repeat by design.
case "$command" in
  harness\ *|*/scripts/harness\ *|scripts/harness\ *|*harness\ status*|*harness\ step*) exit 0 ;;
esac

digest=$(printf '%s' "$command" | cksum | cut -d' ' -f1)
history="$run_dir/command-digests"
printf '%s\n' "$digest" >> "$history" 2>/dev/null || exit 0
count=$(grep -c "^$digest\$" "$history" 2>/dev/null || printf '0')
[ "$count" -eq "$REPEAT_THRESHOLD" ] || exit 0

python3 "$HARNESS_ROOT/scripts/phase_checkpoint.py" tool-repeat --repeats "$count" --project "$HARNESS_ROOT" --db-root "$DB_ROOT" >/dev/null 2>&1 || :
exit 0
