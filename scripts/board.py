#!/usr/bin/env python3
"""Architecture boards: check, map, import and read a project's board, plus its live overlay.

Everything is written under <db>/targets/<id>/board/; nothing is ever written inside the target
project. The session-start hook runs `check --spawn`; the dashboard reads `list`, `show`, `state`
and `overlay`. The overlay also carries the error heat that harness-signals writes under
<db>/targets/<id>/signals/. See docs/architecture-board.md and docs/observability-heatmap.md.

  harness board check   --target ID|PATH [--spawn] [--json]
  harness board map     --target ID|PATH [--full]
  harness board import  --target ID|PATH --board FILE
  harness board status  --target ID|PATH
  harness board list    [--json] [--removed]
  harness board remove  --target ID          (hide from the dashboard and stop mapping; the board is kept)
  harness board restore --target ID
  harness board show    --target ID          (board.json; exit 3 when not mapped)
  harness board state   --target ID --json
  harness board overlay --target ID --json
  harness board session --target PATH --session-id ID   (create or reuse the session's focus link; prints key and url)
  harness board focus   --target ID --session KEY       (signals per card for one session; exit 3 when unknown)
"""
import argparse
import calendar
import contextlib
import fnmatch
import getpass
import hashlib
import json
import os
import re
import secrets
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SCRIPT = Path(__file__).resolve()
ASSETS = SCRIPT.parent.parent / "tools" / "architecture-board"
DB_ROOT = Path(os.environ.get("HARNESS_DB_ROOT") or SCRIPT.parent.parent / ".harness-db")
SKIP_DIRS = {".git", "node_modules", "vendor", "build", "dist", ".gradle", "target", "__pycache__", ".venv", "venv", ".idea", ".next", ".cache", ".harness-db"}
NOTES = {"progress.md", "task.json", "review-findings.json"}
MANIFESTS = {"package.json", "Cargo.toml", "go.mod", "pyproject.toml", "setup.py", "requirements.txt", "settings.gradle", "settings.gradle.kts",
             "build.gradle", "build.gradle.kts", "composer.json", "Gemfile", "pom.xml", "CMakeLists.txt", "Makefile", "mix.exs", "deno.json"}
SOURCE_EXT = {".py", ".js", ".mjs", ".ts", ".tsx", ".jsx", ".kt", ".kts", ".java", ".rs", ".go", ".php", ".rb", ".swift", ".c", ".h", ".cc", ".cpp",
              ".cs", ".scala", ".ex", ".exs", ".vue", ".svelte", ".dart", ".lua", ".sh", ".sql"}
TONES = ["orange", "blue", "green", "teal", "red", "purple", "gray", "slate", "amber", "pink", "indigo", "brown"]
ICONS = ["person", "screen", "hub", "db", "bell", "plug", "term", "cloud", "grid", "server", "layers", "android", "key", "flow", "chat", "kbd",
         "rpc", "box", "check", "globe", "gear", "lock", "doc", "code", "widget", "clock", "dot"]
STYLES = ["solid", "dashed", "dotted", "faint"]
DEFAULT_KINDS = ["call", "remote", "platform", "build"]


# ---------- targets and storage ----------

def read_state(path):
    out = {}
    for line in path.read_text().splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def targets():
    rows = []
    for state in sorted((DB_ROOT / "targets").glob("*/target.state")):
        s = read_state(state)
        if s.get("TARGET_ID") and s.get("TARGET_ROOT"):
            rows.append(s)
    return rows


def resolve_target(ref):
    """A target id, or a path inside a registered target (the deepest registered root wins)."""
    rows = targets()
    for t in rows:
        if t["TARGET_ID"] == ref:
            return t
    if os.path.exists(ref):
        path = os.path.realpath(ref)
        best = None
        for t in rows:
            root = os.path.realpath(t["TARGET_ROOT"])
            if (path == root or path.startswith(root + os.sep)) and root not in {os.path.realpath(h) for h in HOME_DIRS} \
                    and (best is None or len(root) > len(os.path.realpath(best["TARGET_ROOT"]))):
                best = t
        if best:
            return best
    print(f"no registered harness target for {ref}", file=sys.stderr)
    raise SystemExit(1)


def board_dir(target):
    return DB_ROOT / "targets" / target["TARGET_ID"] / "board"


def load_json(path, default=None):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return default


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    # A temporary name of its own: the dashboard runs several board queries at
    # once, and a shared name let one rename another's file away.
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=path.name + ".", suffix=".tmp")
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(json.dumps(value, indent=1) + "\n")
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise


def is_removed(target):
    return (board_dir(target) / "removed.json").exists()


def set_removed(target, removed):
    path = board_dir(target) / "removed.json"
    if removed:
        write_json(path, {"removed_at": now()})
    else:
        path.unlink(missing_ok=True)
    return {"id": target["TARGET_ID"], "removed": removed}


def set_state(target, **fields):
    path = board_dir(target) / "state.json"
    state = load_json(path, {}) or {}
    state.update(fields, updated_at=now())
    write_json(path, state)
    return state


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


# ---------- files on disk ----------

def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=False)


def is_git(root):
    r = git(root, "rev-parse", "--is-inside-work-tree")
    return r.returncode == 0 and r.stdout.strip() == b"true"


