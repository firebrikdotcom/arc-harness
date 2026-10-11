#!/usr/bin/env sh
# Source this file in each shell that launches harness agents.
#
# This enables collection only. Whether Jev's answers are followed is the
# persisted switch: `scripts/harness jev on|off|status` (off: every call is a
# shadow comparison and the agent's baseline runs; on: route, launch, the
# session route, and advise follow Jev, while deterministic gates still decide
# first). Read `scripts/harness advise --report` before turning it on.

export TYPESAFE_MODEL="${TYPESAFE_MODEL:-jev-1.13.0}"
export HARNESS_JEV_CHECKPOINTS=1
export HARNESS_JEV_TIMEOUT="${HARNESS_JEV_TIMEOUT:-10}"
export HARNESS_AUDIT_ENABLED=1
export HARNESS_AUDIT_URL="${HARNESS_AUDIT_URL:-http://127.0.0.1:18080}"

# Per-shell activation flags from earlier sessions are cleared on purpose; the
# switch file, not the environment, says whether Jev is followed.
if [ "${JEV_KEEP_ACTIVATION:-0}" != "1" ]; then
  unset HARNESS_TYPESAFE_ACTIVE HARNESS_TYPESAFE_OPERATOR_ACTIVATION
fi

if [ "${JEV_ENABLE_QUIET:-0}" != "1" ]; then
  printf '%s\n' 'JEV: automatic checkpoints and Arc telemetry enabled; whether Jev is followed: harness jev status.' >&2
fi
