#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
python3 "$ROOT/services/harness-updater/tests/test_hooks.py"
