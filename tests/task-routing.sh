#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_routing.py -q
python3 -m unittest discover -s tests -p test_context_advice.py -q
tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/harness-route-link.XXXXXX")
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM
ln -s "$ROOT/scripts/harness" "$tmpdir/harness"
"$tmpdir/harness" route --help >/dev/null
"$tmpdir/harness" advise --help >/dev/null
printf '%s\n' 'PASS: task-entry routing, dynamic advice, and launcher'
