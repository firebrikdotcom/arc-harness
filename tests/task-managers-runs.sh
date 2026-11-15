#!/usr/bin/env sh
# Task-manager delegated runs: the state machine, its gates, the draft pull
# request and the never-merge rule, against tests/fake_agent.py (dispatch), a
# fake gh and a temporary git repository with a bare origin. Offline.
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_managers_runs.py -q
printf '%s\n' 'PASS: task-manager delegated runs, gates, and draft pull request'
