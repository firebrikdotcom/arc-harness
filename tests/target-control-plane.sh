#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/target-control-plane.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
mkdir -p "$TMP_ROOT/workspace/harness/scripts" "$TMP_ROOT/workspace/project/src"
cp "$ROOT/scripts/harness-target.sh" "$TMP_ROOT/workspace/harness/scripts/"
TARGET="$TMP_ROOT/workspace/harness/scripts/harness-target.sh"
unset HARNESS_DB_ROOT
"$TARGET" register "$TMP_ROOT/workspace" >/dev/null
for path in "$TMP_ROOT/workspace/harness" "$TMP_ROOT/workspace/harness/scripts"; do
  if "$TARGET" lookup "$path"; then
    printf '%s\n' 'FAIL: control plane inherited its registered ancestor' >&2
    exit 1
  fi
done
parent=$("$TARGET" lookup "$TMP_ROOT/workspace/project/src")
"$TARGET" register "$TMP_ROOT/workspace/project" >/dev/null
project=$("$TARGET" lookup "$TMP_ROOT/workspace/project/src")
[ "$parent" != "$project" ]
[ "$("$TARGET" db-root "$TMP_ROOT/workspace/project")" = "$project/db" ]
printf '%s\n' 'PASS: control plane keeps its base database while targets use their nearest registered ancestor'
