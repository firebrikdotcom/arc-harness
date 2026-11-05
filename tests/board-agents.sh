#!/usr/bin/env sh
# Issue-agent UI on the architecture board: Analyze, job panel updates, copy, take-over, escaping, no Fix, phone layout.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
command -v python3 >/dev/null 2>&1 || { printf '%s\n' "SKIP: python3 is unavailable; board agents DOM test needs it"; exit 0; }
if ! python3 -c 'import sys; sys.path.insert(0, "tests"); import test_board_agents as t; sys.exit(0 if t.CHROME else 1)'; then
  printf '%s\n' "SKIP: no Chrome or Chromium; set HARNESS_TEST_CHROME to run the board agents DOM test"
  exit 0
fi
status=0
log=$(python3 -m unittest discover -s tests -p test_board_agents.py -q 2>&1) || status=$?
# Headless Chrome prints display and allocator noise on stderr; keep only the test's own output.
printf '%s\n' "$log" | grep -v -e ':ERROR:' -e 'allocator multiple times' || true
[ "$status" -eq 0 ] || exit "$status"
printf '%s\n' 'PASS: board agents DOM test (desktop and phone)'
