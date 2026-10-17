#!/usr/bin/env bash
# Publish and update Firebrik Sites pages from any agent, with proof.
#
#   harness sites doctor                      What publishing needs, and what is missing.
#   harness sites publish PATH ROUTE [NAME]   Create or update /{username}/ROUTE from PATH:
#                                             an .html file, a .zip, or a folder with index.html.
#   harness sites list                        Pages the token can see (id, route, url).
#
# Settings come from the `sites` section of the harness config.json
# (url, token_file, enabled); FIREBRIK_SITES_URL, FIREBRIK_SITES_TOKEN and
# FIREBRIK_SITES_TOKEN_FILE override them for one shell. The token itself is
# never stored in config.json and never printed.
#
# publish uploads the file to POST URL/api/pages, which stores it as the
# dashboard upload does (scripts and assets kept). It succeeds only when the
# server received exactly the uploaded bytes, every published file's sha256
# matches the local one, and the live URL serves them. When it cannot publish
# it says what is missing and how to fix it.
#
# Exit codes: 0 ok, 1 publish or verification failed, 2 usage, 3 setup missing
# (doctor lists each missing item with its fix).
set -uo pipefail

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
EXIT_FAIL=1
EXIT_USAGE=2
EXIT_SETUP=3
NEEDS_VERSION="Firebrik Sites 0.1.3 or newer"

usage() { sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; }
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

enabled=true url="" token_file="" token="" config_error=""
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

# --- HTTP -----------------------------------------------------------------------

# request PATH [AUTH] [curl args...]: POST to URL/PATH; writes $work/response.json, sets $http.
# The token goes through a 0600 header file, never the process list.
http=""
request() {
    local path=$1 auth=$2
    shift 2
    local -a headers=(-H 'Accept: application/json, text/event-stream')
    if [[ $auth == yes ]]; then
        (umask 077 && printf 'Authorization: Bearer %s\n' "$token" >"$work/auth.header")
        headers+=(-H "@$work/auth.header")
    fi
    http=$(curl -sS -m 300 -o "$work/response.json" -w '%{http_code}' -X POST "$url/$path" \
        "${headers[@]}" "$@" 2>"$work/curl.err") || http=000
}

# rpc METHOD PARAMS_FILE: one MCP JSON-RPC call with the token.
rpc() {
    jq -n --arg m "$1" --slurpfile p "$2" '{jsonrpc: "2.0", id: 1, method: $m, params: $p[0]}' >"$work/request.json"
    request mcp/sites yes -H 'Content-Type: application/json' --data-binary "@$work/request.json"
}

body_excerpt() { tr -s '[:space:]' ' ' <"$work/response.json" | head -c 200; }

# Explain a failed call in terms of what to fix: two lines, the problem and "fix: ...".
explain_http() {
    local error
    error=$(jq -r '.error // .message // empty' "$work/response.json" 2>/dev/null)
    case $http in
        000) info "cannot reach $url ($(tr -d '\n' <"$work/curl.err" | head -c 160))"
             info "fix: check the network or the sites.url setting (harness config show)" ;;
        401) info "the server rejected the token: ${error:-HTTP 401}"
             info "fix: create a new token at $url/settings?tab=api-tokens and save it to $token_file" ;;
        403) info "the server refused: ${error:-HTTP 403}"
             info "fix: use a token with the pages:write ability, or check the account's plan limits in the dashboard" ;;
        404|405) info "$url has no upload endpoint (HTTP $http on /api/pages)"
             info "fix: deploy $NEEDS_VERSION, or point sites.url at a Firebrik Sites host" ;;
        413) info "the upload is too large for the server (HTTP 413)"
             info "fix: keep the upload under 50 MB (zip limit) and the site under 100 MB unpacked" ;;
        422) info "the server refused the upload: ${error:-$(body_excerpt)}"
             info "fix: correct the upload as the message says (route, file type, index.html)" ;;
        5??) info "the Sites server failed (HTTP $http; Cloudflare shows this as 'Scheduled Maintenance')"
             info "fix: check the Sites app logs (deployment sites-app) and redeploy" ;;
        *)   info "unexpected HTTP $http: $(body_excerpt)"
             info "fix: run harness sites doctor" ;;
    esac
}