def list_files(root):
    root = Path(root)
    if is_git(root):
        out = git(root, "ls-files", "-co", "--exclude-standard", "-z").stdout.decode("utf-8", "replace")
        paths = [p for p in out.split("\0") if p]
    else:
        paths = []
        for d, dirs, files in os.walk(root):
            dirs[:] = [x for x in dirs if x not in SKIP_DIRS and not x.startswith(".")]
            for f in files:
                paths.append(os.path.relpath(os.path.join(d, f), root))
    keep = []
    for p in paths:
        if p in NOTES or p.startswith("tasks/"):
            continue
        full = root / p
        if full.is_file() and not full.is_symlink():
            keep.append(p)
    return sorted(set(keep))


def file_hash(path):
    try:
        st = path.stat()
        if st.st_size > 5_000_000:
            return f"size:{st.st_size}:{int(st.st_mtime)}"
        return hashlib.sha1(path.read_bytes()).hexdigest()
    except OSError:
        return "unreadable"


def manifest(root):
    root = Path(root)
    return {p: file_hash(root / p) for p in list_files(root)}


def digest(man):
    h = hashlib.sha1()
    for p in sorted(man):
        h.update(f"{p}\0{man[p]}\n".encode())
    return h.hexdigest()


def is_software(root, files):
    names = {os.path.basename(p) for p in files if "/" not in p}
    if names & MANIFESTS:
        return True
    return sum(1 for p in files if os.path.splitext(p)[1] in SOURCE_EXT) >= 5


# ---------- ownership ----------

def owns(node, path):
    for pat in list(node.get("owns") or []) + [f["path"] if isinstance(f, dict) else f for f in node.get("files") or []] + list(node.get("tests") or []):
        if not pat:
            continue
        if pat == path or (pat.endswith("/") and path.startswith(pat)) or fnmatch.fnmatch(path, pat):
            return True
    return False


def owners(board, path):
    return [n["id"] for n in board.get("nodes", []) if owns(n, path)]


# ---------- check ----------

def check(target, ignore_lock=False):
    result = _check(target, ignore_lock)
    cached = {k: v for k, v in result.items() if k != "drift"}
    if result.get("drift"):
        cached["drift_counts"] = {k: len(v) for k, v in result["drift"].items()}
    if (board_dir(target) / "state.json").exists() or result.get("action") != "none" or result.get("board") \
            or result.get("reason") in ("not a software project", "opted out"):
        write_json(board_dir(target) / "check.json", dict(cached, checked_at=now()))
    return result


def _check(target, ignore_lock=False):
    root = Path(target["TARGET_ROOT"])
    bd = board_dir(target)
    board = load_json(bd / "board.json")
    old = load_json(bd / "manifest.json", {}) or {}
    state = load_json(bd / "state.json", {}) or {}
    result = {"target": target["TARGET_ID"], "root": str(root), "state": state.get("state", "none"), "board": bool(board)}
    if not root.is_dir():
        return {**result, "action": "none", "reason": "root missing"}
    if is_removed(target):
        return {**result, "action": "none", "reason": "removed"}
    if (root / ".harness-no-upload").exists() or os.environ.get("HARNESS_BOARD", "1") == "0":
        return {**result, "action": "none", "reason": "opted out"}
    man = manifest(root)
    if not is_software(root, list(man)):
        return {**result, "action": "none", "reason": "not a software project"}
    result["tree_hash"] = digest(man)
    result["files"] = len(man)
    if not ignore_lock and lock_alive(target):
        return {**result, "action": "none", "reason": "mapper running"}
    if not board:
        return {**result, "action": "map_full", "reason": "no board yet"}
    if board.get("mapped_tree_hash") == result["tree_hash"]:
        return {**result, "action": "none", "reason": "board current"}
    changed = sorted(p for p in man if p in old and old[p] != man[p])
    added = sorted(p for p in man if p not in old)
    deleted = sorted(p for p in old if p not in man)
    affected = sorted({o for p in changed + deleted for o in owners(board, p)})
    unowned_new = [p for p in added if not owners(board, p)]
    return {**result, "action": "refresh", "reason": "drifted",
            "drift": {"changed": changed, "added": added, "deleted": deleted}, "affected": affected, "unowned_new": unowned_new}


def lock_alive(target):
    lock = board_dir(target) / "mapper.lock"
    try:
        pid = int(lock.read_text().split()[0])
        os.kill(pid, 0)
        return True
    except (OSError, ValueError, IndexError):
        return False


def spawn(target, full=False):
    """Start the mapper detached; the caller returns at once."""
    if os.environ.get("HARNESS_BOARD_MAPPER") == "1":
        return None  # never map from inside a mapper session
    bd = board_dir(target)
    bd.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, HARNESS_AUTO_INIT="0", HARNESS_BOARD_MAPPER="1")
    log = open(bd / "mapper.log", "ab")
    args = [sys.executable, str(SCRIPT), "--db-root", str(DB_ROOT), "map", "--target", target["TARGET_ID"]] + (["--full"] if full else [])
    proc = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, env=env, start_new_session=True, cwd=str(bd))
    return proc.pid


# ---------- map ----------

def acquire_lock(target):
    bd = board_dir(target)
    bd.mkdir(parents=True, exist_ok=True)
    lock = bd / "mapper.lock"
    if lock.exists() and not lock_alive(target):
        lock.unlink()
    fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    os.write(fd, f"{os.getpid()} {now()}\n".encode())
    os.close(fd)
    return lock


def mapper_command():
    raw = os.environ.get("HARNESS_BOARD_MAPPER_CMD")
    if raw:
        return json.loads(raw)
    # Read-only tools, no user/project settings (so no hooks), no MCP servers, nothing persisted.
    return ["claude", "-p", "--model", os.environ.get("HARNESS_BOARD_MODEL", "sonnet"), "--tools", "Read,Grep,Glob",
            "--setting-sources", "", "--strict-mcp-config", "--no-session-persistence", "--output-format", "json"]


