#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_session_runs.py -q
