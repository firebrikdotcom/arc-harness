#!/usr/bin/env sh
# Task-manager write-back: comment and complete send exactly one POST each,
# readers send none, and a failed write is never repeated. Against
# tests/fake_basecamp.py on localhost. Offline.
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_managers_write.py -q
printf '%s\n' 'PASS: task-manager comment and complete write-back'
