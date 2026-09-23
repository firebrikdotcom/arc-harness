#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
SERVICE_ROOT=${HARNESS_AUDIT_ROOT:-$ROOT/services/harness-audit}

if [ ! -f "$SERVICE_ROOT/Cargo.toml" ]; then
  printf '%s\n' "FAIL: Arc audit service is missing at $SERVICE_ROOT" >&2
  exit 2
fi

cd "$SERVICE_ROOT"

case "${1:-serve}" in
  setup|migrate|serve)
    exec cargo run -- "$1"
    ;;
  check|test)
    exec cargo "$1"
    ;;
  install)
    case "$(uname -s)" in
      Linux)
        command -v systemctl >/dev/null 2>&1 || { printf '%s\n' 'FAIL: systemctl is required for Linux service installation.' >&2; exit 2; }
        install -D -m 0644 "$ROOT/ops/harness-audit.service" "$HOME/.config/systemd/user/harness-audit.service"
        systemctl --user daemon-reload
        systemctl --user enable --now harness-audit.service
        ;;
      Darwin)
        command -v launchctl >/dev/null 2>&1 || { printf '%s\n' 'FAIL: launchctl is required for macOS service installation.' >&2; exit 2; }
        install -d -m 0755 "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
        sed "s|__HOME__|$HOME|g" "$ROOT/ops/harness-audit.launchd.plist" > "$HOME/Library/LaunchAgents/com.lotharthesavior.harness-audit.plist"
        launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.lotharthesavior.harness-audit.plist" 2>/dev/null || launchctl kickstart -k "gui/$(id -u)/com.lotharthesavior.harness-audit"
        ;;
      *)
        printf '%s\n' "FAIL: unsupported operating system: $(uname -s)" >&2
        exit 2
        ;;
    esac
    ;;
  status)
    case "$(uname -s)" in
      Linux) exec systemctl --user --no-pager status harness-audit.service ;;
      Darwin) exec launchctl print "gui/$(id -u)/com.lotharthesavior.harness-audit" ;;
      *) printf '%s\n' "FAIL: unsupported operating system: $(uname -s)" >&2; exit 2 ;;
    esac
    ;;
  *)
    printf '%s\n' "Usage: scripts/audit-service.sh [setup|migrate|serve|install|status|check|test]" >&2
    exit 2
    ;;
esac
