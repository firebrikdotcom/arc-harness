#!/usr/bin/env sh
# Source this file in each shell that launches harness agents.

export TYPESAFE_MODEL=jev-1.13.0
export HARNESS_TYPESAFE_ACTIVE=1
export HARNESS_TYPESAFE_OPERATOR_ACTIVATION=1
export HARNESS_TYPESAFE_ROLLOUT_PERCENT=100
export HARNESS_AUDIT_ENABLED=1
export HARNESS_AUDIT_URL="${HARNESS_AUDIT_URL:-http://127.0.0.1:18080}"

if [ "${JEV_ENABLE_QUIET:-0}" != "1" ]; then
  printf '%s\n' 'JEV active: 100% eligible-task rollout; Arc telemetry enabled.' >&2
fi