def build_prompt(target, files, board, drift):
    root = target["TARGET_ROOT"]
    guide = (ASSETS / "mapper-prompt.md").read_text()
    example = load_json(ASSETS / "fixtures" / "firebrik-terminal.board.json", {})
    sample = {k: example.get(k) for k in ("title", "edge_kinds")}
    sample["frames"] = example.get("frames", [])[:3]
    sample["nodes"] = [n for n in example.get("nodes", []) if n["id"] in ("session", "state", "user")]
    sample["edges"] = [e for e in example.get("edges", []) if e["from"] in ("session", "user") or e["to"] == "state"][:5]
    sample["flows"] = example.get("flows", [])[:1]
    map_md = DB_ROOT / "targets" / target["TARGET_ID"] / "map.md"
    listing = files if len(files) <= 1500 else files[:1500] + [f"... {len(files) - 1500} more files"]
    parts = [guide,
             f"\n## Project\n\nRoot (your working directory): {root}\nName: {os.path.basename(root)}\n",
             "\n## Harness project map\n\n" + (map_md.read_text()[:6000] if map_md.exists() else "(none)"),
             "\n## Files (paths relative to the root)\n\n" + "\n".join(listing),
             "\n## Example of the expected quality (a slice of another project's board)\n\n```json\n" + json.dumps(sample, indent=1)[:9000] + "\n```"]
    if board and drift:
        keep = {k: v for k, v in board.items() if k not in ("mapped_at", "mapped_tree_hash", "source")}
        parts.append("\n## Refresh, not a new board\n\nThe board below is current except for the drift listed after it. "
                     "Re-describe only the affected cards, place new files (on an existing card or a new card in a free cell), "
                     "drop deleted paths, and keep every other card, position, edge and flow exactly as it is. Return the full board.\n\n"
                     "```json\n" + json.dumps(keep, indent=1) + "\n```\n\nDrift:\n```json\n" + json.dumps(drift, indent=1)[:20000] + "\n```")
    return "\n".join(parts)