# miss_http: record the current failed call as a missing item.
miss_http() {
    local why
    why=$(explain_http)
    miss "$(head -n1 <<<"$why")" "$(sed -n '2s/^fix: //p' <<<"$why")"
}

tool_error() {
    jq -e '.result.isError == true' "$work/response.json" >/dev/null 2>&1 &&
        jq -r '[.result.content[]?.text] | join(" ")' "$work/response.json"
}

# --- doctor --------------------------------------------------------------------

# check yes|no: prints each requirement (ok lines only with yes); 0 when publishing can work.
check() {
    local show_ok=$1
    pass() { if [[ $show_ok == yes ]]; then ok "$1"; fi; }
    missing=0

    if [[ -n $config_error ]]; then
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
    [[ $tools_ok == yes ]] || return 1
    pass "curl, jq, sha256sum and python3 installed"

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
        local mode
        mode=$(stat -c '%a' "$token_file" 2>/dev/null || stat -f '%Lp' "$token_file")
        if [[ -z $token ]]; then
            miss "$token_file is empty" "paste a token from $url/settings?tab=api-tokens into it"
        elif [[ $mode != 600 && $mode != 400 ]]; then
            miss "$token_file is readable by other users (mode $mode)" "chmod 600 \"$token_file\""
        else
            pass "token file $token_file"
        fi
    fi

    # The upload endpoint, without a token: a current, healthy server answers 401 JSON.
    request api/pages no
    if [[ $http == 401 ]] && jq -e '.error' "$work/response.json" >/dev/null 2>&1; then
        pass "upload endpoint $url/api/pages answers"
    else
        miss_http
        return 1
    fi

    [[ -n $token ]] || return 1

    # With the token and no file: 422 means the token is accepted and may publish.
    request api/pages yes
    if [[ $http == 422 ]]; then
        pass "token accepted and allowed to publish"
    else
        miss_http
    fi
    ((missing == 0))
}

