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

# Cached project snapshots are data, not source belonging to this project.
mkdir -p "$project/.harness-db/snapshots"
printf '#!/bin/sh\nif then\n' > "$project/.harness-db/snapshots/invalid.sh"

HARNESS_DB_ROOT="$tmpdir/db" "$ROOT/scripts/verify.sh" --project "$project" > "$tmpdir/output" 2>&1 || {
  cat "$tmpdir/output"
  exit 1
}
grep -q 'PASS: bash:syntax' "$tmpdir/output"
if command -v shellcheck >/dev/null 2>&1; then
  grep -q 'PASS: bash:shellcheck' "$tmpdir/output"
fi
printf '%s\n' 'PASS: shell verification ignores Python, bytecode, and local database snapshots'

# Inside a git work tree, ignored files and nested repositories are not this project's scripts.
if command -v git >/dev/null 2>&1; then
  gitproject="$tmpdir/gitproject"
  mkdir -p "$gitproject/scripts" "$gitproject/.meetings/.venv/bin" "$gitproject/nested/scripts"
  printf '#!/bin/sh\nexit 0\n' > "$gitproject/scripts/ok.sh"
  printf '#!/bin/sh\nif then\n' > "$gitproject/.meetings/.venv/bin/broken.sh"
  printf '#!/bin/sh\nif then\n' > "$gitproject/nested/scripts/broken.sh"
  printf '.meetings/\n' > "$gitproject/.gitignore"
  git -C "$gitproject" init -q
  git -C "$gitproject/nested" init -q
  HARNESS_DB_ROOT="$tmpdir/db2" "$ROOT/scripts/verify.sh" --project "$gitproject" > "$tmpdir/output2" 2>&1 || {
    cat "$tmpdir/output2"
    exit 1
  }
  grep -q 'PASS: bash:syntax' "$tmpdir/output2"
  if command -v shellcheck >/dev/null 2>&1; then
    grep -q 'PASS: bash:shellcheck' "$tmpdir/output2"
  fi
  printf '%s\n' 'PASS: shell verification ignores git-ignored files and nested repositories'
fi