def run_mapper(prompt, cwd, log):
    cmd = mapper_command()
    env = dict(os.environ, HARNESS_AUTO_INIT="0", HARNESS_BOARD_MAPPER="1")
    timeout = int(os.environ.get("HARNESS_BOARD_TIMEOUT", "1200"))
    log(f"running: {' '.join(cmd)}")
    proc = subprocess.Popen(cmd, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, start_new_session=True)
    try:
        out, err = proc.communicate(prompt, timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        raise RuntimeError(f"mapper timed out after {timeout}s")
    if proc.returncode != 0:
        raise RuntimeError(f"mapper exited {proc.returncode}: {(err or out)[-800:]}")
    text, usage = out, {}
    try:
        env_json = json.loads(out[out.find("{"):])  # launchers (mise) may print a line before the JSON envelope
        text = env_json.get("result", "")
        usage = {"cost_usd": env_json.get("total_cost_usd"), "duration_ms": env_json.get("duration_ms"), "turns": env_json.get("num_turns")}
    except ValueError:
        pass
    return text, usage


def extract_json(text):
    """The largest JSON object in the answer that looks like a board (fenced blocks first, then the outermost braces)."""
    candidates = re.findall(r"```(?:json)?\s*(\{.*?\})\s*```", text, re.S)
    candidates.append(text[text.find("{"): text.rfind("}") + 1])
    best = None
    for raw in sorted(candidates, key=len, reverse=True):
        try:
            value = json.loads(raw)
        except ValueError:
            continue
        if isinstance(value, dict) and (value.get("nodes") or value.get("cards")):
            best = value
            break
        best = best or value
    if best is None:
        raise ValueError("no JSON object found")
    if isinstance(best, dict) and len(best) == 1 and isinstance(next(iter(best.values())), dict):
        best = next(iter(best.values()))  # {"board": {...}}
    if "nodes" not in best and "cards" in best:
        best["nodes"] = best.pop("cards")
    return best


def validate(board, root):
    """Return (clean_board, errors, warnings). Errors make the result unusable; warnings are fixed in place."""
    errors, warnings = [], []
    root = os.path.realpath(root)

    def inside(p):
        full = os.path.realpath(os.path.join(root, p))
        return (full == root or full.startswith(root + os.sep)) and os.path.exists(full)

    kinds = {k: v for k, v in (board.get("edge_kinds") or {}).items() if isinstance(v, dict)}
    for v in kinds.values():
        v["tone"] = v.get("tone") if v.get("tone") in TONES else "slate"
        v["style"] = v.get("style") if v.get("style") in STYLES else "solid"
    frames, fids = [], set()
    for f in board.get("frames") or []:
        if not isinstance(f, dict) or not f.get("id") or f["id"] in fids:
            errors.append(f"frame without a unique id: {f!r:.80}")
            continue
        fids.add(f["id"])
        try:
            f.update(col=int(f.get("col", 0)), row=int(f.get("row", 0)), cols=max(1, int(f.get("cols", 1))), rows=max(1, int(f.get("rows", 1))))
        except (TypeError, ValueError):
            errors.append(f"frame {f['id']}: non-integer position")
            continue
        if f.get("tone") not in TONES:
            f["tone"] = "slate"
        frames.append(f)
    for i, a in enumerate(frames):
        for b in frames[i + 1:]:
            if a["col"] < b["col"] + b["cols"] and b["col"] < a["col"] + a["cols"] and a["row"] < b["row"] + b["rows"] and b["row"] < a["row"] + a["rows"]:
                errors.append(f"frames {a['id']} and {b['id']} overlap")
    nodes, nids, cells = [], set(), {}
    fmap = {f["id"]: f for f in frames}
    for n in board.get("nodes") or []:
        if not isinstance(n, dict) or not n.get("id") or n["id"] in nids:
            errors.append(f"node without a unique id: {n!r:.80}")
            continue
        nids.add(n["id"])
        f = fmap.get(n.get("frame"))
        if not f:
            errors.append(f"node {n['id']}: unknown frame {n.get('frame')!r}")
            continue
        try:
            n["col"], n["row"] = int(n.get("col", 0)), int(n.get("row", 0))
        except (TypeError, ValueError):
            errors.append(f"node {n['id']}: non-integer position")
            continue
        if not (f["col"] <= n["col"] < f["col"] + f["cols"] and f["row"] <= n["row"] < f["row"] + f["rows"]):
            errors.append(f"node {n['id']} at ({n['col']},{n['row']}) is outside frame {f['id']}")
        cell = (n["col"], n["row"])
        if cell in cells:
            errors.append(f"nodes {cells[cell]} and {n['id']} share cell {cell}")
        cells[cell] = n["id"]
        if n.get("icon") not in ICONS:
            n["icon"] = "dot"
        files = []
        for x in n.get("files") or []:
            x = {"path": x, "note": ""} if isinstance(x, str) else x
            if isinstance(x, dict) and x.get("path") and inside(x["path"]):
                files.append({"path": x["path"], "note": x.get("note", "")})
            else:
                warnings.append(f"node {n['id']}: dropped missing file {x!r:.100}")
        n["files"] = files
        n["tests"] = [t for t in n.get("tests") or [] if isinstance(t, str) and inside(t)]
        n["owns"] = [o for o in n.get("owns") or [] if isinstance(o, str) and o]
        nodes.append(n)
    edges = []
    for e in board.get("edges") or []:
        if not isinstance(e, dict) or e.get("from") not in nids or e.get("to") not in nids or e.get("from") == e.get("to"):
            warnings.append(f"dropped edge {e!r:.100}")
            continue
        if e.get("kind") not in kinds and e.get("kind") not in DEFAULT_KINDS:
            e["kind"] = "call"
        edges.append({"from": e["from"], "to": e["to"], "label": str(e.get("label", "")), "kind": e["kind"]})
    flows = []
    for fl in board.get("flows") or []:
        steps = [s for s in fl.get("steps") or [] if isinstance(s, list) and len(s) == 2 and s[0] in nids and s[1] in nids]
        if steps:
            flows.append({"id": str(fl.get("id", f"flow{len(flows)}")), "title": fl.get("title", ""), "sub": fl.get("sub", ""), "steps": steps, "text": [str(t) for t in fl.get("text") or []]})
    if not nodes:
        errors.append("board has no cards")
    clean = {"version": 1, "title": board.get("title") or os.path.basename(root), "overview": board.get("overview") or {},
             "edge_kinds": kinds, "frames": frames, "nodes": nodes, "edges": edges, "flows": flows}
    return clean, errors, warnings


def repo_info(root):
    url = git(root, "remote", "get-url", "origin").stdout.decode().strip()
    m = re.match(r"(?:git@|https://)([^/:]+)[/:](.+?)(?:\.git)?$", url)
    branch = git(root, "symbolic-ref", "--short", "refs/remotes/origin/HEAD").stdout.decode().strip().split("/")[-1]
    return (f"https://{m.group(1)}/{m.group(2)}" if m else ""), (branch or "main")


def do_map(target, full=False):
    root = target["TARGET_ROOT"]
    bd = board_dir(target)
    log = lambda m: print(f"[{now()}] {m}", flush=True)
    try:
        acquire_lock(target)
    except FileExistsError:
        log("another mapper holds the lock")
        return 1
    try:
        info = check(target, ignore_lock=True) if not full else {"action": "map_full"}
        board = None if full else load_json(bd / "board.json")
        drift = None
        if board and info.get("action") == "refresh":
            drift = {**info["drift"], "affected_cards": info["affected"], "unowned_new": info["unowned_new"]}
        mode = "refreshing" if drift else "mapping"
        set_state(target, state=mode, started_at=now(), affected=(drift or {}).get("affected_cards", []), error=None)
        man = manifest(root)  # snapshot at launch: edits made during the run show up as drift next time
        prompt = build_prompt(target, list(man), board, drift)
        log(f"{mode}: {len(man)} files, prompt {len(prompt)} chars")
        text, usage = run_mapper(prompt, root, log)
        (bd / "last-answer.txt").write_text(text)
        try:
            raw = extract_json(text)
        except ValueError as exc:
            raise RuntimeError(f"mapper did not return JSON: {exc}")
        clean, errors, warnings = validate(raw, root)
        if errors:
            log(f"{len(errors)} structural errors; asking for one repair: {errors[:5]}")
            repair = (prompt + "\n\n## Your previous answer had structural errors\n\n" + "\n".join(f"- {e}" for e in errors[:40]) +
                      "\n\nPrevious answer:\n```json\n" + json.dumps(raw)[:60000] + "\n```\n\nReturn the corrected full board JSON only.")
            text, usage2 = run_mapper(repair, root, log)
            (bd / "last-answer.txt").write_text(text)
            usage = {k: (usage.get(k) or 0) + (usage2.get(k) or 0) for k in set(usage) | set(usage2)}
            clean, errors, warnings = validate(extract_json(text), root)
            if errors:
                raise RuntimeError("structural errors after repair: " + "; ".join(errors[:8]))
        repo, branch = repo_info(root)
        clean.update(repo_url=repo, default_branch=branch, mapped_at=now(), mapped_tree_hash=digest(man), source="mapper")
        write_json(bd / "board.json", clean)
        write_json(bd / "manifest.json", man)
        set_state(target, state="ready", finished_at=now(), affected=[], warnings=warnings[:50], usage=usage, error=None)
        log(f"ready: {len(clean['nodes'])} cards, {len(clean['edges'])} edges, {len(warnings)} warnings, usage {usage}")
        return 0
    except Exception as exc:  # the session never depends on this; record and move on
        set_state(target, state="failed", finished_at=now(), error=str(exc)[:600])
        log(f"failed: {exc}")
        return 1
    finally:
        try:
            (bd / "mapper.lock").unlink()
        except OSError:
            pass


def do_import(target, path):
    root = target["TARGET_ROOT"]
    clean, errors, warnings = validate(load_json(path), root)
    if errors:
        raise SystemExit("import refused: " + "; ".join(errors))
    src = load_json(path)
    man = manifest(root)
    clean.update(repo_url=src.get("repo_url", ""), default_branch=src.get("default_branch", "main"), mapped_at=now(), mapped_tree_hash=digest(man), source="imported")
    bd = board_dir(target)
    write_json(bd / "board.json", clean)
    write_json(bd / "manifest.json", man)
    set_state(target, state="ready", finished_at=now(), affected=[], warnings=warnings, error=None)
    print(f"imported {len(clean['nodes'])} cards into {bd}; {len(warnings)} warnings")



# ---------- worktrees, runs and sessions ----------

LIVE_SECONDS = 30 * 60
WORKTREE_DAYS = 14


def id_for_root(root):
    """The registry id harness-target.sh derives for a root path."""
    base = re.sub(rb"[^A-Za-z0-9._-]", b"_", os.path.basename(root).encode())[:48].decode()
    return f"{base}-{hashlib.sha256(root.encode()).hexdigest()[:12]}"


def target_kind(target):
    if target.get("TARGET_KIND"):
        return target["TARGET_KIND"]
    root = target["TARGET_ROOT"]
    return "worktree" if os.path.isfile(os.path.join(root, ".git")) else "repo" if os.path.isdir(os.path.join(root, ".git")) else "dir"


def parent_root_of(target):
    """The main checkout of a worktree target: from target.state, else a cached git answer."""
    if target_kind(target) != "worktree":
        return ""
    if "TARGET_PARENT_ROOT" in target:
        return target["TARGET_PARENT_ROOT"]
    cache = board_dir(target) / "parent.json"
    cached = load_json(cache)
    if isinstance(cached, dict) and "parent_root" in cached:
        return cached["parent_root"] or ""
    common = git(target["TARGET_ROOT"], "rev-parse", "--path-format=absolute", "--git-common-dir").stdout.decode("utf-8", "replace").strip()
    parent = os.path.realpath(common[:-5]) if common.endswith("/.git") else ""
    with contextlib.suppress(OSError):
        write_json(cache, {"parent_root": parent})
    return parent


def parent_id_of(target, known=None):
    """The registered main folder's id for a worktree, or None."""
    parent = parent_root_of(target)
    if not parent:
        return None
    pid = id_for_root(parent)
    known = known if known is not None else {t["TARGET_ID"] for t in targets()}
    return pid if pid in known else None


def head_branch(root):
    """Current branch from HEAD files alone (no git process); None when detached or unreadable."""
    try:
        dot = os.path.join(root, ".git")
        if os.path.isfile(dot):
            line = Path(dot).read_text().strip()
            gitdir = line[len("gitdir:"):].strip() if line.startswith("gitdir:") else ""
            gitdir = gitdir if os.path.isabs(gitdir) else os.path.join(root, gitdir)
        else:
            gitdir = dot
        head = Path(gitdir, "HEAD").read_text().strip()
    except OSError:
        return None
    return head[len("ref: refs/heads/"):] if head.startswith("ref: refs/heads/") else None


def parse_time(text):
    try:
        return calendar.timegm(time.strptime(text, "%Y-%m-%dT%H:%M:%SZ"))
    except (ValueError, TypeError):
        return None


def runs_dir(target):
    return DB_ROOT / "targets" / target["TARGET_ID"] / "db" / "runs"


def run_view(run_dir):
    """What a run says about itself: id, phase, title, liveness and last activity."""
    info = load_json(run_dir / "run.json")
    if not isinstance(info, dict):
        return None
    try:
        mtime = (run_dir / "run.json").stat().st_mtime
    except OSError:
        mtime = 0
    updated = parse_time(info.get("updated_at")) or mtime
    task = load_json(run_dir / "task.json", {}) or {}
    return {"run_id": info.get("run_id") or run_dir.name, "phase": info.get("current_phase"), "title": task.get("goal") if isinstance(task, dict) else None,
            "live": info.get("status") == "active" and time.time() - updated < LIVE_SECONDS,
            "last_active": info.get("updated_at") or time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(mtime)), "updated": updated, "dir": run_dir}


