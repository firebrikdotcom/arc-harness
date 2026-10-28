#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)

# Machine-specific settings live in the checkout's gitignored .env (see
# .env.example). Lines are read as KEY=VALUE (an `export ` prefix, surrounding
# quotes, and CRLF endings are tolerated), never executed, and a variable already
# set in the environment wins.
if [ -f "$ROOT/.env" ]; then
  cr=$(printf '\r')
  while IFS= read -r line || [ -n "$line" ]; do
    line=${line%"$cr"}
    line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/^export[[:space:]][[:space:]]*//')
    case "$line" in
      HARNESS_AUDIT_ROOT=*|HARNESS_AUDIT_LAUNCHD_LABEL=*) ;;
      *) continue ;;
    esac
    key=${line%%=*}
    value=${line#*=}
    case "$value" in
      \"*\"|\'*\') value=${value#?}; value=${value%?} ;;
    esac
    eval "current=\${$key-}"
    if [ -z "$current" ]; then
      export "$key=$value"
    fi
  done < "$ROOT/.env"
fi

SERVICE_ROOT=${HARNESS_AUDIT_ROOT:-$ROOT/services/harness-audit}
LABEL=${HARNESS_AUDIT_LAUNCHD_LABEL:-local.harness.audit}

# Escapes for each unit format, then for the sed replacement text.
sed_text() { printf '%s' "$1" | sed 's/[|&\\]/\\&/g'; }
xml_text() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"; }
# systemd expands % specifiers everywhere; ExecStart's quoted path also escapes \ and ".
unit_text() { printf '%s' "$1" | sed 's/%/%%/g'; }
unit_quoted() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g'; }

# Fill a unit template with this checkout's real locations.
render() {
  case "$1" in
    systemd)
      sed -e "s|__HARNESS_ROOT__|$(sed_text "$(unit_quoted "$ROOT")")|g" \
          -e "s|__SERVICE_ROOT__|$(sed_text "$(unit_text "$SERVICE_ROOT")")|g" \
          "$ROOT/ops/harness-audit.service"
      ;;
    launchd)
      sed -e "s|__HARNESS_ROOT__|$(sed_text "$(xml_text "$ROOT")")|g" \
          -e "s|__SERVICE_ROOT__|$(sed_text "$(xml_text "$SERVICE_ROOT")")|g" \
          -e "s|__HOME__|$(sed_text "$(xml_text "$HOME")")|g" \
          -e "s|__LABEL__|$(sed_text "$(xml_text "$LABEL")")|g" \
          "$ROOT/ops/harness-audit.launchd.plist"
      ;;
  esac
}

case "${1:-}" in
  render)
    case "${2:-}" in
      systemd|launchd) render "$2" ;;
      *) printf '%s\n' 'Usage: scripts/audit-service.sh render systemd|launchd' >&2; exit 2 ;;
    esac
    exit 0
    ;;
esac
CARGO_BIN=${CARGO:-"$HOME/.cargo/bin/cargo"}

if [ ! -x "$CARGO_BIN" ]; then
  CARGO_BIN=$(command -v cargo || true)
fi

if [ -z "$CARGO_BIN" ]; then
  printf '%s\n' 'FAIL: cargo is required to run the Arc audit service.' >&2
  exit 2
fi

if [ ! -f "$SERVICE_ROOT/Cargo.toml" ]; then
  printf '%s\n' "FAIL: Arc audit service is missing at $SERVICE_ROOT" >&2
  exit 2
fi

cd "$SERVICE_ROOT"

case "${1:-serve}" in
setup|migrate|serve)
    # The release build ingests events several times faster, so parallel agents' queues drain.
    exec "$CARGO_BIN" run --release -- "$1"
    ;;
  check|test)
    exec "$CARGO_BIN" "$1"
    ;;
  install)
    case "$(uname -s)" in
      Linux)
        command -v systemctl >/dev/null 2>&1 || { printf '%s\n' 'FAIL: systemctl is required for Linux service installation.' >&2; exit 2; }
        install -d -m 0755 "$HOME/.config/systemd/user"
        render systemd > "$HOME/.config/systemd/user/harness-audit.service"
        systemctl --user daemon-reload
        systemctl --user enable --now harness-audit.service
        ;;
      Darwin)
        command -v launchctl >/dev/null 2>&1 || { printf '%s\n' 'FAIL: launchctl is required for macOS service installation.' >&2; exit 2; }
        install -d -m 0755 "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
        # An agent running this checkout under another label would hold the same
        # port and restart forever; retire it before installing this label.
        program="<string>$(xml_text "$ROOT")/scripts/audit-service.sh</string>"
        for plist in "$HOME/Library/LaunchAgents"/*.plist; do
          [ -f "$plist" ] || continue
          old_label=$(basename "$plist" .plist)
          [ "$old_label" != "$LABEL" ] || continue
          if grep -qF "$program" "$plist"; then
            printf '%s
' "Retiring launchd agent $old_label, which runs this checkout; it is replaced by $LABEL."
            launchctl bootout "gui/$(id -u)/$old_label" 2>/dev/null || true
            rm -f "$plist"
          fi
        done
        render launchd > "$HOME/Library/LaunchAgents/$LABEL.plist"
        launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$LABEL.plist" 2>/dev/null || launchctl kickstart -k "gui/$(id -u)/$LABEL"
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
      Darwin) exec launchctl print "gui/$(id -u)/$LABEL" ;;
      *) printf '%s\n' "FAIL: unsupported operating system: $(uname -s)" >&2; exit 2 ;;
    esac
    ;;
  *)
    printf '%s\n' "Usage: scripts/audit-service.sh [setup|migrate|serve|install|status|check|test|render systemd|launchd]" >&2
    exit 2
    ;;
esac
