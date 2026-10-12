#!/usr/bin/env sh
set -eu

if [ "$#" -lt 2 ]; then
  printf 'FAIL: Usage: scripts/reviewer.sh PACKET FINDINGS_OUT\n' >&2
  exit 2
fi

PACKET="$1"
FINDINGS_OUT="$2"

if [ ! -f "$PACKET" ]; then
  printf 'FAIL: packet file not found: %s\n' "$PACKET" >&2
  exit 2
fi

mkdir -p "$(dirname "$FINDINGS_OUT")"

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT="${HARNESS_ROOT:-$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)}"

# 1. Runtime detection
KIND="${HARNESS_REVIEWER_KIND:-}"
if [ -n "$KIND" ]; then
  case "$KIND" in
    claude|codex) ;;
    *)
      printf 'FAIL: unknown HARNESS_REVIEWER_KIND: %s (expected claude or codex)\n' "$KIND" >&2
      exit 2
      ;;
  esac
else
  is_claude=0
  is_codex=0
  if [ -n "${CLAUDE_SESSION_ID:-}" ] || [ -n "${CLAUDE_CODE_SESSION_ID:-}" ] || [ -n "${CLAUDECODE:-}" ] || [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    is_claude=1
  fi
  if [ -n "${CODEX_THREAD_ID:-}" ]; then
    is_codex=1
  fi

  if [ "$is_claude" -eq 1 ] && [ "$is_codex" -eq 0 ]; then
    KIND=claude
  elif [ "$is_codex" -eq 1 ] && [ "$is_claude" -eq 0 ]; then
    KIND=codex
  elif [ "$is_claude" -eq 1 ] && [ "$is_codex" -eq 1 ]; then
    printf 'FAIL: ambiguous runtime: both Claude Code and Codex session variables are present; set HARNESS_REVIEWER_KIND\n' >&2
    exit 2
  else
    printf 'FAIL: cannot detect calling runtime from environment (expected Claude Code session variables or CODEX_THREAD_ID, or set HARNESS_REVIEWER_KIND)\n' >&2
    exit 2
  fi
fi

# 2. Reviewer prompt
PROMPT="Review packet: $PACKET
Read the packet and follow it. Do not edit any project file. Write only the findings JSON to $FINDINGS_OUT."

is_valid_json() {
  _file="$1"
  [ -s "$_file" ] || return 1
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json, sys; d = json.load(open(sys.argv[1])); sys.exit(0 if isinstance(d, dict) and "tree_hash" in d and d.get("verdict") in ("approve", "block") and "reviewer" in d and isinstance(d.get("findings"), list) else 1)' "$_file" >/dev/null 2>&1
  elif command -v node >/dev/null 2>&1; then
    node -e 'const fs=require("fs"); const d = JSON.parse(fs.readFileSync(process.argv[1], "utf-8")); process.exit(d && typeof d === "object" && d.tree_hash && (d.verdict === "approve" || d.verdict === "block") && d.reviewer && Array.isArray(d.findings) ? 0 : 1);' "$_file" >/dev/null 2>&1
  else
    grep -q '^[[:space:]]*{' "$_file"
  fi
}

extract_json_to_file() {
  _raw_file="$1"
  _target_file="$2"
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$_raw_file" "$_target_file" <<'PY'
import json, sys, re

with open(sys.argv[1], "r", encoding="utf-8", errors="replace") as f:
    text = f.read()

def is_findings(d):
    return (
        isinstance(d, dict)
        and "tree_hash" in d
        and d.get("verdict") in ("approve", "block")
        and "reviewer" in d
        and isinstance(d.get("findings"), list)
    )

data = None
try:
    parsed = json.loads(text.strip())
    if isinstance(parsed, dict):
        data = parsed
except Exception:
    pass

if not is_findings(data):
    for m in re.finditer(r'```(?:json)?\s*(\{.*?\})\s*```', text, re.DOTALL):
        try:
            parsed = json.loads(m.group(1))
            if is_findings(parsed):
                data = parsed
                break
            elif data is None and isinstance(parsed, dict):
                data = parsed
        except Exception:
            pass

if not is_findings(data):
    for m in re.finditer(r'(\{.*\})', text, re.DOTALL):
        try:
            parsed = json.loads(m.group(1))
            if is_findings(parsed):
                data = parsed
                break
            elif data is None and isinstance(parsed, dict):
                data = parsed
        except Exception:
            pass

if is_findings(data):
    with open(sys.argv[2], "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    sys.exit(0)
sys.exit(1)
PY
  else
    return 1
  fi
}

# 3. Headless execution (outside Herdr)
if [ "${HERDR_ENV:-0}" != "1" ]; then
  if [ "$KIND" = "claude" ]; then
    raw_out=$(mktemp "${TMPDIR:-/tmp}/claude-review-raw.XXXXXX")
    claude_status=0
    rev_sid=$(python3 -c 'import uuid; print(uuid.uuid4())' 2>/dev/null || :)

    # Build claude -p command with read-only tools
    cmd="claude -p --tools=Read"
    if [ -n "$rev_sid" ]; then
      cmd="$cmd --session-id \"$rev_sid\""
    fi
    if [ -n "${HARNESS_REVIEWER_MODEL:-}" ]; then
      cmd="$cmd --model \"$HARNESS_REVIEWER_MODEL\""
    fi
    if [ -n "${HARNESS_REVIEWER_EFFORT:-}" ]; then
      cmd="$cmd --effort \"$HARNESS_REVIEWER_EFFORT\""
    fi

    # Run claude -p in background with exemption watcher
    eval "$cmd \"\$PROMPT\"" > "$raw_out" 2>/dev/null &
    claude_pid=$!

    if [ -n "$rev_sid" ] && [ -f "$HARNESS_ROOT/scripts/workflow_audit.py" ]; then
      (
        while kill -0 "$claude_pid" 2>/dev/null; do
          show_out=$(python3 "$HARNESS_ROOT/scripts/workflow_audit.py" todo show --session-id "$rev_sid" 2>/dev/null || :)
          if printf '%s' "$show_out" | grep -q '"confirmed": false'; then
            python3 "$HARNESS_ROOT/scripts/workflow_audit.py" todo exempt --session-id "$rev_sid" --reason "Independent reviewer: packet inspection" >/dev/null 2>&1 || :
          fi
          sleep 0.2
        done
      ) &
      exempt_pid=$!
    else
      exempt_pid=""
    fi

    wait "$claude_pid" 2>/dev/null || claude_status=$?
    if [ -n "$exempt_pid" ]; then
      kill "$exempt_pid" 2>/dev/null || true
      wait "$exempt_pid" 2>/dev/null || true
    fi

    if [ "$claude_status" -ne 0 ] && [ ! -s "$raw_out" ]; then
      rm -f "$raw_out"
      printf 'FAIL: claude -p exited %s with no output\n' "$claude_status" >&2
      exit 1
    fi

    if ! extract_json_to_file "$raw_out" "$FINDINGS_OUT"; then
      rm -f "$raw_out"
      printf 'FAIL: claude did not produce valid findings JSON\n' >&2
      exit 1
    fi
    rm -f "$raw_out"
    exit 0

  elif [ "$KIND" = "codex" ]; then
    cmd="codex exec --sandbox read-only -o \"$FINDINGS_OUT\""
    if [ -n "${HARNESS_REVIEWER_MODEL:-}" ]; then
      cmd="$cmd --model \"$HARNESS_REVIEWER_MODEL\""
    fi
    if [ -n "${HARNESS_REVIEWER_EFFORT:-}" ]; then
      cmd="$cmd -c 'model_reasoning_effort=\"$HARNESS_REVIEWER_EFFORT\"'"
    fi

    codex_status=0
    eval "$cmd \"\$PROMPT\"" >/dev/null 2>&1 &
    codex_pid=$!

    if [ -f "$HARNESS_ROOT/scripts/workflow_audit.py" ]; then
      (
        while kill -0 "$codex_pid" 2>/dev/null; do
          # shellcheck disable=SC2010
          rollout=$(ls -l "/proc/$codex_pid/fd" 2>/dev/null | grep -o '/.*rollout.*\.jsonl' | head -n 1 || :)
          if [ -n "$rollout" ]; then
            sid=$(basename "$rollout" .jsonl | sed -n 's/.*-\([0-9a-f-]\{36\}\)$/\1/p')
            if [ -n "$sid" ]; then
              show_out=$(python3 "$HARNESS_ROOT/scripts/workflow_audit.py" todo show --session-id "$sid" 2>/dev/null || :)
              if printf '%s' "$show_out" | grep -q '"confirmed": false'; then
                python3 "$HARNESS_ROOT/scripts/workflow_audit.py" todo exempt --session-id "$sid" --reason "Independent reviewer: packet inspection" >/dev/null 2>&1 || :
              fi
            fi
          fi
          sleep 0.2
        done
      ) &
      exempt_pid=$!
    else
      exempt_pid=""
    fi

    wait "$codex_pid" 2>/dev/null || codex_status=$?
    if [ -n "$exempt_pid" ]; then
      kill "$exempt_pid" 2>/dev/null || true
      wait "$exempt_pid" 2>/dev/null || true
    fi

    if [ "$codex_status" -ne 0 ] || [ ! -s "$FINDINGS_OUT" ]; then
      printf 'FAIL: codex exec failed or produced no output (exit %s)\n' "$codex_status" >&2
      exit 1
    fi

    raw_tmp=$(mktemp "${TMPDIR:-/tmp}/codex-review-raw.XXXXXX")
    cp "$FINDINGS_OUT" "$raw_tmp"
    if ! extract_json_to_file "$raw_tmp" "$FINDINGS_OUT"; then
      rm -f "$raw_tmp"
      printf 'FAIL: codex did not produce valid findings JSON\n' >&2
      exit 1
    fi
    rm -f "$raw_tmp"
    exit 0
  fi
fi

# 4. Herdr execution (HERDR_ENV = 1)
current_json=$(herdr pane current --current 2>/dev/null || :)
[ -n "$current_json" ] || { printf 'FAIL: herdr pane current --current failed\n' >&2; exit 1; }

WORKSPACE_ID=$(printf '%s' "$current_json" | python3 -c 'import json, sys; print(json.load(sys.stdin).get("result", {}).get("pane", {}).get("workspace_id", ""))' 2>/dev/null || :)
CALLER_TAB_ID=$(printf '%s' "$current_json" | python3 -c 'import json, sys; print(json.load(sys.stdin).get("result", {}).get("pane", {}).get("tab_id", ""))' 2>/dev/null || :)
# shellcheck disable=SC2034
CALLER_PANE_ID=$(printf '%s' "$current_json" | python3 -c 'import json, sys; print(json.load(sys.stdin).get("result", {}).get("pane", {}).get("pane_id", ""))' 2>/dev/null || :)

[ -n "$WORKSPACE_ID" ] && [ -n "$CALLER_TAB_ID" ] || { printf 'FAIL: could not resolve calling workspace or tab from herdr pane current\n' >&2; exit 1; }

tab_json=$(herdr tab get "$CALLER_TAB_ID" 2>/dev/null || :)
CALLER_TAB_LABEL=$(printf '%s' "$tab_json" | python3 -c 'import json, sys; print(json.load(sys.stdin).get("result", {}).get("tab", {}).get("label", ""))' 2>/dev/null || :)
[ -n "$CALLER_TAB_LABEL" ] || CALLER_TAB_LABEL="tab"

tab_list_json=$(herdr tab list --workspace "$WORKSPACE_ID" 2>/dev/null || :)
existing_labels=$(printf '%s' "$tab_list_json" | python3 -c 'import json, sys; tabs = json.load(sys.stdin).get("result", {}).get("tabs", []); print("\n".join(t.get("label", "") for t in tabs))' 2>/dev/null || :)

# Label selection: <caller tab label>-reviewer, or -reviewer-2, -reviewer-3, ...
base_label="${CALLER_TAB_LABEL}-reviewer"
cand_label="$base_label"
suffix=2
while printf '%s\n' "$existing_labels" | grep -F -x -q "$cand_label"; do
  cand_label="${base_label}-${suffix}"
  suffix=$((suffix + 1))
done

PROJECT_ROOT="${HARNESS_TARGET_ROOT:-$PWD}"
PROJECT_ROOT=$(cd "$PROJECT_ROOT" && pwd -P)

new_tab_json=$(herdr tab create --workspace "$WORKSPACE_ID" --cwd "$PROJECT_ROOT" --label "$cand_label" --no-focus 2>/dev/null || :)
NEW_TAB_ID=$(printf '%s' "$new_tab_json" | python3 -c 'import json, sys; data = json.load(sys.stdin).get("result", {}); print(data.get("tab", {}).get("tab_id", ""))' 2>/dev/null || :)
NEW_PANE_ID=$(printf '%s' "$new_tab_json" | python3 -c 'import json, sys; data = json.load(sys.stdin).get("result", {}); print(data.get("root_pane", {}).get("pane_id", ""))' 2>/dev/null || :)

[ -n "$NEW_TAB_ID" ] && [ -n "$NEW_PANE_ID" ] || { printf 'FAIL: herdr tab create failed to return tab or root pane ID\n' >&2; exit 1; }

# Derive a clean agent name matching [a-z][a-z0-9_-]{0,31}
clean_label=$(printf '%s' "$cand_label" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9_-' '-')
case "$clean_label" in
  [a-z]*) ;;
  *) clean_label="r-${clean_label}" ;;
esac
if [ "${#clean_label}" -gt 32 ]; then
  if command -v sha256sum >/dev/null 2>&1; then
    hash_sfx=$(printf '%s' "$cand_label" | sha256sum | cut -c 1-7)
  elif command -v shasum >/dev/null 2>&1; then
    hash_sfx=$(printf '%s' "$cand_label" | shasum -a 256 | cut -c 1-7)
  elif command -v python3 >/dev/null 2>&1; then
    hash_sfx=$(python3 -c 'import hashlib, sys; print(hashlib.sha256(sys.argv[1].encode()).hexdigest()[:7])' "$cand_label")
  else
    hash_sfx=$(printf '%s' "$cand_label" | cut -c 1-7)
  fi
  AGENT_NAME="$(printf '%.24s' "$clean_label")-${hash_sfx}"
else
  AGENT_NAME="$clean_label"
fi

if [ "$KIND" = "claude" ]; then
  agent_cmd="herdr agent start \"$AGENT_NAME\" --kind claude --pane \"$NEW_PANE_ID\" -- --permission-mode auto"
  if [ -n "${HARNESS_REVIEWER_MODEL:-}" ]; then
    agent_cmd="$agent_cmd --model \"$HARNESS_REVIEWER_MODEL\""
  fi
  if [ -n "${HARNESS_REVIEWER_EFFORT:-}" ]; then
    agent_cmd="$agent_cmd --effort \"$HARNESS_REVIEWER_EFFORT\""
  fi
  eval "$agent_cmd" >/dev/null 2>&1 || :
elif [ "$KIND" = "codex" ]; then
  agent_cmd="herdr agent start \"$AGENT_NAME\" --kind codex --pane \"$NEW_PANE_ID\" -- --no-daemon --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust"
  if [ -n "${HARNESS_REVIEWER_MODEL:-}" ]; then
    agent_cmd="$agent_cmd --model \"$HARNESS_REVIEWER_MODEL\""
  fi
  if [ -n "${HARNESS_REVIEWER_EFFORT:-}" ]; then
    agent_cmd="$agent_cmd -c 'model_reasoning_effort=\"$HARNESS_REVIEWER_EFFORT\"'"
  fi
  eval "$agent_cmd" >/dev/null 2>&1 || :
fi

herdr agent prompt "$AGENT_NAME" "$PROMPT" >/dev/null 2>&1 || :

# Wait for findings bounded by HARNESS_REVIEWER_TIMEOUT (default 1200 s)
TIMEOUT="${HARNESS_REVIEWER_TIMEOUT:-1200}"
start_time=$(date +%s)
deadline=$((start_time + TIMEOUT))
success=0
herdr_sid=""

while [ "$(date +%s)" -lt "$deadline" ]; do
  if [ -z "$herdr_sid" ]; then
    herdr_sid=$(herdr agent get "$AGENT_NAME" 2>/dev/null | python3 -c 'import json, sys; s = json.load(sys.stdin).get("result", {}).get("agent", {}).get("agent_session"); print((s or {}).get("value", ""))' 2>/dev/null || :)
    if [ -z "$herdr_sid" ]; then
      herdr_sid=$(herdr pane get "$NEW_PANE_ID" 2>/dev/null | python3 -c 'import json, sys; s = json.load(sys.stdin).get("result", {}).get("pane", {}).get("agent_session"); print((s or {}).get("value", ""))' 2>/dev/null || :)
    fi
  fi
  if [ -n "$herdr_sid" ] && [ -f "$HARNESS_ROOT/scripts/workflow_audit.py" ]; then
    show_out=$(python3 "$HARNESS_ROOT/scripts/workflow_audit.py" todo show --session-id "$herdr_sid" 2>/dev/null || :)
    if printf '%s' "$show_out" | grep -q '"confirmed": false'; then
      python3 "$HARNESS_ROOT/scripts/workflow_audit.py" todo exempt --session-id "$herdr_sid" --reason "Independent reviewer: packet inspection" >/dev/null 2>&1 || :
    fi
  fi

  if [ -s "$FINDINGS_OUT" ]; then
    tmp_check=$(mktemp "${TMPDIR:-/tmp}/herdr-findings-check.XXXXXX")
    cp "$FINDINGS_OUT" "$tmp_check"
    if extract_json_to_file "$tmp_check" "$FINDINGS_OUT"; then
      success=1
      rm -f "$tmp_check"
      break
    fi
    rm -f "$tmp_check"
  fi

  transcript=$(herdr agent read "$AGENT_NAME" --source recent-unwrapped --lines 500 2>/dev/null || herdr pane read "$NEW_PANE_ID" --source recent-unwrapped --lines 500 2>/dev/null || :)
  if [ -n "$transcript" ]; then
    tmp_t=$(mktemp "${TMPDIR:-/tmp}/herdr-transcript.XXXXXX")
    printf '%s\n' "$transcript" > "$tmp_t"
    if extract_json_to_file "$tmp_t" "$FINDINGS_OUT"; then
      success=1
      rm -f "$tmp_t"
      break
    fi
    rm -f "$tmp_t"
  fi

  sleep 1
done

if [ "$success" -eq 1 ] && is_valid_json "$FINDINGS_OUT"; then
  herdr tab close "$NEW_TAB_ID" >/dev/null 2>&1 || :
  exit 0
else
  printf 'FAIL: reviewer run failed or timed out in tab %s (pane %s)\n' "$cand_label" "$NEW_PANE_ID" >&2
  printf 'Reviewer tab %s (pane %s) left open for inspection.\n' "$cand_label" "$NEW_PANE_ID" >&2
  exit 1
fi