def latest_run(target):
    best = None
    try:
        for p in runs_dir(target).iterdir():
            if not p.is_dir() or p.name in ("archive", "sessions"):
                continue
            try:
                m = (p / "run.json").stat().st_mtime
            except OSError:
                continue
            if best is None or m > best[0]:
                best = (m, p)
    except OSError:
        return None
    return run_view(best[1]) if best else None


def session_records(target):
    """Session records of a target, newest first."""
    rows = []
    for p in (board_dir(target) / "sessions").glob("*.json"):
        rec = load_json(p)
        if isinstance(rec, dict) and rec.get("key"):
            try:
                rows.append((p.stat().st_mtime, rec))
            except OSError:
                pass
    return [r for _, r in sorted(rows, key=lambda x: x[0], reverse=True)]


def tailscale_login():
    for exe in ("tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"):
        try:
            out = subprocess.run([exe, "status", "--json"], capture_output=True, text=True, timeout=2)
            data = json.loads(out.stdout)
            return data["User"][str(data["Self"]["UserID"])]["LoginName"] or None
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            continue
    return None


def audit_url(target_id, key):
    base = (os.environ.get("HARNESS_AUDIT_URL") or "http://127.0.0.1:18080").rstrip("/")
    return f"{base}/projects/{target_id}/sessions/{key}"


