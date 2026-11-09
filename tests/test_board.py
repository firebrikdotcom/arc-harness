"""Architecture board: drift check, validation, mapper run with a fake agent, overlay, and the session-start trigger."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import board  # noqa: E402

FIXTURE = ROOT / "tools" / "architecture-board" / "fixtures" / "firebrik-terminal.board.json"

SMALL_BOARD = {
    "title": "Demo",
    "overview": {"heading": "Demo", "text": "A tiny project."},
    "frames": [{"id": "core", "title": "Core", "tag": "src", "tone": "green", "col": 0, "row": 0, "cols": 2, "rows": 1}],
    "nodes": [
        {"id": "app", "frame": "core", "col": 0, "row": 0, "icon": "hub", "title": "App", "sub": "entry", "summary": "Starts.",
         "responsibility": "Wires.", "files": [{"path": "src/app.py", "note": "entry"}], "tests": ["tests/test_app.py"], "owns": ["src/app.py", "tests/"]},
        {"id": "store", "frame": "core", "col": 1, "row": 0, "icon": "db", "title": "Store", "sub": "data", "summary": "Keeps data.",
         "responsibility": "Persists.", "files": [{"path": "src/store.py", "note": ""}], "tests": [], "owns": ["src/store.py"]},
    ],
    "edges": [{"from": "app", "to": "store", "label": "reads", "kind": "call"}],
    "flows": [{"id": "run", "title": "Run", "sub": "", "steps": [["app", "store"]], "text": ["App reads the store."]}],
}

# A fake agent: prints a launcher line before the JSON envelope (as mise does) and answers with SMALL_BOARD.
FAKE_AGENT = """import json, sys
sys.stdin.read()
print("mise tools: claude@1")
board = json.loads(%r)
print(json.dumps({"result": "Here is the board:\\n```json\\n" + json.dumps(board) + "\\n```", "total_cost_usd": 0.01, "duration_ms": 5, "num_turns": 1}))
""" % json.dumps(SMALL_BOARD)


def git(cwd, *args):
    subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True)


class BoardTests(unittest.TestCase):
    def setUp(self):
        # verify.sh exports HARNESS_BOARD=0 for nested runs; these tests exercise the board itself.
        self.saved_switch = os.environ.pop("HARNESS_BOARD", None)
        self.saved_mapper = os.environ.pop("HARNESS_BOARD_MAPPER", None)
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name).resolve()
        self.db = base / "db"
        self.project = base / "demo"
        (self.project / "src").mkdir(parents=True)
        (self.project / "tests").mkdir()
        for name in ("app.py", "store.py", "util.py", "cli.py", "web.py"):
            (self.project / "src" / name).write_text(f"# {name}\n")
        (self.project / "tests" / "test_app.py").write_text("# test\n")
        (self.project / "Makefile").write_text("test:\n\ttrue\n")
        git(self.project, "init", "-q")
        git(self.project, "add", ".")
        git(self.project, "-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "init")
        registered = subprocess.run(
            ["sh", str(ROOT / "scripts" / "harness-target.sh"), "register", str(self.project)],
            env=dict(os.environ, HARNESS_DB_ROOT=str(self.db)),
            check=True, capture_output=True, text=True,
        )
        self.target_id = Path(registered.stdout.strip()).name
        board.DB_ROOT = self.db
        self.target = board.resolve_target(self.target_id)
        self.agent = base / "agent.py"
        self.agent.write_text(FAKE_AGENT)
        self.env = dict(os.environ, HARNESS_BOARD_MAPPER_CMD=json.dumps([sys.executable, str(self.agent)]))
        self.env.pop("HARNESS_BOARD_MAPPER", None)

    def tearDown(self):
        self.tmp.cleanup()
        if self.saved_switch is not None:
            os.environ["HARNESS_BOARD"] = self.saved_switch
        if self.saved_mapper is not None:
            os.environ["HARNESS_BOARD_MAPPER"] = self.saved_mapper

    def project_state(self):
        return subprocess.run(["git", "-C", str(self.project), "status", "--porcelain", "--ignored"], capture_output=True, text=True).stdout

    def run_cli(self, *args, env=None):
        return subprocess.run([sys.executable, str(ROOT / "scripts" / "board.py"), "--db-root", str(self.db), *args],
                              capture_output=True, text=True, env=env or self.env, timeout=60)

    def test_concurrent_writes_to_one_file_do_not_collide(self):
        # The dashboard runs several board queries at once, each rewriting check.json.
        import threading
        path = self.db / "targets" / "demo-1" / "board" / "check.json"
        errors = []

        def write(n):
            try:
                for i in range(50):
                    board.write_json(path, {"writer": n, "i": i})
            except Exception as error:  # noqa: BLE001
                errors.append(error)

        threads = [threading.Thread(target=write, args=(n,)) for n in range(8)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(errors, [])
        self.assertIn("writer", json.loads(path.read_text()))
        self.assertEqual(list(path.parent.glob("*.tmp")), [])

    def test_resolve_by_path_inside_target(self):
        self.assertEqual(board.resolve_target(str(self.project / "src"))["TARGET_ID"], self.target_id)

    def test_check_without_board_asks_for_full_mapping(self):
        info = board.check(self.target)
        self.assertEqual(info["action"], "map_full")
        self.assertTrue((self.db / "targets" / self.target_id / "board" / "check.json").exists())

    def test_check_skips_non_software_and_opted_out(self):
        for p in self.project.rglob("*.py"):
            p.unlink()
        (self.project / "Makefile").unlink()
        (self.project / "notes.md").write_text("hi")
        self.assertEqual(board.check(self.target)["reason"], "not a software project")
        (self.project / "Makefile").write_text("x:\n")
        (self.project / ".harness-no-upload").write_text("")
        self.assertEqual(board.check(self.target)["reason"], "opted out")

    def test_map_writes_only_to_the_db_and_any_change_is_drift(self):
        before = self.project_state()
        result = self.run_cli("map", "--target", self.target_id)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bd = self.db / "targets" / self.target_id / "board"
        saved = json.loads((bd / "board.json").read_text())
        self.assertEqual([n["id"] for n in saved["nodes"]], ["app", "store"])
        self.assertEqual(json.loads((bd / "state.json").read_text())["state"], "ready")
        self.assertEqual(self.project_state(), before, "the mapper must not change the project")
        self.assertEqual(board.check(self.target)["action"], "none")
        (self.project / "src" / "store.py").write_text("# changed\n")
        info = board.check(self.target)
        self.assertEqual(info["action"], "refresh")
        self.assertEqual(info["drift"]["changed"], ["src/store.py"])
        self.assertEqual(info["affected"], ["store"])
        (self.project / "src" / "new.py").write_text("# new\n")
        self.assertIn("src/new.py", board.check(self.target)["unowned_new"])

    def test_failed_mapping_keeps_state_and_releases_lock(self):
        env = dict(self.env, HARNESS_BOARD_MAPPER_CMD=json.dumps([sys.executable, "-c", "print('no json here')"]))
        result = self.run_cli("map", "--target", self.target_id, env=env)
        self.assertEqual(result.returncode, 1)
        bd = self.db / "targets" / self.target_id / "board"
        self.assertEqual(json.loads((bd / "state.json").read_text())["state"], "failed")
        self.assertFalse((bd / "mapper.lock").exists())
        self.assertFalse((bd / "board.json").exists())

    def test_live_lock_blocks_a_second_mapper(self):
        bd = self.db / "targets" / self.target_id / "board"
        bd.mkdir(parents=True)
        (bd / "mapper.lock").write_text(f"{os.getpid()} now\n")
        self.assertEqual(board.check(self.target)["reason"], "mapper running")
        self.assertEqual(self.run_cli("map", "--target", self.target_id).returncode, 1)

    def test_spawn_is_refused_inside_a_mapper_session(self):
        os.environ["HARNESS_BOARD_MAPPER"] = "1"
        try:
            self.assertIsNone(board.spawn(self.target))
        finally:
            del os.environ["HARNESS_BOARD_MAPPER"]

    def test_validate_rejects_layout_errors_and_drops_missing_paths(self):
        bad = json.loads(json.dumps(SMALL_BOARD))
        bad["nodes"][1]["col"] = 0  # shares a cell with app
        bad["nodes"][0]["files"].append({"path": "src/missing.py", "note": ""})
        bad["nodes"][0]["icon"] = "nonsense"
        bad["frames"].append({"id": "other", "title": "Other", "col": 1, "row": 0, "cols": 1, "rows": 1})
        clean, errors, warnings = board.validate(bad, str(self.project))
        self.assertTrue(any("share cell" in e for e in errors))
        self.assertTrue(any("overlap" in e for e in errors))
        self.assertTrue(any("missing.py" in w for w in warnings))
        self.assertEqual(clean["nodes"][0]["icon"], "dot")
        escape = json.loads(json.dumps(SMALL_BOARD))
        escape["nodes"][0]["files"] = [{"path": "../../etc/passwd", "note": ""}]
        clean, errors, warnings = board.validate(escape, str(self.project))
        self.assertEqual(clean["nodes"][0]["files"], [])

    def test_extract_json_handles_nested_and_fenced_answers(self):
        self.assertEqual(board.extract_json('```json\n{"board": {"nodes": [1]}}\n```')["nodes"], [1])
        self.assertEqual(board.extract_json('text {"cards": [2]} more')["nodes"], [2])

    def test_overlay_maps_uncommitted_files_to_cards(self):
        board.do_import(self.target, self.write(SMALL_BOARD))
        (self.project / "src" / "app.py").write_text("# edited\n")
        (self.project / "src" / "fresh.py").write_text("# untracked\n")
        ov = board.overlay(self.target)
        self.assertEqual(ov["edited"], {"app": ["src/app.py"]})
        self.assertIn("src/fresh.py", ov["unownedEdited"])
        self.assertIsNone(ov["heat"])

    def test_overlay_carries_the_signals_heat_and_marks_it_stale(self):
        board.do_import(self.target, self.write(SMALL_BOARD))
        signals = self.db / "targets" / self.target_id / "signals"
        signals.mkdir()
        fresh = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        heat = {"generated_at": fresh, "cards": {"app": {"heat": 1.0, "score": 3.2, "issues": 1, "top": []}}, "unmapped": []}
        (signals / "heat.json").write_text(json.dumps(heat))
        ov = json.loads(self.run_cli("overlay", "--target", self.target_id, "--json").stdout)
        self.assertEqual(ov["heat"]["cards"]["app"]["heat"], 1.0)
        self.assertFalse(ov["heat"]["stale"])
        (signals / "heat.json").write_text(json.dumps(dict(heat, generated_at="2026-01-01T00:00:00Z")))
        self.assertTrue(board.overlay(self.target)["heat"]["stale"])
        (signals / "heat.json").write_text("not json")
        self.assertIsNone(board.overlay(self.target)["heat"])

    def test_list_and_show_commands(self):
        self.assertEqual(self.run_cli("show", "--target", self.target_id).returncode, 3)
        board.do_import(self.target, self.write(SMALL_BOARD))
        rows = json.loads(self.run_cli("list", "--json").stdout)
        self.assertEqual(rows[0]["id"], self.target_id)
        self.assertEqual(rows[0]["state"], "ready")
        shown = self.run_cli("show", "--target", self.target_id)
        self.assertEqual(json.loads(shown.stdout)["title"], "Demo")
        state = json.loads(self.run_cli("state", "--target", self.target_id, "--json").stdout)
        self.assertEqual(state["check"]["action"], "none")
        self.assertEqual(self.run_cli("state", "--target", "nope", "--json").returncode, 1)

    def test_remove_hides_the_project_and_stops_mapping_until_restored(self):
        board.do_import(self.target, self.write(SMALL_BOARD))
        self.assertEqual(json.loads(self.run_cli("remove", "--target", self.target_id).stdout), {"id": self.target_id, "removed": True})
        self.assertEqual(json.loads(self.run_cli("list", "--json").stdout), [])
        self.assertEqual([r["id"] for r in json.loads(self.run_cli("list", "--json", "--removed").stdout)], [self.target_id])
        (self.project / "src" / "app.py").write_text("# changed\n")
        self.assertEqual(board.check(self.target)["reason"], "removed")
        # The stored board is kept, so a restore shows it again at once.
        self.assertEqual(self.run_cli("show", "--target", self.target_id).returncode, 0)
        self.run_cli("restore", "--target", self.target_id)
        self.assertEqual([r["id"] for r in json.loads(self.run_cli("list", "--json").stdout)], [self.target_id])
        self.assertEqual(json.loads(self.run_cli("list", "--json", "--removed").stdout), [])
        self.assertEqual(board.check(self.target)["action"], "refresh")
        self.assertEqual(self.run_cli("remove", "--target", "nope").returncode, 1)

    def test_fixture_is_a_valid_board(self):
        clean, errors, _ = board.validate(json.loads(FIXTURE.read_text()), str(self.project))
        self.assertEqual(errors, [])
        self.assertEqual(len(clean["nodes"]), 20)

    def test_session_start_hook_maps_in_the_background(self):
        payload = json.dumps({"hook_event_name": "SessionStart", "session_id": "t", "cwd": str(self.project)})
        env = dict(self.env, HARNESS_DB_ROOT=str(self.db), HARNESS_ROOT=str(ROOT), HARNESS_BOARD="1", HARNESS_AUTO_INIT="1",
                   HARNESS_JEV_CHECKPOINTS="0", HARNESS_AUDIT_ENABLED="0")
        started = time.time()
        subprocess.run(["sh", str(ROOT / "scripts" / "hooks" / "auto-init.sh")], input=payload, text=True, env=env,
                       capture_output=True, timeout=120, cwd=str(self.project))
        self.assertEqual(
            sorted(p.parent.name for p in (self.db / "targets").glob("*/target.state")),
            [self.target_id], "session startup must reuse the registered fixture target",
        )
        bd = self.db / "targets" / self.target_id / "board"
        deadline = time.time() + 60
        while time.time() < deadline and not (bd / "board.json").exists():
            time.sleep(0.5)
        self.assertTrue((bd / "board.json").exists(), (bd / "mapper.log").read_text() if (bd / "mapper.log").exists() else "no mapper log")
        self.assertLess(started, time.time())
        with_board_off = dict(env, HARNESS_BOARD="0")
        (bd / "board.json").unlink()
        (bd / "state.json").unlink()
        subprocess.run(["sh", str(ROOT / "scripts" / "hooks" / "auto-init.sh")], input=payload, text=True, env=with_board_off,
                       capture_output=True, timeout=120, cwd=str(self.project))
        time.sleep(2)
        self.assertFalse((bd / "board.json").exists(), "HARNESS_BOARD=0 must disable the trigger")

    def write(self, value):
        path = Path(self.tmp.name) / "in.json"
        path.write_text(json.dumps(value))
        return str(path)


class SessionFocusTests(unittest.TestCase):
    """Worktrees under their main folder, session links, and the per-session focus signals."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        base = Path(self.tmp.name).resolve()
        self.db = base / "db"
        self.main = base / "shop"
        (self.main / "src").mkdir(parents=True)
        for name in ("app.py", "store.py", "util.py", "web.py", "other.py"):
            (self.main / "src" / name).write_text(f"# {name}\n")
        git(self.main, "init", "-q", "-b", "main")
        git(self.main, "add", ".")
        git(self.main, "-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "init")
        self.wt = base / "shop-feature"
        git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))
        self.stale = base / "shop-old"
        git(self.main, "worktree", "add", "-q", "-b", "old", str(self.stale))
        self.env = dict(os.environ, HARNESS_DB_ROOT=str(self.db), HARNESS_WORKFLOW_STATE=str(base / "wf.sqlite"))
        self.main_id = self.register(self.main)
        self.wt_id = self.register(self.wt)
        self.stale_id = self.register(self.stale)
        board.DB_ROOT = self.db
        os.environ["HARNESS_WORKFLOW_STATE"] = self.env["HARNESS_WORKFLOW_STATE"]
        self.addCleanup(os.environ.pop, "HARNESS_WORKFLOW_STATE", None)
        cards = [{"id": cid, "frame": "core", "owns": [path], "files": [], "tests": []}
                 for cid, path in (("app", "src/app.py"), ("store", "src/store.py"), ("util", "src/util.py"), ("web", "src/web.py"))]
        board_dir = self.db / "targets" / self.main_id / "board"
        board_dir.mkdir(parents=True)
        (board_dir / "board.json").write_text(json.dumps({"default_branch": "main", "nodes": cards}))

    def register(self, path):
        out = subprocess.run(["sh", str(ROOT / "scripts" / "harness-target.sh"), "register", str(path)], env=self.env,
                             check=True, capture_output=True, text=True)
        return Path(out.stdout.strip()).name

    def cli(self, *args):
        return subprocess.run([sys.executable, str(ROOT / "scripts" / "board.py"), "--db-root", str(self.db), *args],
                              capture_output=True, text=True, env=self.env, timeout=60)

    def make_run(self, run_id, goal, deliverables, status="active", age=0):
        run = self.db / "targets" / self.wt_id / "db" / "runs" / run_id
        run.mkdir(parents=True)
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - age))
        (run / "run.json").write_text(json.dumps({"run_id": run_id, "status": status, "current_phase": "build", "updated_at": stamp}))
        (run / "task.json").write_text(json.dumps({"goal": goal, "deliverables": deliverables}))
        return run

    def point_session(self, session_id, run_id):
        from run_paths import session_key
        pointer = self.db / "targets" / self.wt_id / "db" / "runs" / "sessions" / session_key(session_id) / "current"
        pointer.parent.mkdir(parents=True)
        pointer.write_text(run_id + "\n")

    def rows(self):
        result = self.cli("list", "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        return {r["id"]: r for r in json.loads(result.stdout)}

    def test_register_records_parent_and_list_groups_worktrees(self):
        state = board.read_state(self.db / "targets" / self.wt_id / "target.state")
        self.assertEqual(state["TARGET_KIND"], "worktree")
        self.assertEqual(state["TARGET_PARENT_ROOT"], str(self.main))
        self.assertEqual(board.id_for_root(str(self.main)), self.main_id)
        self.make_run("r1", "Add billing", ["src/app.py"])
        started = json.loads(self.cli("session", "--target", str(self.wt), "--session-id", "sess-a").stdout)
        rows = self.rows()
        self.assertEqual(rows[self.main_id]["kind"], "repo")
        self.assertIsNone(rows[self.main_id]["parent"])
        row = rows[self.wt_id]
        self.assertEqual((row["kind"], row["parent"], row["branch"]), ("worktree", self.main_id, "feat"))
        self.assertEqual(row["task"]["title"], "Add billing")
        self.assertEqual(row["task"]["run_id"], "r1")
        self.assertTrue(row["task"]["live"])
        self.assertEqual(row["session_key"], started["key"])
        self.assertEqual(set(row["owner"]), {"user", "login"})
        self.assertNotIn(self.stale_id, rows, "a worktree with no session and no recent run is not listed")

    def test_parent_is_listed_and_old_state_is_computed_then_cached(self):
        state = self.db / "targets" / self.wt_id / "target.state"
        state.write_text("".join(l for l in state.read_text().splitlines(True) if not l.startswith("TARGET_PARENT_ROOT")))
        self.make_run("r1", "Task", [], status="done", age=40 * 60)
        rows = self.rows()
        self.assertIn(self.main_id, rows)
        self.assertEqual(rows[self.wt_id]["parent"], self.main_id)
        self.assertFalse(rows[self.wt_id]["task"]["live"])
        self.assertTrue((self.db / "targets" / self.wt_id / "board" / "parent.json").exists())
        self.assertNotIn("TARGET_PARENT_ROOT", state.read_text())

    def test_session_is_idempotent_and_has_a_url(self):
        self.env = dict(self.env, HARNESS_AUDIT_URL="http://audit.test:9/")
        first = json.loads(self.cli("session", "--target", str(self.wt), "--session-id", "sess-a").stdout)
        again = json.loads(self.cli("session", "--target", str(self.wt), "--session-id", "sess-a").stdout)
        other = json.loads(self.cli("session", "--target", str(self.wt), "--session-id", "sess-b").stdout)
        self.assertEqual(first, again)
        self.assertNotEqual(first["key"], other["key"])
        self.assertEqual(first["url"], f"http://audit.test:9/projects/{self.wt_id}/sessions/{first['key']}")
        record = json.loads((self.db / "targets" / self.wt_id / "board" / "sessions" / f"{first['key']}.json").read_text())
        self.assertEqual((record["session_id"], record["target"], record["root"]), ("sess-a", self.wt_id, str(self.wt)))
        self.assertEqual(set(record["owner"]), {"user", "login"})

    def test_focus_signals(self):
        wt = self.wt
        (wt / "src" / "app.py").write_text("# committed on the branch\n")
        git(wt, "add", ".")
        git(wt, "-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "branch work")
        (wt / "src" / "store.py").write_text("# uncommitted\n")
        (wt / "src" / "brand_new.py").write_text("# untracked\n")
        self.make_run("mine", "Billing", ["src/util.py"])
        newest = self.make_run("zz-newest", "Other task", ["src/web.py"])
        os.utime(newest / "run.json", (time.time() + 60, time.time() + 60))
        self.point_session("sess-a", "mine")
        con = sqlite3.connect(self.env["HARNESS_WORKFLOW_STATE"])
        con.execute("CREATE TABLE touched_files (session_id TEXT, path TEXT, tool TEXT, at TEXT, PRIMARY KEY(session_id, path))")
        for path in (wt / "src" / "web.py", wt / "src" / "app.py", wt / "src" / "util.py", wt / "src" / "other.py", Path("/elsewhere/x.py")):
            con.execute("INSERT INTO touched_files VALUES ('sess-a', ?, 'Read', 'now')", (str(path),))
        con.execute("INSERT INTO touched_files VALUES ('sess-b', ?, 'Read', 'now')", (str(wt / "src" / "store.py"),))
        con.commit()
        con.close()
        key = json.loads(self.cli("session", "--target", str(wt), "--session-id", "sess-a").stdout)["key"]
        result = self.cli("focus", "--target", self.wt_id, "--session", key)
        self.assertEqual(result.returncode, 0, result.stderr)
        view = json.loads(result.stdout)
        self.assertEqual((view["target"], view["board_target"], view["parent"]), (self.wt_id, self.main_id, self.main_id))
        self.assertEqual((view["session"]["branch"], view["session"]["base"], view["session"]["run_id"]), ("feat", "main", "mine"))
        self.assertEqual(view["session"]["title"], "Billing")
        self.assertEqual(view["signals"]["app"], {"edited": ["src/app.py"], "scope": [], "touched": []})
        self.assertEqual(view["signals"]["store"]["edited"], ["src/store.py"])
        self.assertEqual(view["signals"]["store"]["touched"], [])
        self.assertEqual(view["signals"]["util"]["scope"], ["src/util.py"])
        self.assertEqual(view["signals"]["web"], {"edited": [], "scope": [], "touched": ["src/web.py"]})
        self.assertEqual(view["unmapped"], ["src/brand_new.py", "src/other.py"])
        self.assertIn("changed 2 parts", view["summary"])

    def test_audit_records_touched_files_with_absolute_paths(self):
        import workflow_audit
        db = sqlite3.connect(":memory:")
        workflow_audit.touched_files(db, "s", "Read", {"file_path": "/a/b.py"}, "/cwd")
        workflow_audit.touched_files(db, "s", "Edit", {"file_path": "rel/c.py"}, "/cwd")
        workflow_audit.touched_files(db, "s", "Read", {"file_path": "/a/b.py"}, "/cwd")
        patch = "*** Begin Patch\n*** Update File: d.py\n*** End Patch"
        workflow_audit.touched_files(db, "s", "apply_patch", {"input": patch}, "/cwd")
        workflow_audit.touched_files(db, "s", "Bash", {"command": "ls"}, "/cwd")
        rows = sorted(r[0] for r in db.execute("SELECT path FROM touched_files WHERE session_id='s'"))
        self.assertEqual(rows, ["/a/b.py", "/cwd/d.py", "/cwd/rel/c.py"])

    def test_unknown_session_exits_3(self):
        result = self.cli("focus", "--target", self.wt_id, "--session", "nope")
        self.assertEqual(result.returncode, 3)
        self.assertEqual(json.loads(result.stdout), {"error": "unknown session"})


class ChangedPathsTests(unittest.TestCase):
    def test_stale_local_default_branch_uses_newer_origin_base(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp).resolve()
            ident = ["-c", "user.email=t@example.com", "-c", "user.name=t"]
            subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(base / "origin.git")], check=True)
            work = base / "work"
            work.mkdir()
            git(work, "init", "-q", "-b", "main")
            (work / "a.txt").write_text("a\n")
            git(work, "add", ".")
            git(work, *ident, "commit", "-qm", "init")
            git(work, "remote", "add", "origin", str(base / "origin.git"))
            git(work, "push", "-q", "origin", "main")
            (work / "upstream.txt").write_text("u\n")
            git(work, "add", ".")
            git(work, *ident, "commit", "-qm", "advance origin")
            git(work, "push", "-q", "origin", "main")
            git(work, "branch", "feat")
            git(work, "reset", "-q", "--hard", "HEAD~1")
            git(work, "fetch", "-q", "origin")
            git(work, "checkout", "-q", "feat")
            (work / "own.txt").write_text("o\n")
            git(work, "add", ".")
            git(work, *ident, "commit", "-qm", "own work")
            self.assertEqual(board.changed_paths(str(work), "main"), {"own.txt"})


if __name__ == "__main__":
    unittest.main()
