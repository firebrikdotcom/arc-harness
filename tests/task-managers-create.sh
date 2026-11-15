#!/usr/bin/env sh
# Task-manager provider primitives: list-create, todo-create and todo-update send
# one write each and keep untouched fields; get --full; open-walk; me; check's
# repo_paths. Against tests/fake_basecamp.py on localhost. Offline.
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_managers_create.py -q
printf '%s\n' 'PASS: task-manager create, update, full description, open walk and me'
