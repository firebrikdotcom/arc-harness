#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
python3 -m unittest discover -s "$root/tests" -p test_behavior_verification.py
