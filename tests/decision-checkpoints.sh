#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
python3 "$ROOT/tests/test_decision_checkpoints.py"
