#!/usr/bin/env sh
# Source this file in each shell that launches harness agents.
#
# Shadow by default. Task-entry routing, phase-gate checkpoints, verify and
# review predictions, session routes, and hook observations are all recorded
# and labeled, but the existing commands and deterministic gates keep running
# unchanged. Active delegation is earned, not declared: enable it only after
# `scripts/harness advise --report` shows the labeled pilot batch and the
# task-entry outcome gate documented in docs/setup.md is satisfied.

export TYPESAFE_MODEL="${TYPESAFE_MODEL:-jev-1.13.0}"
export HARNESS_JEV_CHECKPOINTS=1
export HARNESS_JEV_TIMEOUT="${HARNESS_JEV_TIMEOUT:-10}"
export HARNESS_AUDIT_ENABLED=1
export HARNESS_AUDIT_URL="${HARNESS_AUDIT_URL:-http://127.0.0.1:18080}"

# Unvalidated activation flags from earlier sessions are cleared on purpose.
if [ "${JEV_KEEP_ACTIVATION:-0}" != "1" ]; then
  unset HARNESS_TYPESAFE_ACTIVE HARNESS_TYPESAFE_OPERATOR_ACTIVATION
fi

if [ "${JEV_ENABLE_QUIET:-0}" != "1" ]; then
  printf '%s\n' 'JEV shadow: automatic checkpoints and Arc telemetry enabled; activation is earned via advise --report.' >&2
fi
