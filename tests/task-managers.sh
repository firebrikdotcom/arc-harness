#!/usr/bin/env sh
# Task-manager reader: Basecamp walk with pagination, the response cache, and
# the CLI contract, against tests/fake_basecamp.py on localhost. Offline.
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_managers.py -q
printf '%s\n' 'PASS: task-manager reader, cache, and CLI contract'
