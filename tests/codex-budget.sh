#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_codex_budget.py -q
printf '%s\n' 'PASS: Codex token-budget measurement and interruption'
