#!/usr/bin/env sh
# Idle session logs lose base64 images, and the installer registers the sweep for both agents.
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-slim.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
cd "$ROOT"
python3 -m unittest discover -s tests -p test_slim_session_logs.py -q
scripts/install-hooks.sh --dry-run --claude "$TMP_ROOT/claude.json" --codex "$TMP_ROOT/codex.json" --root-file "$TMP_ROOT/root" > "$TMP_ROOT/out.txt"
python3 - "$TMP_ROOT/out.txt" <<'PY'
import json, re, sys
text = open(sys.argv[1]).read()
files = [json.loads(block) for block in re.findall(r"^--- .*?\.json .*?\n(\{.*?\n\})\n", text, re.S | re.M)]
assert len(files) == 2, "expected Claude Code and Codex hook files"
for data in files:
    for event in ("SessionStart", "SessionEnd"):
        commands = [h["command"] for g in data["hooks"][event] for h in g["hooks"]]
        assert any("slim_session_logs.py\" --spawn" in c for c in commands), (event, commands)
PY
printf '%s\n' 'PASS: idle session logs drop base64 images; busy, recent and older logs stay; hooks registered'
