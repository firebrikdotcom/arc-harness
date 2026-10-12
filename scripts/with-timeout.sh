#!/usr/bin/env sh
# with-timeout.sh SECONDS LABEL VAR CMD [ARG...]
# with-timeout.sh --check
#
# Runs CMD in its own process group and stops it, with every child process
# still in that group, when it runs longer than SECONDS: TERM first, then KILL
# after HARNESS_TIMEOUT_GRACE seconds (default 5). A timed-out command exits
# 124 and prints one TIMEOUT: line naming LABEL, the limit, and VAR, the
# variable that raises it. Otherwise CMD's own exit status is returned.
#
# python3 supervises the group when available; GNU timeout (or gtimeout) is
# the fallback. With neither, CMD is not run: the wrapper exits 125.
# --check exits 0 when a supervisor is available and 125 when not.
set -u

info() {
  printf '%s\n' "$*"
}

is_positive_int() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$1" -gt 0 ] 2>/dev/null
}

# supervisor  Prints python3, or the GNU timeout command to use, or nothing.
# Other timeout builds (busybox, for one) signal only the command, not its
# process group, so they are not used.
supervisor() {
  if command -v python3 >/dev/null 2>&1; then
    printf '%s\n' python3
    return
  fi
  for _tool in timeout gtimeout; do
    if command -v "$_tool" >/dev/null 2>&1 && "$_tool" --version 2>/dev/null | head -n 1 | grep -q 'GNU coreutils'; then
      printf '%s\n' "$_tool"
      return
    fi
  done
}

SUPERVISOR=$(supervisor)

if [ "$#" -eq 1 ] && [ "$1" = "--check" ]; then
  [ -n "$SUPERVISOR" ] && exit 0
  exit 125
fi

if [ "$#" -lt 4 ]; then
  info "Usage: scripts/with-timeout.sh SECONDS LABEL VAR CMD [ARG...] | --check" >&2
  exit 2
fi

LIMIT=$1
LABEL=$2
LIMIT_VAR=$3
shift 3
GRACE=${HARNESS_TIMEOUT_GRACE:-5}

if ! is_positive_int "$LIMIT"; then
  info "FAIL: $LIMIT_VAR must be a whole number of seconds greater than 0, got '$LIMIT'." >&2
  exit 2
fi
if ! is_positive_int "$GRACE"; then
  info "FAIL: HARNESS_TIMEOUT_GRACE must be a whole number of seconds greater than 0, got '$GRACE'." >&2
  exit 2
fi

if [ -z "$SUPERVISOR" ]; then
  info "FAIL: $LABEL cannot be time-limited: no timeout supervisor (python3, GNU timeout, or gtimeout) is installed. Install one and rerun. Nothing was run." >&2
  exit 125
fi

MESSAGE="TIMEOUT: $LABEL did not finish within ${LIMIT}s; it and its child processes were stopped. Set $LIMIT_VAR to allow more time."

if [ "$SUPERVISOR" = python3 ]; then
  # The supervisor forwards INT, TERM, and HUP to the group so an interrupted
  # bootstrap or verification does not leave the command running.
  exec python3 -c '
import os, signal, subprocess, sys

limit, grace, message = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
command = sys.argv[4:]
try:
    child = subprocess.Popen(command, preexec_fn=os.setpgrp)
except OSError as error:
    sys.stderr.write("%s: %s\n" % (command[0], error.strerror))
    sys.exit(127)

def signal_group(sig):
    try:
        os.killpg(child.pid, sig)
    except (ProcessLookupError, PermissionError):
        pass

for forwarded in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
    signal.signal(forwarded, lambda sig, _frame: signal_group(sig))

try:
    status = child.wait(timeout=limit)
except subprocess.TimeoutExpired:
    signal_group(signal.SIGTERM)
    try:
        child.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    # KILL whatever is left in the group, including children that outlived
    # the command or ignored TERM.
    signal_group(signal.SIGKILL)
    child.wait()
    sys.stderr.write(message + "\n")
    sys.exit(124)
sys.exit(status if status >= 0 else 128 - status)
' "$LIMIT" "$GRACE" "$MESSAGE" "$@"
fi

# GNU timeout signals its whole process group on expiry.
started=$(date +%s)
status=0
"$SUPERVISOR" -k "$GRACE" "$LIMIT" "$@" || status=$?
# 124 after TERM, 137 when KILL was needed; the elapsed time tells a timeout
# apart from a command that exits with one of those codes itself.
if { [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; } && [ $(( $(date +%s) - started )) -ge "$LIMIT" ]; then
  info "$MESSAGE" >&2
  exit 124
fi
exit "$status"
