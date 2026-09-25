#!/usr/bin/env sh
# Register the harness hooks in the user's Claude Code settings and Codex
# hooks file: automatic target initialisation plus the shadow Jev route at
# SessionStart, and the repeated-command observer at PreToolUse. Idempotent:
# existing harness entries are replaced, other hooks are preserved, and each
# file is backed up before it is rewritten.
#
#   scripts/install-hooks.sh [--claude PATH] [--codex PATH] [--uninstall] [--dry-run]
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
CLAUDE_SETTINGS="${HOME}/.claude/settings.json"
CODEX_HOOKS="${HOME}/.codex/hooks.json"
MODE=install
DRY_RUN=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --claude) CLAUDE_SETTINGS=$2; shift 2 ;;
    --codex) CODEX_HOOKS=$2; shift 2 ;;
    --uninstall) MODE=uninstall; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help)
      sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) printf 'FAIL: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

command -v python3 >/dev/null 2>&1 || { printf 'FAIL: python3 is required.\n' >&2; exit 2; }

python3 - "$HARNESS_ROOT" "$MODE" "$DRY_RUN" "$CLAUDE_SETTINGS" "$CODEX_HOOKS" <<'PY'
import json
import sys
import time
from pathlib import Path

root, mode, dry_run, *targets = sys.argv[1:]
dry_run = dry_run == "1"
SESSION = str(Path(root) / "scripts/hooks/auto-init.sh")
OBSERVE = str(Path(root) / "scripts/hooks/jev-observe.sh")
MARKERS = ("scripts/hooks/auto-init.sh", "scripts/hooks/session-route.sh", "scripts/hooks/jev-observe.sh")
ENTRIES = {
    "SessionStart": {"matcher": "startup|resume|clear", "hooks": [{"type": "command", "command": f'"{SESSION}"', "timeout": 30}]},
    "PreToolUse": {"matcher": "Bash", "hooks": [{"type": "command", "command": f'"{OBSERVE}"', "timeout": 15}]},
}


def is_ours(group: dict) -> bool:
    for hook in group.get("hooks", []) if isinstance(group, dict) else []:
        command = str(hook.get("command", "")) if isinstance(hook, dict) else ""
        if any(marker in command for marker in MARKERS):
            return True
    return False


for target in targets:
    path = Path(target)
    data = {}
    if path.is_file():
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            print(f"FAIL: {path} is not valid JSON ({error}); not touching it.", file=sys.stderr)
            sys.exit(1)
    if not isinstance(data, dict):
        print(f"FAIL: {path} must contain a JSON object; not touching it.", file=sys.stderr)
        sys.exit(1)
    hooks = data.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        print(f"FAIL: {path} has a non-object 'hooks' key; not touching it.", file=sys.stderr)
        sys.exit(1)
    for event, entry in ENTRIES.items():
        groups = [group for group in hooks.get(event, []) if not is_ours(group)]
        if mode == "install":
            groups.append(entry)
        if groups:
            hooks[event] = groups
        else:
            hooks.pop(event, None)
    rendered = json.dumps(data, indent=2) + "\n"
    if dry_run:
        print(f"--- {path} ({mode}, dry run)")
        print(rendered, end="")
        continue
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_file():
        backup = path.with_name(path.name + ".bak-jev-" + time.strftime("%Y%m%d%H%M%S"))
        backup.write_bytes(path.read_bytes())
        print(f"backup: {backup}")
    path.write_text(rendered, encoding="utf-8")
    print(f"{mode}ed Jev hooks in {path}")
PY
