"""Architecture board: drift check, validation, mapper run with a fake agent, overlay, and the session-start trigger."""
import json
import os
from pathlib import Path
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
        tdir = self.db / "targets" / "demo-1"
        tdir.mkdir(parents=True)
        (tdir / "target.state").write_text(f"TARGET_ID=demo-1\nTARGET_ROOT={self.project}\nLAST_SEEN_AT=2026-10-19T00:00:00Z\n")
        board.DB_ROOT = self.db
        self.target = board.resolve_target("demo-1")
        self.agent = base / "agent.py"
        self.agent.write_text(FAKE_AGENT)
        self.env = dict(os.environ, HARNESS_BOARD_MAPPER_CMD=json.dumps([sys.executable, str(self.agent)]))
        self.env.pop("HARNESS_BOARD_MAPPER", None)

    def tearDown(self):
        self.tmp.cleanup()
        if self.saved_switch is not None:
            os.environ["HARNESS_BOARD"] = self.saved_switch

    def project_state(self):
        return subprocess.run(["git", "-C", str(self.project), "status", "--porcelain", "--ignored"], capture_output=True, text=True).stdout

    def run_cli(self, *args, env=None):
        return subprocess.run([sys.executable, str(ROOT / "scripts" / "board.py"), "--db-root", str(self.db), *args],
                              capture_output=True, text=True, env=env or self.env, timeout=60)

    def test_resolve_by_path_inside_target(self):
        self.assertEqual(board.resolve_target(str(self.project / "src"))["TARGET_ID"], "demo-1")

    def test_check_without_board_asks_for_full_mapping(self):
        info = board.check(self.target)
        self.assertEqual(info["action"], "map_full")
        self.assertTrue((self.db / "targets" / "demo-1" / "board" / "check.json").exists())

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
        result = self.run_cli("map", "--target", "demo-1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bd = self.db / "targets" / "demo-1" / "board"
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
        result = self.run_cli("map", "--target", "demo-1", env=env)
        self.assertEqual(result.returncode, 1)
        bd = self.db / "targets" / "demo-1" / "board"
        self.assertEqual(json.loads((bd / "state.json").read_text())["state"], "failed")
        self.assertFalse((bd / "mapper.lock").exists())
        self.assertFalse((bd / "board.json").exists())

    def test_live_lock_blocks_a_second_mapper(self):
        bd = self.db / "targets" / "demo-1" / "board"
        bd.mkdir(parents=True)
        (bd / "mapper.lock").write_text(f"{os.getpid()} now\n")
        self.assertEqual(board.check(self.target)["reason"], "mapper running")
        self.assertEqual(self.run_cli("map", "--target", "demo-1").returncode, 1)

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

    def test_list_and_show_commands(self):
        self.assertEqual(self.run_cli("show", "--target", "demo-1").returncode, 3)
        board.do_import(self.target, self.write(SMALL_BOARD))
        rows = json.loads(self.run_cli("list", "--json").stdout)
        self.assertEqual(rows[0]["id"], "demo-1")
        self.assertEqual(rows[0]["state"], "ready")
        shown = self.run_cli("show", "--target", "demo-1")
        self.assertEqual(json.loads(shown.stdout)["title"], "Demo")
        state = json.loads(self.run_cli("state", "--target", "demo-1", "--json").stdout)
        self.assertEqual(state["check"]["action"], "none")
        self.assertEqual(self.run_cli("state", "--target", "nope", "--json").returncode, 1)

    def test_remove_hides_the_project_and_stops_mapping_until_restored(self):
        board.do_import(self.target, self.write(SMALL_BOARD))
        self.assertEqual(json.loads(self.run_cli("remove", "--target", "demo-1").stdout), {"id": "demo-1", "removed": True})
        self.assertEqual(json.loads(self.run_cli("list", "--json").stdout), [])
        self.assertEqual([r["id"] for r in json.loads(self.run_cli("list", "--json", "--removed").stdout)], ["demo-1"])
        (self.project / "src" / "app.py").write_text("# changed\n")
        self.assertEqual(board.check(self.target)["reason"], "removed")
        # The stored board is kept, so a restore shows it again at once.
        self.assertEqual(self.run_cli("show", "--target", "demo-1").returncode, 0)
        self.run_cli("restore", "--target", "demo-1")
        self.assertEqual([r["id"] for r in json.loads(self.run_cli("list", "--json").stdout)], ["demo-1"])
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
        bd = self.db / "targets" / "demo-1" / "board"
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


if __name__ == "__main__":
    unittest.main()
