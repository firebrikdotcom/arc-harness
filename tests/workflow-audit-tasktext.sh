#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_workflow_audit_tasktext.py -q
printf '%s\n' 'PASS: task-manager text is withheld from Workflow audit events'
