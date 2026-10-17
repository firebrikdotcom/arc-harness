#!/usr/bin/env sh
# harness sites: doctor names each missing item, publish creates then updates
# with a verified receipt, and every failure mode exits with a reason. Runs
# against tests/fake_sites.py on localhost; no real Sites host is contacted.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT_UNDER_TEST=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HARNESS="$HARNESS_ROOT_UNDER_TEST/scripts/harness"
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-sites.XXXXXX")
SERVER_PID=""
cleanup() {
  [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT HUP INT TERM

OUT="$TMP_ROOT/out.txt"
HARNESS_CONFIG_HOME="$TMP_ROOT/config"
HARNESS_DB_ROOT="$TMP_ROOT/db"
HARNESS_SITES_RETRY_SECONDS=0
export HARNESS_CONFIG_HOME HARNESS_DB_ROOT HARNESS_SITES_RETRY_SECONDS
unset HARNESS_CONFIG HARNESS_AUDIT_SETTINGS FIREBRIK_SITES_URL FIREBRIK_SITES_TOKEN FIREBRIK_SITES_TOKEN_FILE || true
TOKEN_FILE="$TMP_ROOT/token"
PAGE="$TMP_ROOT/page.html"
printf '<!doctype html>\n<html><body><h1>Hello</h1><p>From an agent</p></body></html>\n' > "$PAGE"

fail() {
  printf '%s\n' "FAIL: $*"
  [ -f "$OUT" ] && { printf '%s\n' "--- last output ---"; cat "$OUT"; }
  exit 1
}
expect() { grep -q -e "$1" "$OUT" || fail "expected output to match: $1"; }
refute() { ! grep -q -e "$1" "$OUT" || fail "output must not match: $1"; }

# run EXPECTED_EXIT ARGS...
run() {
  expected=$1
  shift
  status=0
  (cd "$TMP_ROOT" && "$HARNESS" sites "$@") > "$OUT" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "harness sites $* exited $status, expected $expected"
}

start_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -f "$TMP_ROOT/port"
  python3 "$SCRIPT_DIR/fake_sites.py" "$TMP_ROOT/port" "$1" &
  SERVER_PID=$!
  tries=0
  while [ ! -s "$TMP_ROOT/port" ]; do
    tries=$((tries + 1))
    [ "$tries" -lt 100 ] || fail "fake sites server did not start"
    sleep 0.05
  done
  python3 "$HARNESS_ROOT_UNDER_TEST/scripts/harness_config.py" update --section sites \
    --values "{\"url\":\"http://127.0.0.1:$(cat "$TMP_ROOT/port")\",\"token_file\":\"$TOKEN_FILE\",\"enabled\":true}" >/dev/null
}

# Record every curl argv, then run the real curl.
mkdir -p "$TMP_ROOT/bin"
REAL_CURL=$(command -v curl)
cat > "$TMP_ROOT/bin/curl" <<SHIM
#!/bin/sh
printf '%s\\n' "\$*" >> "$TMP_ROOT/curl-argv.log"
exec "$REAL_CURL" "\$@"
SHIM
chmod +x "$TMP_ROOT/bin/curl"
PATH="$TMP_ROOT/bin:$PATH"
export PATH

start_server healthy

# No token yet: doctor names it and how to fix it; publish refuses with the same list.
run 3 doctor
expect "MISSING  no API token: $TOKEN_FILE does not exist"
expect "settings?tab=api-tokens"
expect "ok       upload endpoint .*answers"
expect "NOT READY: 1 item"
run 3 publish "$PAGE" hello
expect "Cannot publish to"
expect "no API token"

# A token readable by other users is reported, not used silently.
printf 'good-token\n' > "$TOKEN_FILE"
chmod 644 "$TOKEN_FILE"
run 3 doctor
expect "readable by other users (mode 644)"
chmod 600 "$TOKEN_FILE"

# A wrong token: the server's reason and where to get a new one.
printf 'wrong-token' > "$TOKEN_FILE"
run 3 doctor
expect "the server rejected the token: Invalid or expired API token"
printf 'good-token' > "$TOKEN_FILE"
refute "good-token"

# Ready: an HTML file with scripts is created, then updated, both checked live.
run 0 doctor
expect "ok       token accepted and allowed to publish"
expect "READY"
printf '<html><body><script>window.online = true;</script></body></html>\n' > "$PAGE"
run 0 publish "$PAGE" hello "Hello page"
expect "hello: html, 1 file(s)"
expect "OK: http://127.0.0.1:[0-9]*/testuser/hello"
expect "created, 1 file(s) match sha256"
printf '<html><body><p>Second version</p></body></html>\n' > "$PAGE"
run 0 publish "$PAGE" hello
expect "updated, 1 file(s) match sha256"

# A folder is zipped (hidden files left out) and every asset is checked live.
SITE="$TMP_ROOT/site"
mkdir -p "$SITE/css" "$SITE/.git"
printf '<html><head><link rel="stylesheet" href="css/style.css"></head><body><script src="app.js"></script></body></html>\n' > "$SITE/index.html"
printf 'document.title = "ready";\n' > "$SITE/app.js"
printf 'body { color: red; }\n' > "$SITE/css/style.css"
printf 'secret\n' > "$SITE/.git/config"
run 0 publish "$SITE" app
expect "app: folder, 3 file(s)"
expect "created, 3 file(s) match sha256"
# A zip is uploaded as it is.
(cd "$SITE" && python3 -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1], "w"); [z.write(f) for f in ("index.html", "app.js", "css/style.css")]; z.close()' "$TMP_ROOT/site.zip")
run 0 publish "$TMP_ROOT/site.zip" app
expect "app: zip, 3 file(s)"
expect "updated, 3 file(s) match sha256"
run 0 list
expect "hello"
expect "app"
refute "good-token"