cmd_doctor() {
    [[ $# -eq 0 ]] || { usage; exit "$EXIT_USAGE"; }
    info "Firebrik Sites publishing: $url"
    if check yes; then
        info "READY: harness sites publish PATH ROUTE [NAME]"
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

cmd_list() {
    [[ $# -eq 0 ]] || { usage; exit "$EXIT_USAGE"; }
    preflight
    local page=1 last error
    while :; do
        jq -n --argjson page "$page" '{name: "list-pages", arguments: {page: $page, per_page: 50}}' >"$work/params.json"
        rpc tools/call "$work/params.json"
        [[ $http == 200 ]] || { explain_http >&2; exit "$EXIT_FAIL"; }
        if error=$(tool_error); then fail "list-pages: $error"; fi
        jq -r '.result.structuredContent.pages[]? | "\(.id)\t\(.route_path)\t\(.url // "")"' "$work/response.json"
        last=$(jq -r '.result.structuredContent.last_page // 1' "$work/response.json")
        ((page >= last)) && break
        page=$((page + 1))
    done
}

# --- publish -----------------------------------------------------------------------

cmd_publish() {
    [[ $# -ge 2 && $# -le 3 ]] || { usage; exit "$EXIT_USAGE"; }
    local source=$1 route=$2 name=${3:-}
    [[ -e $source ]] || { info "FAIL: no such file or folder: $source" >&2; exit "$EXIT_USAGE"; }
    [[ $route =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || { info "FAIL: ROUTE must be lowercase letters, digits and dashes: $route" >&2; exit "$EXIT_USAGE"; }

    local upload=$source kind
    if [[ -d $source ]]; then
        [[ -f $source/index.html ]] || { info "FAIL: $source has no index.html at its root" >&2; exit "$EXIT_USAGE"; }
        upload="$work/site.zip"
        python3 "$SCRIPT_DIR/sites_manifest.py" pack "$source" "$upload" || exit "$EXIT_USAGE"
        kind=folder
    elif python3 -c 'import sys, zipfile; sys.exit(0 if zipfile.is_zipfile(sys.argv[1]) else 1)' "$source"; then
        kind=zip
    else
        case ${source,,} in
            *.html|*.htm) kind=html ;;
            *) info "FAIL: publish an .html file, a .zip, or a folder with index.html: $source" >&2; exit "$EXIT_USAGE" ;;
        esac
    fi
    preflight

    python3 "$SCRIPT_DIR/sites_manifest.py" manifest "$upload" >"$work/local.json" || fail "could not read $source"
    local sha bytes count
    sha=$(sha256sum "$upload" | cut -d' ' -f1)
    bytes=$(wc -c <"$upload" | tr -d ' ')
    count=$(jq 'length' "$work/local.json")
    info "→ $route: $kind, $count file(s), $bytes bytes uploaded, sha256 $sha"

    local -a fields=(-F "file=@$upload" -F "route_path=$route" -F "sha256=$sha")
    [[ -n $name ]] && fields+=(-F "name=$name")
    request api/pages yes "${fields[@]}"
    if [[ $http != 200 && $http != 201 ]]; then
        explain_http >&2
        exit "$EXIT_FAIL"
    fi
    cp "$work/response.json" "$work/receipt.json"

    local received
    received=$(jq -r '.received_sha256 // empty' "$work/receipt.json")
    [[ $received == "$sha" ]] || fail "the server received sha256 ${received:-none} but the upload is $sha"
    jq -e '.verified == true' "$work/receipt.json" >/dev/null || fail "the server could not verify the stored files"
    # Every local file must be published with the same sha256.
    local differ
    differ=$(jq -r --slurpfile local "$work/local.json" '
        (.files // [] | map({key: .path, value: .sha256}) | from_entries) as $stored
        | $local[0] | to_entries[] | select($stored[.key] != .value.sha256) | .key' "$work/receipt.json")
    [[ -z $differ ]] || fail "the server's files differ from the local ones: $(tr '\n' ' ' <<<"$differ")"
    jq -r '.warnings[]?' "$work/receipt.json" | sed 's/^/WARN: /' >&2

    # The live site, on the host we published to whatever APP_URL the server reports.
    local live path
    live=$(jq -r '.url' "$work/receipt.json")
    path=${live#*://}
    live="$url/${path#*/}"
    verify_live "$live" || fail "$live did not serve the published files after 5 attempts: $live_problem"
    info "OK: $live"
    info "    $(jq -r .action "$work/receipt.json"), $count file(s) match sha256, page id $(jq -r .id "$work/receipt.json")"
}

# verify_live URL: the page shows the stored index tail and every non-HTML asset
# is served byte-for-byte (HTML pages get the site's head tags injected).
live_problem=""
verify_live() {
    local live=$1 attempt status tail asset expected got
    tail=$(jq -r '.tail_excerpt' "$work/receipt.json")
    for attempt in 1 2 3 4 5; do
        live_problem=""
        status=$(curl -sS -m 30 -o "$work/live.html" -w '%{http_code}' "$live" 2>/dev/null) || status=000
        if [[ $status != 200 ]] || ! jq -e -Rs --arg t "$tail" 'contains($t)' "$work/live.html" >/dev/null; then
            live_problem="the page answered HTTP $status without the published content (a password-protected page cannot be checked)"
        else
            while IFS=$'\t' read -r asset expected; do
                status=$(curl -sS -m 30 -o "$work/asset" -w '%{http_code}' "$live/$asset" 2>/dev/null) || status=000
                got=$(sha256sum "$work/asset" | cut -d' ' -f1)
                if [[ $status != 200 || $got != "$expected" ]]; then
                    live_problem="$asset answered HTTP $status with sha256 $got"
                    break
                fi
            done < <(jq -r 'to_entries[] | select(.key | test("\\.html?$") | not) | "\(.key)\t\(.value.sha256)"' "$work/local.json" | head -n 25)
            [[ -z $live_problem ]] && return 0
        fi
        sleep "${HARNESS_SITES_RETRY_SECONDS:-$((attempt * 2))}"
    done
    return 1
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
