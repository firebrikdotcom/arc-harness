#!/usr/bin/env sh
# Architecture board: drift check, validation, fake-agent mapping, overlay, and the session-start trigger.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
command -v git >/dev/null 2>&1 || { printf '%s\n' "SKIP: git is unavailable; board test needs it"; exit 0; }
python3 -m unittest discover -s tests -p test_board.py -q
printf '%s\n' 'PASS: architecture board check, mapper, overlay and session-start trigger'