def start_session(target, session_id):
    """Create (or reuse) the focus link record for one agent session; idempotent per session id."""
    for rec in session_records(target):
        if rec.get("session_id") == session_id:
            return {"key": rec["key"], "url": audit_url(target["TARGET_ID"], rec["key"])}
    key = "s" + secrets.token_urlsafe(24)
    rec = {"key": key, "session_id": session_id, "target": target["TARGET_ID"], "root": target["TARGET_ROOT"],
           "owner": {"user": getpass.getuser(), "login": tailscale_login()}, "started_at": now()}
    write_json(board_dir(target) / "sessions" / f"{key}.json", rec)
    return {"key": key, "url": audit_url(target["TARGET_ID"], key)}


def default_branch(root, board):
    name = board.get("default_branch")
    if not name:
        ref = git(root, "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD").stdout.decode().strip()
        name = ref[len("refs/remotes/origin/"):] if ref.startswith("refs/remotes/origin/") else None
    if not name:
        for cand in ("main", "master"):
            if git(root, "rev-parse", "--verify", "--quiet", cand).returncode == 0:
                return cand
    return name


def changed_paths(root, base_name):
    """Files changed on this branch since it left the default branch, plus uncommitted and untracked ones."""
    paths = set()
    best = None
    for ref in (base_name, f"origin/{base_name}") if base_name else ():
        mb = git(root, "merge-base", "HEAD", ref)
        sha = mb.stdout.decode().strip() if mb.returncode == 0 else ""
        if not sha:
            continue
        count = git(root, "rev-list", "--count", f"{sha}..HEAD")
        try:
            distance = int(count.stdout.decode().strip())
        except ValueError:
            continue
        if best is None or distance < best[0]:
            best = (distance, sha)
    if best:
        out = git(root, "diff", "--name-only", "-z", best[1]).stdout.decode("utf-8", "replace")
        paths.update(x for x in out.split("\0") if x)
    out = git(root, "status", "--porcelain", "-z", "--untracked-files=all").stdout.decode("utf-8", "replace").split("\0")
    i = 0
    while i < len(out):
        entry_ = out[i]
        i += 1
        if len(entry_) < 4:
            continue
        code, path = entry_[:2], entry_[3:]
        if "R" in code or "C" in code:
            i += 1
        paths.add(path)
    return {p for p in paths if p not in NOTES}


def touched_paths(session_id, root):
    db = Path(os.environ.get("HARNESS_WORKFLOW_STATE") or SCRIPT.parent.parent / ".harness-db" / "workflow-state.sqlite")
    if not db.exists():
        return set()
    prefixes = {root.rstrip("/") + "/", os.path.realpath(root).rstrip("/") + "/"}
    found = set()
    try:
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=2)
        try:
            for (path,) in con.execute("SELECT path FROM touched_files WHERE session_id=?", (session_id,)):
                for pre in prefixes:
                    if path.startswith(pre):
                        found.add(path[len(pre):])
                        break
        finally:
            con.close()
    except sqlite3.Error:
        pass
    return found


def session_run(target, session_id):
    """The run this session owns, through its run pointer; never the newest run."""
    if str(SCRIPT.parent) not in sys.path:
        sys.path.insert(0, str(SCRIPT.parent))
    try:
        from run_paths import session_key
        key = session_key(session_id)
    except (ValueError, ImportError):
        return None
    if not key:
        return None
    try:
        run_id = (runs_dir(target) / "sessions" / key / "current").read_text().splitlines()[0].strip()
    except (OSError, IndexError):
        return None
    return run_view(runs_dir(target) / run_id) if run_id and "/" not in run_id else None


def plural(n, word):
    return f"{n} {word}" + ("" if n == 1 else "s")


