#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_managers_seed.py -q
printf '%s\n' 'PASS: task-manager seed packets are bounded, fenced, and kept out of the contract draft'
