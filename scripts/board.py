#!/usr/bin/env python3
"""Architecture boards: check, map, import and read a project's board, plus its live overlay.

Everything is written under <db>/targets/<id>/board/; nothing is ever written inside the target
project. The session-start hook runs `check --spawn`; the dashboard reads `list`, `show`, `state`
and `overlay`. See docs/architecture-board.md.

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
"""
import argparse
import calendar
import fnmatch
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
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
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=1) + "\n")
    os.replace(tmp, path)


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
    return {"edited": edited, "inScope": in_scope, "refreshing": refreshing, "unownedEdited": unowned}


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
            "cards": len(board.get("nodes", []))}


def listing(limit_recent=4, removed=False):
    """Projects with a board or a mapping state first, then the most recently active others.

    Removed projects are left out; `removed=True` lists only them, all of them."""
    rows = [t for t in targets() if Path(t["TARGET_ROOT"]).is_dir() and t["TARGET_ROOT"].rstrip("/") not in HOME_DIRS]
    rows.sort(key=lambda t: t.get("LAST_SEEN_AT", ""), reverse=True)
    if removed:
        return [entry(t) for t in rows if is_removed(t)]
    rows = [t for t in rows if not is_removed(t)]
    skip = lambda t: (load_json(board_dir(t) / "check.json", {}) or {}).get("reason") in ("not a software project", "opted out")
    rows = [t for t in rows if not skip(t)]
    known = [t for t in rows if (board_dir(t) / "board.json").exists() or (board_dir(t) / "state.json").exists()]
    recent = [t for t in rows if t not in known][:limit_recent]
    return [entry(t) for t in known + recent]


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
    ap.add_argument("command", choices=["check", "map", "import", "status", "list", "show", "state", "overlay", "remove", "restore"])
    ap.add_argument("--target")
    ap.add_argument("--spawn", action="store_true")
    ap.add_argument("--full", action="store_true")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--board")
    ap.add_argument("--removed", action="store_true")
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
