#!/usr/bin/env sh
set -eu
ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
cd "$ROOT"
python3 -m unittest discover -s tests -p test_workflow_audit.py -q
python3 -m unittest discover -s tests -p test_audit_emit.py -q
printf '%s\n' 'PASS: independent audit controls, Workflow correlation, and durable delivery'
