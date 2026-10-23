#!/usr/bin/env sh
set -eu

# harness-signals: pulls errors from the observability tools of the projects this
# machine is configured to track (HARNESS_CONFIG_HOME/signals.json) and writes the
# board's heat layer. See docs/observability-heatmap.md.
#
#   scripts/signals-service.sh serve|once|check|prune   run the service (cargo builds it first)
#   scripts/signals-service.sh login nightwatch          one-time browser sign-in for polling Nightwatch
#   scripts/signals-service.sh tools nightwatch          list Nightwatch's MCP tools (diagnostics)
#   scripts/signals-service.sh install|status     install or inspect the background service
#   scripts/signals-service.sh render systemd|launchd

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
SERVICE_ROOT=${HARNESS_SIGNALS_ROOT:-$ROOT/services/harness-signals}
LABEL=${HARNESS_SIGNALS_LAUNCHD_LABEL:-local.harness.signals}

# Escapes for each unit format, then for the sed replacement text.
sed_text() { printf '%s' "$1" | sed 's/[|&\\]/\\&/g'; }
xml_text() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"; }
unit_text() { printf '%s' "$1" | sed 's/%/%%/g'; }
unit_quoted() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g'; }

render() {
  case "$1" in
    systemd)
      sed -e "s|__HARNESS_ROOT__|$(sed_text "$(unit_quoted "$ROOT")")|g" \
          -e "s|__SERVICE_ROOT__|$(sed_text "$(unit_text "$SERVICE_ROOT")")|g" \
          "$ROOT/ops/harness-signals.service"
      ;;
    launchd)
      sed -e "s|__HARNESS_ROOT__|$(sed_text "$(xml_text "$ROOT")")|g" \
          -e "s|__SERVICE_ROOT__|$(sed_text "$(xml_text "$SERVICE_ROOT")")|g" \
          -e "s|__HOME__|$(sed_text "$(xml_text "$HOME")")|g" \
          -e "s|__LABEL__|$(sed_text "$(xml_text "$LABEL")")|g" \
          "$ROOT/ops/harness-signals.launchd.plist"
      ;;
  esac
}

case "${1:-}" in
  render)
    case "${2:-}" in
      systemd|launchd) render "$2"; exit 0 ;;
      *) printf '%s\n' 'Usage: scripts/signals-service.sh render systemd|launchd' >&2; exit 2 ;;
    esac
    ;;
  install)
    case "$(uname -s)" in
      Linux)
        command -v systemctl >/dev/null 2>&1 || { printf '%s\n' 'FAIL: systemctl is required for Linux service installation.' >&2; exit 2; }
        install -d -m 0755 "$HOME/.config/systemd/user"
        render systemd > "$HOME/.config/systemd/user/harness-signals.service"
        systemctl --user daemon-reload
        systemctl --user enable --now harness-signals.service
        systemctl --user restart harness-signals.service
        ;;
      Darwin)
        command -v launchctl >/dev/null 2>&1 || { printf '%s\n' 'FAIL: launchctl is required for macOS service installation.' >&2; exit 2; }
        install -d -m 0755 "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
        render launchd > "$HOME/Library/LaunchAgents/$LABEL.plist"
        launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$LABEL.plist" 2>/dev/null || launchctl kickstart -k "gui/$(id -u)/$LABEL"
        ;;
      *) printf '%s\n' "FAIL: unsupported operating system: $(uname -s)" >&2; exit 2 ;;
    esac
    exit 0
    ;;
  status)
    case "$(uname -s)" in
      Linux) exec systemctl --user --no-pager status harness-signals.service ;;
      Darwin) exec launchctl print "gui/$(id -u)/$LABEL" ;;
      *) printf '%s\n' "FAIL: unsupported operating system: $(uname -s)" >&2; exit 2 ;;
    esac
    ;;
esac

CARGO_BIN=${CARGO:-"$HOME/.cargo/bin/cargo"}
if [ ! -x "$CARGO_BIN" ]; then
  CARGO_BIN=$(command -v cargo || true)
fi
if [ -z "$CARGO_BIN" ]; then
  printf '%s\n' 'FAIL: cargo is required to run harness-signals.' >&2
  exit 2
fi
if [ ! -f "$SERVICE_ROOT/Cargo.toml" ]; then
  printf '%s\n' "FAIL: harness-signals is missing at $SERVICE_ROOT" >&2
  exit 2
fi

[ "$#" -gt 0 ] || set -- serve
case "$1" in
  serve|once|check|prune|login|tools)
    cd "$SERVICE_ROOT"
    exec "$CARGO_BIN" run --quiet --release -- "$@"
    ;;
  *)
    printf '%s\n' 'Usage: scripts/signals-service.sh [serve|once|check|prune|login nightwatch|tools nightwatch|install|status|render systemd|launchd]' >&2
    exit 2
    ;;
esac
