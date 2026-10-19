#!/usr/bin/env sh
# SessionStart hook for Claude Code and Codex.
# Initialises the project the session starts in, whatever it is: a git
# worktree, a plain checkout, or a directory. It registers the project as a
# harness target in the machine-local database and runs the harness bootstrap
# in automatic mode (dependency install once per lockfile fingerprint, in the
# background). It then hands the same payload to session-route.sh so the shadow
# task-entry route sees the registration. Prints at most a few context lines
# and always exits 0. Disable with HARNESS_AUTO_INIT=0.
set -u

HOOK_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=${HARNESS_ROOT:-$(CDPATH='' cd "$HOOK_DIR/../.." && pwd -P)}

payload=$(cat 2>/dev/null || :)

if [ "${HARNESS_AUTO_INIT:-1}" != "1" ]; then
  exit 0
fi

cwd=""
if command -v python3 >/dev/null 2>&1; then
  cwd=$(printf '%s' "$payload" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("cwd", ""))
except Exception:
    print("")' 2>/dev/null)
fi
[ -n "$cwd" ] || cwd=$PWD

if [ -x "$HARNESS_ROOT/scripts/init.sh" ] && [ -d "$cwd" ]; then
  "$HARNESS_ROOT/scripts/init.sh" --project "$cwd" --auto 2>/dev/null | grep '^Harness auto-init:' || :
fi

# Resume from durable state, not from memory: the run, its contract, any pause,
# the last steps, and the project map.
if [ -x "$HARNESS_ROOT/scripts/harness" ] && [ -d "$cwd" ]; then
  (cd "$cwd" && "$HARNESS_ROOT/scripts/harness" brief 2>/dev/null | head -n 20) || :
fi

# Architecture board: a detached check maps the project in the background when
# it has no board or anything changed since the last mapping. The session never
# waits, the check writes only to the harness DB, and a mapper session (which
# sets HARNESS_BOARD_MAPPER=1 and HARNESS_AUTO_INIT=0) never starts another.
# Disable with HARNESS_BOARD=0.
if [ "${HARNESS_BOARD:-1}" = "1" ] && [ "${HARNESS_BOARD_MAPPER:-0}" != "1" ] \
  && [ -f "$HARNESS_ROOT/scripts/board.py" ] && [ -d "$cwd" ] && command -v python3 >/dev/null 2>&1; then
  _board_db=${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}
  if [ -d "$_board_db/targets" ]; then
    (cd / && nohup python3 "$HARNESS_ROOT/scripts/board.py" --db-root "$_board_db" check --target "$cwd" --spawn --quiet \
      </dev/null >/dev/null 2>&1 &) || :
  fi
fi

if [ -f "$HOOK_DIR/session-route.sh" ]; then
  printf '%s' "$payload" | sh "$HOOK_DIR/session-route.sh" || :
fi
exit 0
