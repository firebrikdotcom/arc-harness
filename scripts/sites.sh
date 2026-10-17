#!/usr/bin/env bash
# Publish and update Firebrik Sites pages from any agent, with proof.
#
#   harness sites doctor                      What publishing needs, and what is missing.
#   harness sites publish FILE ROUTE [NAME]   Create or update /{username}/ROUTE from FILE.
#   harness sites list                        Pages the token can see (id, route, url).
#
# Settings come from the `sites` section of the harness config.json
# (url, token_file, enabled); FIREBRIK_SITES_URL, FIREBRIK_SITES_TOKEN and
# FIREBRIK_SITES_TOKEN_FILE override them for one shell. The token itself is
# never stored in config.json and never printed.
#
# publish succeeds only when the server received exactly the local bytes
# (sha256 checked before anything is written), read the stored file back
# unchanged, and the live URL serves it. When it cannot publish it says what
# is missing and how to fix it.
#
# Exit codes: 0 ok, 1 publish or verification failed, 2 usage, 3 setup missing
# (doctor lists each missing item with its fix).
set -uo pipefail

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
EXIT_FAIL=1
EXIT_USAGE=2
EXIT_SETUP=3

usage() { sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; }
info() { printf '%s\n' "$*"; }
fail() { info "FAIL: $*" >&2; exit "$EXIT_FAIL"; }

missing=0
ok() { info "  ok       $1"; }
miss() {
    missing=$((missing + 1))
    info "  MISSING  $1"
    info "           fix: $2"
}

work=$(mktemp -d "${TMPDIR:-/tmp}/harness-sites.XXXXXX")
trap 'rm -rf "$work"' EXIT

# --- settings ----------------------------------------------------------------

enabled=true url="" token_file="" token=""
load_settings() {
    local config
    if config=$(python3 "$SCRIPT_DIR/harness_config.py" show 2>"$work/config.err"); then
        enabled=$(jq -r '.sites.enabled' <<<"$config")
        url=$(jq -r '.sites.url' <<<"$config")
        token_file=$(jq -r '.sites.token_file' <<<"$config")
    else
        config_error=$(tail -n1 "$work/config.err")
    fi
    url=${FIREBRIK_SITES_URL:-${url:-https://sites.firebrik.com}}
    url=${url%/}
    token_file=${FIREBRIK_SITES_TOKEN_FILE:-${token_file:-~/.config/firebrik-sites/token}}
    # shellcheck disable=SC2088 # a literal "~/" from config.json, expanded here
    case $token_file in "~/"*) token_file="$HOME/${token_file#"~/"}" ;; esac
}

# --- one MCP JSON-RPC call ----------------------------------------------------

# rpc METHOD PARAMS_FILE [AUTH]: writes $work/response.json, sets $http.
http=""
rpc() {
    local method=$1 params=$2 auth=${3:-yes}
    local -a headers=(-H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream')
    # The token goes through a 0600 header file, never the process list.
    if [[ $auth == yes ]]; then
        (umask 077 && printf 'Authorization: Bearer %s\n' "$token" >"$work/auth.header")
        headers+=(-H "@$work/auth.header")
    fi
    jq -n --arg m "$method" --slurpfile p "$params" '{jsonrpc: "2.0", id: 1, method: $m, params: $p[0]}' >"$work/request.json"
    http=$(curl -sS -m 120 -o "$work/response.json" -w '%{http_code}' -X POST "$url/mcp/sites" \
        "${headers[@]}" --data-binary "@$work/request.json" 2>"$work/curl.err") || http=000
}

body_excerpt() { tr -s '[:space:]' ' ' <"$work/response.json" | head -c 200; }

# Explain a failed call in terms of what to fix.
explain_http() {
    local error
    error=$(jq -r '.error // empty' "$work/response.json" 2>/dev/null)
    case $http in
        000) info "cannot reach $url ($(tr -d '\n' <"$work/curl.err" | head -c 160))"
             info "fix: check the network or the sites.url setting (harness config show)" ;;
        401) info "the server rejected the token: ${error:-HTTP 401}"
             info "fix: create a new token at $url/settings?tab=api-tokens and save it to $token_file" ;;
        403) info "the token's account may not do this: ${error:-HTTP 403}"
             info "fix: check the account plan or page ownership in the dashboard" ;;
        404) info "$url has no /mcp/sites endpoint"
             info "fix: point sites.url at a Firebrik Sites host" ;;
        5??) info "the Sites server failed (HTTP $http; Cloudflare shows this as 'Scheduled Maintenance')"
             info "fix: check the Sites app logs (deployment sites-app) and redeploy" ;;
        *)   info "unexpected HTTP $http: $(body_excerpt)"
             info "fix: run harness sites doctor" ;;
    esac
}