def focus(target, key):
    rec = load_json(board_dir(target) / "sessions" / f"{key}.json") if re.fullmatch(r"[A-Za-z0-9_-]+", key or "") else None
    if not isinstance(rec, dict) or rec.get("key") != key:
        return None
    root = target["TARGET_ROOT"]
    pid = parent_id_of(target)
    board_target = target
    if pid:
        parent = next(t for t in targets() if t["TARGET_ID"] == pid)
        if (board_dir(parent) / "board.json").exists():
            board_target = parent
    board = load_json(board_dir(board_target) / "board.json") or {}
    base = default_branch(root, board)
    edited = changed_paths(root, base) if is_git(root) else set()
    run = session_run(target, rec.get("session_id", ""))
    scope = set()
    if run:
        task = load_json(run["dir"] / "task.json", {}) or {}
        scope = {str(d.get("path", "") if isinstance(d, dict) else d) for d in task.get("deliverables") or []} - {""}
    touched = touched_paths(rec.get("session_id", ""), root) - edited - scope
    signals, unmapped = {}, set()
    for kind, paths in (("edited", edited), ("scope", scope), ("touched", touched)):
        for path in sorted(paths):
            ids = owners(board, path)
            if not ids:
                unmapped.add(path)
            for cid in ids:
                signals.setdefault(cid, {"edited": [], "scope": [], "touched": []})[kind].append(path)
    branch = head_branch(root)
    count = lambda kind: sum(1 for v in signals.values() if v[kind])
    summary = (f"This session has changed {plural(count('edited'), 'part')}, planned {plural(count('scope'), 'part')} "
               f"and read {plural(count('touched'), 'part')}.")
    return {"target": target["TARGET_ID"], "board_target": board_target["TARGET_ID"], "parent": pid,
            "session": {"key": key, "owner": rec.get("owner") or {"user": None, "login": None}, "branch": branch, "base": base, "root": root,
                        "run_id": run["run_id"] if run else None, "phase": run["phase"] if run else None,
                        "title": (run or {}).get("title") or branch, "live": bool(run and run["live"]),
                        "last_active": run["last_active"] if run else None},
            "signals": signals, "unmapped": sorted(unmapped), "summary": summary}


# ---------- live overlay (computed per request, never stored) ----------

def overlay(target):
    root = target["TARGET_ROOT"]
    bd = board_dir(target)
    board = load_json(bd / "board.json") or {}
    state = load_json(bd / "state.json", {}) or {}
    edited, unowned = {}, []
    if is_git(root):
        out = git(root, "status", "--porcelain", "-z", "--untracked-files=all").stdout.decode("utf-8", "replace").split("\0")
        i = 0
        while i < len(out):
            entry = out[i]
            i += 1
            if len(entry) < 4:
                continue
            code, path = entry[:2], entry[3:]
            if "R" in code or "C" in code:
                i += 1  # skip the original name of a rename
            if path in NOTES:
                continue
            ids = owners(board, path)
            for nid in ids:
                edited.setdefault(nid, []).append(path)
            if not ids:
                unowned.append(path)
    in_scope = []
    runs = DB_ROOT / "targets" / target["TARGET_ID"] / "db" / "runs"
    try:
        latest = max((p for p in runs.iterdir() if p.is_dir() and p.name != "archive"), key=lambda p: p.stat().st_mtime)
        deliverables = (load_json(latest / "task.json", {}) or {}).get("deliverables") or []
        in_scope = sorted({o for d in deliverables for o in owners(board, str(d.get("path", d) if isinstance(d, dict) else d))})
    except (OSError, ValueError):
        pass
    refreshing = state.get("affected", []) if state.get("state") in ("mapping", "refreshing") else []
    return {"edited": edited, "inScope": in_scope, "refreshing": refreshing, "unownedEdited": unowned, "heat": heat_view(target)}


HEAT_STALE_SECONDS = 900