# The token never appears on a command line other processes can read.
grep -q "good-token" "$TMP_ROOT/curl-argv.log" && fail "token passed on the curl command line"
grep -q "auth.header" "$TMP_ROOT/curl-argv.log" || fail "curl argv was not recorded"

# Bad input is a usage error before any network call.
run 2 publish "$TMP_ROOT/missing.html" hello
run 2 publish "$PAGE" "Bad Route"
mkdir -p "$TMP_ROOT/empty"
run 2 publish "$TMP_ROOT/empty" hello
expect "has no index.html"
printf 'notes\n' > "$TMP_ROOT/notes.txt"
run 2 publish "$TMP_ROOT/notes.txt" hello

# A server that stores something else than it received fails the publish.
start_server mangle
run 1 publish "$PAGE" hello
expect "the server's files differ from the local ones: index.html"

# A token without the publish ability is named.
start_server readonly
run 3 doctor
expect "pages:write"

# A server without the upload endpoint is called out.
start_server old
run 3 doctor
expect "has no upload endpoint"
expect "deploy Firebrik Sites 0.1.3 or newer"

# A failing server (Cloudflare's maintenance page) is named as a server fault.
start_server broken
run 3 doctor
expect "the Sites server failed (HTTP 503"
run 3 publish "$PAGE" hello
expect "check the Sites app logs"

# Publishing switched off in the harness config.
python3 "$HARNESS_ROOT_UNDER_TEST/scripts/harness_config.py" update --section sites --values '{"enabled":false}' >/dev/null
run 3 doctor
expect "turned off in the harness config"

# The skill installs for Claude Code and Codex with this checkout's absolute command path.
mkdir -p "$TMP_ROOT/home"
"$HARNESS_ROOT_UNDER_TEST/scripts/install-sites-skill.sh" --home "$TMP_ROOT/home" > "$OUT"
for agent in .claude .agents; do
  skill="$TMP_ROOT/home/$agent/skills/firebrik-sites/SKILL.md"
  [ -f "$skill" ] || fail "skill not installed for $agent"
  grep -q "^name: firebrik-sites$" "$skill" || fail "skill front matter missing for $agent"
  grep -q "$HARNESS_ROOT_UNDER_TEST/scripts/harness sites publish PATH ROUTE" "$skill" || fail "skill does not name this harness for $agent"
  ! grep -q "@HARNESS@" "$skill" || fail "unrendered placeholder in $agent skill"
done
"$HARNESS_ROOT_UNDER_TEST/scripts/install-sites-skill.sh" --home "$TMP_ROOT/home" > "$OUT"
expect "unchanged: .*/.claude/skills/firebrik-sites/SKILL.md"

printf '%s\n' "PASS: harness sites"