tool_error() {
    jq -e '.result.isError == true' "$work/response.json" >/dev/null 2>&1 &&
        jq -r '[.result.content[]?.text] | join(" ")' "$work/response.json"
}

# --- doctor --------------------------------------------------------------------

# check [--quiet-ok]: prints each requirement; returns 0 when publishing can work.
check() {
    local show_ok=${1:-yes}
    pass() { [[ $show_ok == yes ]] && ok "$1"; return 0; }
    missing=0

    if [[ -n ${config_error:-} ]]; then
        miss "harness config.json is invalid: $config_error" "harness config show, then repair the named field"
    elif [[ $enabled != true ]]; then
        miss "Sites publishing is turned off in the harness config" "harness config update --section sites --values '{\"enabled\":true}'"
    else
        pass "publishing enabled for $url"
    fi

    local bin tools_ok=yes
    for bin in curl jq sha256sum python3; do
        if ! command -v "$bin" >/dev/null 2>&1; then
            miss "$bin is not installed" "install $bin with the system package manager"
            tools_ok=no
        fi
    done
    [[ $tools_ok == yes ]] && pass "curl, jq, sha256sum and python3 installed"
    [[ $tools_ok == yes ]] || return 1

    token=${FIREBRIK_SITES_TOKEN:-}
    if [[ -n $token ]]; then
        pass "token from FIREBRIK_SITES_TOKEN"
    elif [[ ! -e $token_file ]]; then
        miss "no API token: $token_file does not exist" \
            "create a token at $url/settings?tab=api-tokens, then run in your own terminal: mkdir -p \"$(dirname "$token_file")\" && read -rs t && printf %s \"\$t\" > \"$token_file\" && chmod 600 \"$token_file\""
    elif [[ ! -r $token_file ]]; then
        miss "$token_file is not readable" "chmod 600 \"$token_file\" (owned by $(id -un))"
    else
        token=$(tr -d '[:space:]' <"$token_file")
        if [[ -z $token ]]; then
            miss "$token_file is empty" "paste a token from $url/settings?tab=api-tokens into it"
        else
            local mode
            mode=$(stat -c '%a' "$token_file" 2>/dev/null || stat -f '%Lp' "$token_file")
            if [[ $mode != 600 && $mode != 400 ]]; then
                miss "$token_file is readable by other users (mode $mode)" "chmod 600 \"$token_file\""
            else
                pass "token file $token_file"
            fi
        fi
    fi

    # The endpoint, without a token: a healthy server answers 401 JSON.
    echo '{}' >"$work/empty.json"
    rpc tools/list "$work/empty.json" no
    if [[ $http == 401 ]] && jq -e '.error' "$work/response.json" >/dev/null 2>&1; then
        pass "endpoint $url/mcp/sites answers"
    else
        local why
        why=$(explain_http)
        miss "endpoint $url/mcp/sites is not healthy: $(head -n1 <<<"$why")" "$(sed -n '2s/^fix: //p' <<<"$why")"
        return 1
    fi

    [[ -n $token ]] || return 1

    rpc tools/list "$work/empty.json"
    if [[ $http != 200 ]]; then
        local why
        why=$(explain_http)
        miss "$(head -n1 <<<"$why")" "$(sed -n '2s/^fix: //p' <<<"$why")"
        return 1
    fi
    pass "token accepted"

    if jq -e '.result.tools[]? | select(.name == "create-page") | .inputSchema.properties.expected_sha256' "$work/response.json" >/dev/null 2>&1; then
        pass "server verifies uploads (expected_sha256 supported)"
    else
        miss "the server does not verify uploads, so a cut-off page could look published" \
            "deploy Firebrik Sites 0.1.2 or newer (scripts/deploy.sh in the sites repository)"
    fi
    ((missing == 0))
}

