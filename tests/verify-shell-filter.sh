#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/harness-shell-filter.XXXXXX")
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM
project="$tmpdir/project"
mkdir -p "$project/scripts/__pycache__"
printf '#!/bin/sh\nexit 0\n' > "$project/scripts/harness"
printf 'def valid_python():\n    return True\n' > "$project/scripts/helper.py"
printf '\001\002\003' > "$project/scripts/__pycache__/helper.pyc"

HARNESS_DB_ROOT="$tmpdir/db" "$ROOT/scripts/verify.sh" --project "$project" > "$tmpdir/output" 2>&1 || {
  cat "$tmpdir/output"
  exit 1
}
grep -q 'PASS: bash:syntax' "$tmpdir/output"
if command -v shellcheck >/dev/null 2>&1; then
  grep -q 'PASS: bash:shellcheck' "$tmpdir/output"
fi
printf '%s\n' 'PASS: shell verification ignores Python and bytecode under scripts/'
