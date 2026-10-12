#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
if [ "${HARNESS_TYPESAFE_LIVE:-}" != "1" ]; then
  printf '%s\n' 'SKIP: set HARNESS_TYPESAFE_LIVE=1 to run live TypeSafe dynamic-advice tests'
  exit 0
fi
exec python3 "$ROOT/tests/live_context_advice.py"