cmd_doctor() {
    [[ $# -eq 0 ]] || { usage; exit "$EXIT_USAGE"; }
    info "Firebrik Sites publishing: $url"
    if check yes; then
        info "READY: harness sites publish FILE ROUTE [NAME]"
        return 0
    fi
    info "NOT READY: $missing item(s) missing"
    exit "$EXIT_SETUP"
}

preflight() {
    check no >"$work/check.out" && return 0
    info "Cannot publish to $url yet:" >&2
    cat "$work/check.out" >&2
    exit "$EXIT_SETUP"
}

# --- list ------------------------------------------------------------------------

# Writes every page to $work/pages.json as one array.
fetch_pages() {
    local page=1 last
    echo '[]' >"$work/pages.json"
    while :; do
        jq -n --argjson page "$page" '{name: "list-pages", arguments: {page: $page, per_page: 50}}' >"$work/params.json"
        rpc tools/call "$work/params.json"
        [[ $http == 200 ]] || { explain_http >&2; exit "$EXIT_FAIL"; }
        local error
        if error=$(tool_error); then fail "list-pages: $error"; fi
        jq -s '.[0] + (.[1].result.structuredContent.pages // [])' "$work/pages.json" "$work/response.json" >"$work/merged.json"
        mv "$work/merged.json" "$work/pages.json"
        last=$(jq -r '.result.structuredContent.last_page // 1' "$work/response.json")
        ((page >= last)) && break
        page=$((page + 1))
    done
}

cmd_list() {
    [[ $# -eq 0 ]] || { usage; exit "$EXIT_USAGE"; }
    preflight
    fetch_pages
    jq -r '.[] | "\(.id)\t\(.route_path)\t\(.url // "")"' "$work/pages.json"
}

# --- publish -----------------------------------------------------------------------

cmd_publish() {
    [[ $# -ge 2 && $# -le 3 ]] || { usage; exit "$EXIT_USAGE"; }
    local file=$1 route=$2 name=${3:-$2}
    [[ -f $file ]] || { info "FAIL: no such file: $file" >&2; exit "$EXIT_USAGE"; }
    [[ $route =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || { info "FAIL: ROUTE must be lowercase letters, digits and dashes: $route" >&2; exit "$EXIT_USAGE"; }
    preflight

    local bytes sha page_id action
    bytes=$(wc -c <"$file" | tr -d ' ')
    sha=$(sha256sum "$file" | cut -d' ' -f1)
    fetch_pages
    page_id=$(jq -r --arg r "$route" '[.[] | select(.route_path == $r)][0].id // empty' "$work/pages.json")

    if [[ -n $page_id ]]; then
        action=update-page
        jq -n --argjson id "$page_id" --arg name "$name" --rawfile html "$file" --argjson bytes "$bytes" --arg sha "$sha" \
            '{name: "update-page", arguments: {page_id: $id, name: $name, html_content: $html, expected_bytes: $bytes, expected_sha256: $sha}}' >"$work/params.json"
    else
        action=create-page
        jq -n --arg name "$name" --arg route "$route" --rawfile html "$file" --argjson bytes "$bytes" --arg sha "$sha" \
            '{name: "create-page", arguments: {name: $name, route_path: $route, html_content: $html, expected_bytes: $bytes, expected_sha256: $sha}}' >"$work/params.json"
    fi

    info "→ $action $route ($bytes bytes, sha256 $sha)"
    rpc tools/call "$work/params.json"
    [[ $http == 200 ]] || { explain_http >&2; exit "$EXIT_FAIL"; }
    local error
    if error=$(tool_error); then fail "$action: $error"; fi
    jq '.result.structuredContent' "$work/response.json" >"$work/receipt.json"

    local received
    received=$(jq -r '.received_sha256 // empty' "$work/receipt.json")
    [[ -n $received ]] || fail "$action: the server sent no integrity receipt; deploy Firebrik Sites 0.1.2 or newer"
    [[ $received == "$sha" ]] || fail "the server received sha256 $received but the local file is $sha"
    jq -e '.verified == true' "$work/receipt.json" >/dev/null || fail "the server could not verify the stored file"
    jq -r '.warnings[]?' "$work/receipt.json" | sed 's/^/WARN: /' >&2

    # Check the page on the host we published to, whatever APP_URL the server reports.
    local live path tail status attempt
    live=$(jq -r '.url' "$work/receipt.json")
    path=${live#*://}
    live="$url/${path#*/}"
    tail=$(jq -r '.tail_excerpt' "$work/receipt.json")
    for attempt in 1 2 3 4 5; do
        status=$(curl -sS -m 30 -o "$work/live.html" -w '%{http_code}' "$live" 2>/dev/null) || status=000
        if [[ $status == 200 ]] && jq -e -Rs --arg t "$tail" 'contains($t)' "$work/live.html" >/dev/null; then
            info "OK: $live"
            info "    $action, stored $(jq -r .stored_bytes "$work/receipt.json") bytes, sha256 $(jq -r .stored_sha256 "$work/receipt.json"), page id $(jq -r .id "$work/receipt.json")"
            return 0
        fi
        sleep "${HARNESS_SITES_RETRY_SECONDS:-$((attempt * 2))}"
    done
    fail "$live answered HTTP $status and did not serve the stored content after 5 attempts"
}

# --- main ----------------------------------------------------------------------------

[[ $# -ge 1 ]] || { usage; exit "$EXIT_USAGE"; }
sub=$1
shift
case $sub in
    -h|--help|help) usage; exit 0 ;;
    doctor|publish|list) ;;
    *) info "FAIL: unknown sites command: $sub" >&2; usage; exit "$EXIT_USAGE" ;;
esac
for bin in jq python3; do
    command -v "$bin" >/dev/null 2>&1 || { info "Cannot publish: $bin is not installed. fix: install $bin" >&2; exit "$EXIT_SETUP"; }
done
load_settings
"cmd_$sub" "$@"
