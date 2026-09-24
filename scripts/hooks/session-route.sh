#!/usr/bin/env sh
# SessionStart hook for Claude Code and Codex.
# When the session's working directory is a harness target, record one
# shadow Jev task-entry route built only from enum metadata derived from git
# and harness state (no prompt text is available to this hook, and none is
# sent). It prints one line of context and always exits 0.
set -u

if [ "${HARNESS_JEV_CHECKPOINTS:-0}" != "1" ]; then
  cat >/dev/null 2>&1 || :
  exit 0
fi

HOOK_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=${HARNESS_ROOT:-$(CDPATH='' cd "$HOOK_DIR/../.." && pwd -P)}
DB_ROOT=${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}

command -v python3 >/dev/null 2>&1 || exit 0
[ -f "$HARNESS_ROOT/scripts/phase_checkpoint.py" ] || exit 0

payload=$(cat 2>/dev/null || :)
cwd=$(printf '%s' "$payload" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("cwd", ""))
except Exception:
    print("")' 2>/dev/null)
[ -n "$cwd" ] || cwd=$PWD

is_target=0
if [ "$cwd" = "$HARNESS_ROOT" ]; then
  is_target=1
else
  for guide in "$cwd/AGENTS.md" "$cwd/CLAUDE.md"; do
    if [ -f "$guide" ] && grep -q 'harness-cli:start' "$guide" 2>/dev/null; then
      is_target=1
      break
    fi
  done
fi
[ "$is_target" -eq 1 ] || exit 0

python3 "$HARNESS_ROOT/scripts/phase_checkpoint.py" session-start --project "$cwd" --db-root "$DB_ROOT" 2>/dev/null || :
exit 0