def heat_view(target):
    """The error heat harness-signals wrote for this target, or None when it tracks nothing here."""
    heat = load_json(board_dir(target).parent / "signals" / "heat.json")
    if not isinstance(heat, dict) or not isinstance(heat.get("cards"), dict):
        return None
    try:
        age = time.time() - calendar.timegm(time.strptime(heat.get("generated_at", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except ValueError:
        age = None
    heat["stale"] = age is None or age > HEAT_STALE_SECONDS
    return heat


HOME_DIRS = {str(Path.home()), str(Path.home() / "Code"), str(Path.home() / "Downloads")}


def entry(target):
    """Sidebar row: cheap, from files only (the last check is cached in check.json)."""
    bd = board_dir(target)
    state = load_json(bd / "state.json", {}) or {}
    last = load_json(bd / "check.json", {}) or {}
    board = load_json(bd / "board.json") or {}
    s = state.get("state")
    if s in ("mapping", "refreshing") and lock_alive(target):
        kind, label = s, s + "\u2026"
    elif s == "failed":
        kind, label = "failed", "failed"
    elif board:
        kind, label = ("drifted", "drifted") if last.get("action") == "refresh" else ("ready", "ready")
    else:
        kind, label = "none", "not mapped"
    return {"id": target["TARGET_ID"], "name": os.path.basename(target["TARGET_ROOT"].rstrip("/")), "root": target["TARGET_ROOT"],
            "last_seen": target.get("LAST_SEEN_AT", ""), "state": kind, "label": label, "mapped_at": board.get("mapped_at"),
            "cards": len(board.get("nodes", [])), "kind": target_kind(target), "parent": None, "branch": head_branch(target["TARGET_ROOT"])}


def worktree_row(target, known):
    """A worktree's row, or None when it is stale: no session record and no run in the last 14 days."""
    sessions = session_records(target)
    run = latest_run(target)
    if not sessions and not (run and time.time() - run["updated"] < WORKTREE_DAYS * 86400):
        return None
    row = entry(target)
    row["parent"] = parent_id_of(target, known)
    owner = (sessions[0].get("owner") if sessions else None) or {"user": getpass.getuser(), "login": None}
    row["owner"] = {"user": owner.get("user"), "login": owner.get("login")}
    row["session_key"] = sessions[0]["key"] if sessions else None
    row["task"] = {"title": run["title"] or row["branch"], "phase": run["phase"], "run_id": run["run_id"], "live": run["live"],
                   "last_active": run["last_active"]} if run else None
    return row


def listing(limit_recent=4, removed=False):
    """Projects with a board or a mapping state first, then the most recently active others.

    Worktrees are listed after their main folder, and only while active (a session record or a run in the last 14 days).
    Removed projects are left out; `removed=True` lists only them, all of them."""
    rows = [t for t in targets() if Path(t["TARGET_ROOT"]).is_dir() and t["TARGET_ROOT"].rstrip("/") not in HOME_DIRS]
    rows.sort(key=lambda t: t.get("LAST_SEEN_AT", ""), reverse=True)
    if removed:
        return [entry(t) for t in rows if is_removed(t)]
    by_id = {t["TARGET_ID"]: t for t in rows}
    trees = [t for t in rows if target_kind(t) == "worktree" and not is_removed(t)]
    rows = [t for t in rows if not is_removed(t) and target_kind(t) != "worktree"]
    skip = lambda t: (load_json(board_dir(t) / "check.json", {}) or {}).get("reason") in ("not a software project", "opted out")
    rows = [t for t in rows if not skip(t)]
    known = [t for t in rows if (board_dir(t) / "board.json").exists() or (board_dir(t) / "state.json").exists()]
    recent = [t for t in rows if t not in known][:limit_recent]
    out = [entry(t) for t in known + recent]
    present = {r["id"] for r in out}
    children = [r for r in (worktree_row(t, set(by_id)) for t in trees) if r]
    for r in children:
        pid = r["parent"]
        if pid and pid not in present:
            out.append(entry(by_id[pid]))
            present.add(pid)
    children.sort(key=lambda r: (r["task"] or {}).get("last_active") or "", reverse=True)
    return out + children


def state_view(target, max_age=20):
    """Board page status: mapping state, the last check (recomputed when stale) and the mapper log tail."""
    bd = board_dir(target)
    last = load_json(bd / "check.json", {}) or {}
    try:
        age = time.time() - calendar.timegm(time.strptime(last.get("checked_at", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except ValueError:
        age = None
    if age is None or age > max_age:
        info = check(target)
        last = {k: v for k, v in info.items() if k != "drift"}
        if info.get("drift"):
            last["drift_counts"] = {k: len(v) for k, v in info["drift"].items()}
    board = load_json(bd / "board.json") or {}
    try:
        log = "\n".join((bd / "mapper.log").read_text(errors="replace").splitlines()[-12:])
    except OSError:
        log = ""
    return {"target": target["TARGET_ID"], "name": os.path.basename(target["TARGET_ROOT"].rstrip("/")), "root": target["TARGET_ROOT"],
            "state": load_json(bd / "state.json", {}) or {}, "running": lock_alive(target), "mapped_at": board.get("mapped_at"),
            "source": board.get("source"), "check": last, "log": log}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db-root")
    ap.add_argument("command", choices=["check", "map", "import", "status", "list", "show", "state", "overlay", "remove", "restore", "session", "focus"])
    ap.add_argument("--target")
    ap.add_argument("--spawn", action="store_true")
    ap.add_argument("--full", action="store_true")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--board")
    ap.add_argument("--removed", action="store_true")
    ap.add_argument("--session-id")
    ap.add_argument("--session")
    argv = list(sys.argv[1:] if argv is None else argv)
    for i in range(len(argv) - 1):
        if argv[i] == "--session" and re.fullmatch(r"-[A-Za-z0-9_-]{31}", argv[i + 1]):
            argv[i:i + 2] = ["--session=" + argv[i + 1]]
            break
    a = ap.parse_args(argv)
    global DB_ROOT
    if a.db_root:
        DB_ROOT = Path(a.db_root)
    if a.command == "list":
        rows = listing(removed=a.removed)
        print(json.dumps(rows) if a.json else "\n".join(f"{r['name']:<32} {r['label']:<12} {r['root']}" for r in rows))
        return 0
    if not a.target:
        ap.error("--target is required")
    target = resolve_target(a.target)
    if a.command == "session":
        if not a.session_id:
            ap.error("--session-id is required")
        print(json.dumps(start_session(target, a.session_id)))
        return 0
    if a.command == "focus":
        view = focus(target, a.session or "")
        if view is None:
            print(json.dumps({"error": "unknown session"}))
            return 3
        print(json.dumps(view))
        return 0
    if a.command == "check":
        info = check(target)
        if a.spawn and info["action"] != "none":
            info["spawned_pid"] = spawn(target, full=info["action"] == "map_full")
        if a.quiet:
            return 0
        if a.json:
            print(json.dumps(info, indent=1))
        else:
            d = info.get("drift") or {}
            print(f"{info['target']}: {info['action']} ({info['reason']})"
                  + (f"; changed {len(d['changed'])}, added {len(d['added'])}, deleted {len(d['deleted'])}, affected cards {info['affected']}" if d else "")
                  + ("; mapper started in background" if info.get("spawned_pid") else ""))
        return 0
    if a.command == "map":
        return do_map(target, a.full)
    if a.command == "import":
        do_import(target, a.board)
        return 0
    if a.command == "show":
        board = load_json(board_dir(target) / "board.json")
        if not board:
            print(json.dumps({"error": "not mapped"}))
            return 3
        print(json.dumps(board))
        return 0
    if a.command == "state":
        print(json.dumps(state_view(target)))
        return 0
    if a.command in ("remove", "restore"):
        print(json.dumps(set_removed(target, a.command == "remove")))
        return 0
    if a.command == "overlay":
        print(json.dumps(overlay(target)))
        return 0
    print(json.dumps(state_view(target, max_age=0), indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
