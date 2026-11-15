#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_task_managers_auth.py -q
printf '%s\n' 'PASS: task-manager login and refresh rotate the token atomically, on loopback, without leaking a secret'
