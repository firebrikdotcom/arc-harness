#!/usr/bin/env python3
"""Delegated task-manager runs: every state and gate, the draft pull request, and
the never-merge rule. Offline: tests/fake_agent.py stands in for the dispatcher,
a generated script for gh, and git works on a temporary clone of a bare origin."""
from __future__ import annotations

import contextlib
import copy
import fcntl
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
TESTS = ROOT / "tests"
sys.path.insert(0, str(ROOT / "scripts"))

sys.path.insert(0, str(TESTS))

import fake_agent  # noqa: E402
import harness_config  # noqa: E402
import task_managers as tm  # noqa: E402
import task_managers_runs as runs  # noqa: E402

FIXTURE = TESTS / "fixtures" / "task-managers" / "injection.json"
# Words that occur only in the fixture item's title, description, project, list or assignee.
ITEM_WORDS = ("Fix export", "ignore previous", "IGNORE PREVIOUS", "export job", "nightly", "café", "Zebra", "Quagga", "Mallory")
GH = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["FAKE_GH_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
print("https://github.com/example/demo/pull/7")
"""
GIT_ENV = {"GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0"}


def git(where: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(where), *args], check=True, capture_output=True, text=True,
                          env={**os.environ, **GIT_ENV}, timeout=60).stdout


class Base(unittest.TestCase):
    VERIFY: str | None = None   # body of scripts/verify.sh in the repository, when it has one
    SYNC = True

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="harness-task-runs.")).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.db = self.tmp / "db"
        self.config_home = self.tmp / "config"
        self.config_home.mkdir()
        self.agent_state = self.tmp / "agent"
        self.gh_log = self.tmp / "gh.log"
        self.item = json.loads(FIXTURE.read_text(encoding="utf-8"))
        self.make_repo()
        gh = self.tmp / "fake-gh"
        gh.write_text(GH)
        self.write_harness_config(enabled=True)
        (self.config_home / "task-managers.json").write_text(json.dumps({
            "version": 1, "owner_login": None, "providers": {}, "repos": {"demo": str(self.repo)}, "mappings": {},
            "run": {"plan_profile": "tm_plan", "build_profile": "tm_build", "review_profile": "tm_review", "base": "origin/HEAD"}}))
        env = {**GIT_ENV, "HARNESS_CONFIG_HOME": str(self.config_home), "FAKE_AGENT_STATE": str(self.agent_state),
               "FAKE_GH_LOG": str(self.gh_log), "HARNESS_TASKS_GH": f"{sys.executable} {gh}",
               "HARNESS_TASKS_DISPATCH": f"{sys.executable} {TESTS / 'fake_agent.py'}", "HARNESS_TASKS_SYNC": "1" if self.SYNC else "0"}
        patcher = patch.dict(os.environ, env)
        patcher.start()
        self.addCleanup(patcher.stop)
        for name in ("HARNESS_CONFIG", "HARNESS_AUDIT_SETTINGS", "FAKE_AGENT_ARTIFACTS", "FAKE_AGENT_FAIL", "FAKE_AGENT_STRAY", "FAKE_AGENT_HOLD",
                     "FAKE_AGENT_WRITE", "FAKE_AGENT_GIT"):
            os.environ.pop(name, None)
        self.ctx = tm.Ctx(self.db, tm.load_config())
        self.ctx.find_item = lambda item_id, refresh=False: dict(self.item)

    def make_repo(self):
        source = self.tmp / "source"
        source.mkdir()
        git(source, "init", "-q", "-b", "main")
        (source / "README.md").write_text("demo\n")
        if self.VERIFY is not None:
            (source / "scripts").mkdir()
            (source / "scripts" / "verify.sh").write_text("#!/bin/sh\n" + self.VERIFY + "\n")
            (source / "scripts" / "verify.sh").chmod(0o755)
        git(source, "add", "-A")
        git(source, "-c", "user.name=Seed", "-c", "user.email=seed@localhost", "commit", "-q", "-m", "seed")
        self.origin = self.tmp / "origin.git"
        subprocess.run(["git", "clone", "-q", "--bare", str(source), str(self.origin)], check=True, capture_output=True, env={**os.environ, **GIT_ENV})
        self.repo = self.tmp / "demo"
        subprocess.run(["git", "clone", "-q", str(self.origin), str(self.repo)], check=True, capture_output=True, env={**os.environ, **GIT_ENV})
        git(self.repo, "config", "user.name", "Run Tester")
        git(self.repo, "config", "user.email", "tester@localhost")
        self.base_commit = git(self.origin, "rev-parse", "main").strip()

    def write_harness_config(self, enabled: bool, profiles=("tm_plan", "tm_build", "tm_review")):
        value = copy.deepcopy(harness_config.DEFAULTS)
        value["typesafe"]["mode"] = "off"
        value["dispatch"]["enabled"] = enabled
        roles = {"tm_plan": "planner", "tm_build": "implementer", "tm_review": "reviewer"}
        for name in profiles:
            value["dispatch"]["profiles"][name] = {"provider": "command", "model": "fake", "effort": "low", "role": roles[name], "command": ["true", "{prompt}"]}
        (self.config_home / "config.json").write_text(json.dumps(value))

    # helpers
    def run_cmd(self, *argv: str) -> tuple[int, dict]:
        """`task_managers.py run ...` through the real argument parser and this module's main."""
        out = io.StringIO()
        code = 0
        with contextlib.redirect_stdout(out):
            try:
                code = runs.main(tm.build_parser().parse_args(["run", *argv]), self.ctx)
            except SystemExit as stop:
                code = stop.code
        lines = [line for line in out.getvalue().splitlines() if line.strip()]
        self.assertEqual(len(lines), 1, out.getvalue())
        return code, json.loads(lines[0])

    def ok(self, *argv: str) -> dict:
        code, value = self.run_cmd(*argv)
        self.assertEqual(code, 0, value)
        return value

    def start(self, *extra: str) -> str:
        return self.ok("start", "--item", "9001", "--repo", "demo", "--agent", "claude", *extra)["run_id"]

    def doc(self, run_id: str) -> dict:
        return self.ok("status", "--id", run_id)

    def states(self, run_id: str) -> list[str]:
        seen = []
        for entry in self.doc(run_id)["history"]:
            if not seen or seen[-1] != entry["state"]:
                seen.append(entry["state"])
        return seen

    def calls(self) -> list[str]:
        log = self.agent_state / "calls.log"
        return log.read_text().splitlines() if log.exists() else []

    def gh_calls(self) -> list[list[str]]:
        return [json.loads(line) for line in self.gh_log.read_text().splitlines()] if self.gh_log.exists() else []

    def origin_branches(self) -> list[str]:
        return git(self.origin, "for-each-ref", "--format=%(refname:short)", "refs/heads").split()

    def wait_for(self, condition, what: str, seconds: float = 30):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if condition():
                return
            time.sleep(0.05)
        self.fail(f"timed out waiting for {what}")


class WalkTest(Base):
    def test_full_walk_stops_at_both_gates_and_never_merges(self):
        os.environ["FAKE_AGENT_STRAY"] = "1"
        run_id = self.start()
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "awaiting_plan_approval")
        self.assertEqual(doc["owned_paths"], ["src/feature.txt"])
        self.assertEqual(doc["branch"], f"tm/basecamp-9001-{run_id[:6]}")
        self.assertEqual(doc["worktree"], f"{self.repo}-tm-{run_id[:6]}")
        self.assertEqual(set(doc["dispatch_runs"]), {"plan"})
        directory = self.db / "task-managers" / "runs" / run_id
        self.assertIn("Plan:", (directory / "plan.md").read_text())
        packet = (directory / "packet.txt").read_text()
        self.assertLessEqual(len(packet.encode()), 6000)
        self.assertTrue(packet.startswith("<<<TASK-MANAGER-UNTRUSTED sha256="))
        self.assertEqual(packet.count("<<<END-TASK-MANAGER-UNTRUSTED>>>"), 1)
        self.assertIn("session=tm-" + run_id, self.calls()[0])

        # The plan gate holds: advancing again does nothing until a person approves.
        self.ok("advance", "--id", run_id)
        self.assertEqual(self.doc(run_id)["state"], "awaiting_plan_approval")
        self.assertFalse(any("build" in line for line in self.calls()))
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])
        code, value = self.run_cmd("approve", "--id", run_id, "--gate", "review")
        self.assertEqual((code, value["error"]["code"]), (2, "wrong_state"))

        self.assertEqual(self.ok("approve", "--id", run_id, "--gate", "plan")["state"], "awaiting_review_approval")
        doc = self.doc(run_id)
        self.assertEqual(self.states(run_id), ["queued", "planning", "awaiting_plan_approval", "building", "verifying", "reviewing",
                                               "opening_pr", "awaiting_review_approval"])
        self.assertEqual(doc["pr_url"], "https://github.com/example/demo/pull/7")
        self.assertEqual(set(doc["dispatch_runs"]), {"plan", "build", "review"})
        self.assertIsNone(doc["error"])
        self.assertTrue(any("no verify script" in entry["note"] for entry in doc["history"]))
        self.assertNotIn("advance", doc)

        # One draft pull request for the run's branch; nothing merged, base untouched.
        calls = self.gh_calls()
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][:3], ["pr", "create", "--draft"])
        self.assertEqual(calls[0][calls[0].index("--head") + 1], doc["branch"])
        self.assertNotIn("merge", " ".join(calls[0]).lower().replace("merged automatically", "").replace("reviews and merges", ""))
        self.assertEqual(sorted(self.origin_branches()), sorted(["main", doc["branch"]]))
        self.assertEqual(git(self.origin, "rev-parse", "main").strip(), self.base_commit)
        self.assertEqual(git(self.origin, "rev-list", "--merges", "--all").strip(), "")
        self.assertEqual(git(self.origin, "rev-list", f"main..{doc['branch']}").split(), [doc["commit"]])
        self.assertEqual(git(self.origin, "rev-list", "--parents", "-n", "1", doc["branch"]).split(), [doc["commit"], self.base_commit])
        # Only the changed owned path is committed; the stray file stays out.
        self.assertEqual(git(self.origin, "diff", "--name-only", "main", doc["branch"]).split(), ["src/feature.txt"])
        self.assertTrue((Path(doc["worktree"]) / "stray-outside-owned.txt").exists())

        # Reference and URL only: no item text in the branch, the commit or the pull request.
        message = git(self.origin, "log", "-1", "--format=%B", doc["branch"])
        self.assertTrue(message.startswith("Task basecamp #9001\n"))
        for text in (message, calls[0][calls[0].index("--body") + 1]):
            self.assertIn(self.item["url"], text)
            self.assertIn(run_id, text)
            self.assertIn("draft", text.lower())
        self.assertNotIn("Co-Authored-By", message)
        for text in (message, json.dumps(calls, ensure_ascii=False), doc["branch"], json.dumps(doc, ensure_ascii=False)):
            for word in ITEM_WORDS:
                self.assertNotIn(word, text)

        self.assertEqual(self.ok("approve", "--id", run_id, "--gate", "review")["state"], "ready_to_merge")
        self.assertEqual(self.states(run_id)[-1], "ready_to_merge")
        self.assertEqual(len(self.gh_calls()), 1)
        self.assertEqual(git(self.origin, "rev-parse", "main").strip(), self.base_commit)
        code, _ = self.run_cmd("approve", "--id", run_id, "--gate", "review")
        self.assertEqual(code, 2)

        # Everything the run wrote is private.
        base = self.db / "task-managers"
        for path in [base, *base.rglob("*")]:
            mode = stat.S_IMODE(path.stat().st_mode)
            self.assertEqual(mode, 0o700 if path.is_dir() else 0o600, path)
        listed = self.ok("list")["runs"]
        self.assertEqual([(run["id"], run["state"], run["pr_url"]) for run in listed], [(run_id, "ready_to_merge", doc["pr_url"])])
        self.assertEqual(set(listed[0]), {"id", "provider", "item_id", "repo", "agent", "state", "updated_at", "pr_url"})

    def test_no_plan_gate_goes_straight_to_the_review_gate(self):
        run_id = self.start("--no-plan-gate")
        self.assertEqual(self.states(run_id), ["queued", "planning", "building", "verifying", "reviewing", "opening_pr", "awaiting_review_approval"])
        self.assertFalse(self.doc(run_id)["plan_gate"])
        self.assertEqual(self.gh_calls()[0][:3], ["pr", "create", "--draft"])

    def test_reject_is_terminal_and_keeps_a_bounded_reason(self):
        reason = self.tmp / "reason.txt"
        reason.write_text("x" * 12_000)
        run_id = self.start()
        code, _ = self.run_cmd("reject", "--id", run_id, "--gate", "review", "--reason-file", str(reason))
        self.assertEqual(code, 2)
        self.assertEqual(self.ok("reject", "--id", run_id, "--gate", "plan", "--reason-file", str(reason))["state"], "rejected")
        doc = self.doc(run_id)
        self.assertEqual((doc["state"], doc["reject_gate"], len(doc["reject_reason"])), ("rejected", "plan", 10_000))
        for argv in (("approve", "--id", run_id, "--gate", "plan"), ("reject", "--id", run_id, "--gate", "plan", "--reason-file", str(reason)),
                     ("cancel", "--id", run_id)):
            self.assertEqual(self.run_cmd(*argv)[0], 2, argv)
        self.ok("advance", "--id", run_id)
        self.assertEqual(self.doc(run_id)["state"], "rejected")
        self.assertFalse(any("build" in line for line in self.calls()))

        second = self.start("--no-plan-gate")
        reason.write_text("not what was asked\n")
        self.ok("reject", "--id", second, "--gate", "review", "--reason-file", str(reason))
        self.assertEqual(self.doc(second)["reject_reason"], "not what was asked")
        self.assertEqual(self.run_cmd("approve", "--id", second, "--gate", "review")[0], 2)

    def test_cancel_is_terminal(self):
        run_id = self.start()
        self.assertEqual(self.ok("cancel", "--id", run_id)["state"], "cancelled")
        self.assertEqual(self.run_cmd("approve", "--id", run_id, "--gate", "plan")[0], 2)
        self.assertEqual(self.run_cmd("cancel", "--id", run_id)[0], 2)
        self.ok("advance", "--id", run_id)
        self.assertEqual(self.doc(run_id)["state"], "cancelled")
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])

    def test_a_held_advance_lock_makes_advance_a_no_op(self):
        os.environ["HARNESS_TASKS_SYNC"] = "0"
        with patch.object(runs, "spawn_advance", lambda *args: None):
            run_id = self.start()
        lock = os.open(self.db / "task-managers" / "runs" / run_id / "advance.lock", os.O_WRONLY | os.O_CREAT, 0o600)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX)
            # No holder marker: the advance waits LOCK_WAIT for the lock, then gives up without working.
            with patch.object(runs, "LOCK_WAIT", 0.3):
                began = time.monotonic()
                self.assertEqual(self.ok("advance", "--id", run_id), {"run_id": run_id, "state": "queued", "advanced": False})
                self.assertGreaterEqual(time.monotonic() - began, 0.3)
            self.assertEqual(self.calls(), [])
        finally:
            os.close(lock)
        self.assertEqual(self.ok("advance", "--id", run_id)["state"], "awaiting_plan_approval")

    def test_dashboard_invocations_and_plan_text(self):
        """The dashboard passes --provider and --json to start, and --json alone to the rest."""
        self.assertEqual(self.ok("list", "--json"), {"runs": []})
        os.environ["HARNESS_TASKS_SYNC"] = "0"
        with patch.object(runs, "spawn_advance", lambda *args: None):
            started = self.ok("start", "--provider", "basecamp", "--item", "9001", "--repo", "demo", "--agent", "codex", "--no-plan-gate", "--json")
        run_id = started["run_id"]
        self.assertEqual(started["state"], "queued")
        before = self.ok("status", "--id", run_id, "--json")
        self.assertIsNone(before["plan_text"])
        self.assertEqual((before["agent"], before["plan_gate"]), ("codex", False))
        (self.db / "task-managers" / "runs" / run_id / "plan.md").write_text("p" * 20_000)
        self.assertEqual(self.ok("status", "--id", run_id, "--json")["plan_text"], "p" * 16_000)
        self.assertEqual(self.ok("list", "--json")["runs"][0]["id"], run_id)
        self.assertEqual(self.ok("cancel", "--id", run_id, "--json")["state"], "cancelled")

        os.environ["HARNESS_TASKS_SYNC"] = "1"
        second = self.start()
        self.assertEqual(self.ok("status", "--id", second, "--json")["plan_text"], fake_agent.PLAN_TEXT + "\n")
        reason = self.tmp / "why.txt"
        reason.write_text("no")
        self.assertEqual(self.ok("reject", "--id", second, "--gate", "plan", "--reason-file", str(reason), "--json")["state"], "rejected")
        third = self.start()
        self.assertEqual(self.ok("approve", "--id", third, "--gate", "plan", "--json")["state"], "awaiting_review_approval")

    def test_unknown_run_is_not_found(self):
        code, value = self.run_cmd("status", "--id", "0" * 32)
        self.assertEqual((code, value["error"]["code"]), (4, "not_found"))


class RefusalTest(Base):
    def test_start_refusals(self):
        code, value = self.run_cmd("start", "--item", "9001", "--repo", "elsewhere", "--agent", "claude")
        self.assertEqual((code, value["error"]["code"]), (2, "unknown_repo"))
        code, value = self.run_cmd("start", "--item", "9001", "--repo", "demo", "--agent", "gemini")
        self.assertEqual(code, 2)
        with self.assertRaises(runs.RunError) as raised:
            runs.start(self.db, self.ctx.config, self.ctx.find_item, "9001", "demo", "gemini")
        self.assertEqual(raised.exception.exit_code, 2)

        self.write_harness_config(enabled=True, profiles=("tm_plan", "tm_build"))
        code, value = self.run_cmd("start", "--item", "9001", "--repo", "demo", "--agent", "codex")
        self.assertEqual((code, value["error"]["code"]), (3, "profile_missing"))
        self.assertIn("tm_review", value["error"]["message"])

        self.write_harness_config(enabled=False)
        code, value = self.run_cmd("start", "--item", "9001", "--repo", "demo", "--agent", "claude")
        self.assertEqual((code, value["error"]["code"]), (3, "dispatch_disabled"))
        self.assertIn("dispatch.enabled", value["error"]["message"])
        self.assertEqual(self.ok("list")["runs"], [])
        self.assertEqual(self.calls(), [])

    def test_exit_codes_through_the_command_line(self):
        self.write_harness_config(enabled=False)
        began = time.monotonic()
        done = subprocess.run([sys.executable, str(ROOT / "scripts" / "task_managers.py"), "--db-root", str(self.db), "run", "list", "--json"],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual((done.returncode, json.loads(done.stdout)), (0, {"runs": []}), done.stderr)
        self.assertLess(time.monotonic() - began, 5)
        base = [sys.executable, str(ROOT / "scripts" / "task_managers.py"), "--db-root", str(self.db), "run", "start", "--item", "9001"]
        for extra, expect in ((["--repo", "demo", "--agent", "claude"], 3), (["--repo", "elsewhere", "--agent", "claude"], 2),
                              (["--repo", "demo", "--agent", "gemini"], 2)):
            done = subprocess.run(base + extra, capture_output=True, text=True, timeout=60)
            self.assertEqual(done.returncode, expect, done.stdout + done.stderr)
            self.assertIn("error", json.loads(done.stdout))

    def test_invalid_proposed_owned_paths_fail_the_run(self):
        for proposed in (["../x"], ["/etc/x"], [".git/config"], ["src/ok.txt", "a/../../b"], [], "src", ["x"] * 0 + [str(n) for n in range(51)]):
            os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps(proposed)
            run_id = self.start("--no-plan-gate")
            doc = self.doc(run_id)
            self.assertEqual(doc["state"], "failed", proposed)
            self.assertIn("owned path", doc["error"])
            self.assertEqual(doc["owned_paths"], [])
        self.assertFalse(any("build" in line for line in self.calls()))
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])
        self.assertEqual(runs.valid_owned_paths(["src/", "./docs/a.md", "src"]), ["src", "docs/a.md"])
        for bad in (["~/x"], ["-rf"], [":(glob)**"], ["a\nb"], ["sub/.GIT/hooks"], [3]):
            with self.assertRaises(runs.StepError):
                runs.valid_owned_paths(bad)

    def test_a_failing_review_fails_the_run_and_keeps_its_evidence(self):
        os.environ["FAKE_AGENT_FAIL"] = "review"
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "failed")
        self.assertIn("REVIEW-FINDING", doc["review_evidence"])
        self.assertEqual(self.states(run_id)[-2:], ["reviewing", "failed"])
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])
        self.assertEqual(self.run_cmd("approve", "--id", run_id, "--gate", "review")[0], 2)

    def test_a_failing_step_error_is_bounded_and_carries_no_credential(self):
        git(self.repo, "remote", "set-url", "origin", "https://user:hunter2-secret@127.0.0.1:1/none.git")
        run_id = self.start()
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "failed")
        self.assertIn("git fetch", doc["error"])
        self.assertLessEqual(len(doc["error"]), 500)
        directory = self.db / "task-managers" / "runs" / run_id
        for text in (json.dumps(doc), (directory / "steps.log").read_text()):
            self.assertNotIn("hunter2", text)


class VerifyPassTest(Base):
    VERIFY = 'test "$1" = --project && test -f "$2/src/feature.txt"'

    def test_the_verify_script_runs_on_the_worktree(self):
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "awaiting_review_approval", doc["error"])
        self.assertTrue(any(entry["note"] == "scripts/verify.sh passed" for entry in doc["history"]))


class VerifyFailTest(Base):
    VERIFY = "echo broken >&2; exit 1"

    def test_a_failing_verify_script_fails_the_run(self):
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "failed")
        self.assertEqual(self.states(run_id)[-2:], ["verifying", "failed"])
        self.assertIn("verify.sh failed", doc["error"])
        self.assertFalse(any(line.startswith("create review") for line in self.calls()))
        self.assertEqual(self.gh_calls(), [])


class DetachedTest(Base):
    SYNC = False

    def test_start_detaches_and_a_second_advance_is_a_no_op(self):
        hold = self.tmp / "hold"
        hold.write_text("")
        os.environ["FAKE_AGENT_HOLD"] = str(hold)
        run_id = self.start()
        self.assertIn(self.doc(run_id)["state"], ("queued", "planning"))
        self.wait_for(lambda: "run plan" in self.calls(), "the detached advance to reach the planning worker")
        began = time.monotonic()
        self.assertEqual(self.ok("advance", "--id", run_id)["advanced"], False)
        self.assertLess(time.monotonic() - began, 5, "a working holder makes the second advance return at once")
        live = self.doc(run_id)["advance"]
        self.assertNotEqual(live["pid"], os.getpid())
        hold.unlink()
        self.wait_for(lambda: self.doc(run_id)["state"] == "awaiting_plan_approval", "the plan gate")
        self.assertEqual([line for line in self.calls() if line.startswith("create")], [f"create plan read session=tm-{run_id}"])
        log = self.db / "task-managers" / "runs" / run_id / "advance.log"
        self.assertEqual(stat.S_IMODE(log.stat().st_mode), 0o600)

        # Approval restarts the detached advance, which stops at the review gate.
        self.ok("approve", "--id", run_id, "--gate", "plan")
        self.wait_for(lambda: self.doc(run_id)["state"] == "awaiting_review_approval", "the review gate")
        self.assertEqual(git(self.origin, "rev-parse", "main").strip(), self.base_commit)

    def test_cancel_stops_a_live_advance(self):
        hold = self.tmp / "hold"
        hold.write_text("")
        os.environ["FAKE_AGENT_HOLD"] = str(hold)
        run_id = self.start()
        self.wait_for(lambda: "run plan" in self.calls(), "the detached advance to reach the planning worker")
        pid = self.doc(run_id)["advance"]["pid"]
        self.assertEqual(self.ok("cancel", "--id", run_id)["state"], "cancelled")
        self.wait_for(lambda: runs.process_start(pid) is None, "the advance process to stop")
        hold.unlink()
        time.sleep(0.3)
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "cancelled")
        self.assertNotIn("advance", doc)
        self.assertFalse(any(line.startswith("create build") for line in self.calls()))


class ReviewFixTest(Base):
    """Findings of the independent review: the index, renames, sensitive paths, duplicate runs,
    the approve race, the pull request's repository, and what `run status` shows at the gates."""

    def run_dir(self, run_id: str) -> Path:
        return self.db / "task-managers" / "runs" / run_id

    def test_a_file_staged_outside_the_owned_paths_fails_the_run(self):
        os.environ["FAKE_AGENT_STRAY"] = "1"
        os.environ["FAKE_AGENT_GIT"] = json.dumps([["add", fake_agent.STRAY]])
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "failed")
        self.assertEqual(self.states(run_id)[-2:], ["opening_pr", "failed"])
        self.assertIn("1 unexpected staged path(s)", doc["error"])
        self.assertNotIn(fake_agent.STRAY, doc["error"])
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])
        self.assertEqual(git(Path(doc["worktree"]), "rev-parse", "HEAD").strip(), self.base_commit)
        self.assertIsNone(doc["pr_url"])

    def test_the_commit_holds_only_the_changed_owned_paths(self):
        """Explicit pathspec: the commit command itself names the paths, whatever the index holds."""
        real = runs.git

        def staging(db_root, run_id, label, where, *args, **kwargs):
            if label == "git commit":   # something stages a file after the index check
                (Path(where) / "late.txt").write_text("late\n")
                git(Path(where), "add", "late.txt")
            return real(db_root, run_id, label, where, *args, **kwargs)
        with patch.object(runs, "git", staging):
            run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "awaiting_review_approval", doc["error"])
        self.assertEqual(git(self.origin, "diff", "--name-only", "main", doc["branch"]).split(), ["src/feature.txt"])

    def test_a_staged_rename_inside_the_owned_paths_is_committed_whole(self):
        self.assertEqual(runs.porcelain_paths("R  new name.txt\0old.txt\0 M a/b.txt\0?? c\0C  copy.txt\0source.txt\0"),
                         ["a/b.txt", "c", "copy.txt", "new name.txt"])
        os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps(["src/feature.txt", "README.md", "MOVED.md"])
        os.environ["FAKE_AGENT_GIT"] = json.dumps([["mv", "README.md", "MOVED.md"]])
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "awaiting_review_approval", doc["error"])
        changes = git(self.origin, "diff", "--name-status", "--no-renames", "main", doc["branch"]).splitlines()
        self.assertEqual(sorted(changes), ["A\tMOVED.md", "A\tsrc/feature.txt", "D\tREADME.md"])
        self.assertEqual(git(Path(doc["worktree"]), "status", "--porcelain").strip(), "")

    def test_sensitive_proposed_owned_paths_fail_the_run(self):
        for proposed in (".github/workflows/ci.yml", ".GitHub", ".githooks/pre-commit", ".gitmodules", ".gitattributes", "docs/.gitattributes",
                         "scripts/verify.sh", "Scripts/Verify.SH", ".claude/settings.json", ".codex", ".harness-db/x"):
            os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps(["src/feature.txt", proposed])
            run_id = self.start("--no-plan-gate")
            doc = self.doc(run_id)
            self.assertEqual(doc["state"], "failed", proposed)
            self.assertIn("sensitive path: " + proposed, doc["error"])
            self.assertEqual(doc["owned_paths"], [])
        for whole in (".", "./", ""):
            os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps([whole])
            self.assertEqual(self.doc(self.start("--no-plan-gate"))["state"], "failed", whole)
        self.assertFalse(any("build" in line for line in self.calls()))
        self.assertEqual(self.gh_calls(), [])
        for fine in ("scripts", "scripts/verify.sh.md", "src/github/x", "docs/claude.md"):
            self.assertFalse(runs.is_sensitive(fine), fine)

    def test_allow_sensitive_paths_lets_the_owner_opt_in(self):
        self.ctx.config["run"]["allow_sensitive_paths"] = True
        os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps([".github/workflows/ci.yml"])
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "awaiting_review_approval", doc["error"])
        self.assertEqual(git(self.origin, "diff", "--name-only", "main", doc["branch"]).split(), [".github/workflows/ci.yml"])

    def test_a_second_run_for_the_same_item_is_refused_while_one_is_active(self):
        first = self.start()
        code, value = self.run_cmd("start", "--item", "9001", "--repo", "demo", "--agent", "codex")
        self.assertEqual((code, value["error"]["code"]), (2, "run_active"))
        self.assertIn(first, value["error"]["message"])
        self.assertEqual([run["id"] for run in self.ok("list")["runs"]], [first])
        self.ok("cancel", "--id", first)
        second = self.start("--no-plan-gate")
        self.assertEqual(self.doc(second)["state"], "awaiting_review_approval")
        self.assertEqual(self.run_cmd("start", "--item", "9001", "--repo", "demo", "--agent", "claude")[1]["error"]["code"], "run_active")
        self.ok("approve", "--id", second, "--gate", "review")   # ready_to_merge: the run is over
        os.environ["HARNESS_TASKS_SYNC"] = "0"
        with patch.object(runs, "spawn_advance", lambda *args: None):
            third = self.start()
            self.assertEqual(self.doc(third)["state"], "queued")
            self.item["id"] = 9002                                # another item is never blocked
            self.assertEqual(self.ok("start", "--item", "9002", "--repo", "demo", "--agent", "claude")["state"], "queued")

    def test_concurrent_starts_create_one_run(self):
        outcomes: list[str] = []
        gate = threading.Barrier(8)

        def one():
            gate.wait(timeout=30)
            try:
                runs.start(self.db, self.ctx.config, self.ctx.find_item, "9001", "demo", "claude")
                outcomes.append("started")
            except runs.RunError as error:
                outcomes.append(error.code)
        with patch.object(runs, "spawn_advance", lambda *args: None):
            threads = [threading.Thread(target=one) for _ in range(8)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(60)
        self.assertEqual(sorted(outcomes), ["run_active"] * 7 + ["started"])
        self.assertEqual(len(self.ok("list")["runs"]), 1)

    def test_an_approve_while_the_planning_advance_still_holds_the_lock_is_not_stranded(self):
        """The planning advance wrote the gate state and dropped its marker but has not released
        advance.lock yet: the advance the approval starts waits for the lock instead of leaving."""
        run_id = self.start()
        self.assertEqual(self.doc(run_id)["state"], "awaiting_plan_approval")
        lock = os.open(self.run_dir(run_id) / "advance.lock", os.O_WRONLY | os.O_CREAT, 0o600)
        fcntl.flock(lock, fcntl.LOCK_EX)
        release = threading.Timer(0.6, os.close, [lock])
        release.start()
        self.addCleanup(release.join)
        began = time.monotonic()
        self.assertEqual(self.ok("approve", "--id", run_id, "--gate", "plan")["state"], "awaiting_review_approval")
        self.assertGreaterEqual(time.monotonic() - began, 0.5)
        self.assertEqual(self.states(run_id)[-5:], ["building", "verifying", "reviewing", "opening_pr", "awaiting_review_approval"])

    def test_an_approve_while_the_holder_still_shows_its_marker_is_carried_on_by_the_holder(self):
        """The other half: the approval's own advance saw the holder's marker and left; the
        holder looks again after dropping the marker and builds."""
        run_id = self.start()
        real, seen = runs._advance, []

        def interrupted(db_root, rid, config):
            real(db_root, rid, config)
            if not seen:   # the approval lands while this advance still holds the lock and its marker
                seen.append(self.doc(run_id)["advance"]["pid"])
                with patch.dict(os.environ, {"HARNESS_TASKS_SYNC": "0"}), patch.object(runs, "spawn_advance", lambda *args: None):
                    self.ok("approve", "--id", run_id, "--gate", "plan")
                # The approval's advance: the marker makes it leave at once, well inside LOCK_WAIT.
                second = threading.Thread(target=lambda: seen.append(runs.advance(db_root, rid, config)["advanced"]))
                second.start()
                second.join(5)
                seen.append(second.is_alive())
        with patch.object(runs, "_advance", interrupted):
            self.assertEqual(self.ok("advance", "--id", run_id), {"run_id": run_id, "state": "awaiting_review_approval", "advanced": True})
        self.assertEqual(seen, [os.getpid(), False, False])
        self.assertEqual(len([line for line in self.calls() if line.startswith("create build")]), 1)

    def test_the_pull_request_names_the_origin_repository(self):
        for url, expect in (("https://github.com/acme/demo.git", "acme/demo"), ("https://user:tok@github.com/acme/demo", "acme/demo"),
                            ("git@github.com:acme/de.mo.git", "acme/de.mo"), ("ssh://git@github.com:22/acme/demo.git/", "acme/demo"),
                            ("https://gitlab.com/acme/demo.git", None), ("https://github.com.evil.example/acme/demo", None),
                            ("https://github.com/acme/demo/extra", None), ("/srv/git/github.com/acme/demo.git", None),
                            ("https://github.com/acme/..", None)):
            match = runs.GITHUB_REMOTE.fullmatch(url)
            self.assertEqual(f"{match.group(1)}/{match.group(2)}" if match and match.group(2) not in (".", "..") else None, expect, url)
        url = "https://bot:hunter2-secret@github.com/acme/demo.git"
        git(self.repo, "config", f"url.{self.origin}.insteadOf", url)
        git(self.repo, "remote", "set-url", "origin", url)
        git(self.repo, "remote", "add", "upstream", "https://github.com/upstream-org/demo.git")
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "awaiting_review_approval", doc["error"])
        call = self.gh_calls()[0]
        self.assertEqual(call[call.index("--repo") + 1], "acme/demo")
        for text in (self.gh_log.read_text(), json.dumps(doc), (self.run_dir(run_id) / "steps.log").read_text()):
            self.assertNotIn("hunter2", text)
            self.assertNotIn("upstream-org", text)

    def test_a_local_origin_gets_no_repo_argument(self):
        self.start("--no-plan-gate")
        self.assertNotIn("--repo", self.gh_calls()[0])

    def test_status_shows_the_owned_paths_and_the_review_evidence_at_the_gates(self):
        run_id = self.start()
        at_plan = self.ok("status", "--id", run_id, "--json")
        self.assertEqual((at_plan["state"], at_plan["owned_paths"]), ("awaiting_plan_approval", ["src/feature.txt"]))
        self.assertTrue(at_plan["plan_text"])
        self.ok("approve", "--id", run_id, "--gate", "plan")
        at_review = self.ok("status", "--id", run_id, "--json")
        self.assertEqual((at_review["state"], at_review["owned_paths"]), ("awaiting_review_approval", ["src/feature.txt"]))
        self.assertEqual(at_review["review_evidence"], "The change matches the request.")
        done = subprocess.run([sys.executable, str(ROOT / "scripts" / "task_managers.py"), "--db-root", str(self.db), "run", "status",
                               "--id", run_id, "--json"], capture_output=True, text=True, timeout=60)
        printed = json.loads(done.stdout)
        self.assertEqual((done.returncode, printed["owned_paths"], printed["review_evidence"]),
                         (0, ["src/feature.txt"], "The change matches the request."), done.stderr)


class SensitiveChangeTest(Base):
    VERIFY = 'echo "* text" > "$2/src/.gitattributes"'

    def test_a_changed_verify_script_fails_the_run_before_it_is_executed(self):
        os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps(["scripts"])
        os.environ["FAKE_AGENT_WRITE"] = json.dumps(["scripts/verify.sh"])
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "failed")
        self.assertIn("sensitive path: scripts/verify.sh", doc["error"])
        self.assertEqual(self.states(run_id)[-2:], ["building", "failed"])
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])

    def test_a_sensitive_path_changed_after_the_build_fails_the_run_at_commit_time(self):
        os.environ["FAKE_AGENT_ARTIFACTS"] = json.dumps(["src/feature.txt", "src"])
        run_id = self.start("--no-plan-gate")
        doc = self.doc(run_id)
        self.assertEqual(doc["state"], "failed")
        self.assertIn("sensitive path: src/.gitattributes", doc["error"])
        self.assertEqual(self.states(run_id)[-2:], ["opening_pr", "failed"])
        self.assertEqual(self.gh_calls(), [])
        self.assertEqual(self.origin_branches(), ["main"])


class RealDispatcherTest(Base):
    def test_the_real_dispatcher_accepts_each_step_plan(self):
        """Offline: `role_dispatch.py create` validates and stores a plan; it launches nothing."""
        packet = runs.seed.build_packet(self.item, runs.PACKET_BYTES)["packet"]
        config = harness_config.load()["dispatch"]
        limit = config["max_context_bytes"]
        env = {**os.environ, "HARNESS_DISPATCH_DB": str(self.tmp / "dispatch.sqlite"), "HARNESS_WORKFLOW_STATE": str(self.tmp / "workflow.sqlite"),
               "HARNESS_AUDIT_OUTBOX": str(self.tmp / "audit-outbox.sqlite"), "HARNESS_AUDIT_ENABLED": "0"}
        for step, profile, owned, extra in (("plan", "tm_plan", None, ""), ("build", "tm_build", ["src/feature.txt", "docs"], "漢字 plan line\n" * 4000),
                                            ("review", "tm_review", ["src/feature.txt", "docs"], json.dumps(["src/feature.txt"]))):
            plan = runs.worker_plan("ab" * 16, step, profile, config["profiles"][profile], packet, limit, owned, extra)
            worker = plan["workers"][0]
            self.assertLessEqual(len(json.dumps(worker).encode()), limit - runs.CONTEXT_MARGIN)
            self.assertEqual(worker["context"].count("<<<END-TASK-MANAGER-UNTRUSTED>>>"), 1)
            self.assertEqual(worker["access"], "edit" if step == "build" else "read")
            file = self.tmp / f"{step}.json"
            file.write_text(json.dumps(plan))
            done = subprocess.run([sys.executable, str(ROOT / "scripts" / "role_dispatch.py"), "create", "--project", str(self.repo),
                                   "--session-id", "tm-" + "ab" * 16, "--plan", str(file)], capture_output=True, text=True, env=env, timeout=120)
            self.assertEqual(done.returncode, 0, f"{step}: {done.stderr[-600:]}")
            self.assertEqual(json.loads(done.stdout)["status"], "active", step)

    def test_a_context_too_large_for_the_packet_keeps_one_closing_fence(self):
        packet = runs.seed.build_packet(self.item, runs.PACKET_BYTES)["packet"]
        profile = {"provider": "command", "model": "fake", "effort": "low", "role": "*"}
        worker = runs.worker_plan("ab" * 16, "build", "tm_build", profile, packet, 5000, ["src"], "plan " * 2000)["workers"][0]
        self.assertLessEqual(len(json.dumps(worker).encode()), 5000 - runs.CONTEXT_MARGIN)
        self.assertTrue(worker["context"].endswith("[truncated]\n<<<END-TASK-MANAGER-UNTRUSTED>>>"))
        self.assertEqual(worker["role"], "implementer")
        with self.assertRaises(runs.StepError):
            runs.worker_plan("ab" * 16, "plan", "tm_plan", profile, packet, 3000)


if __name__ == "__main__":
    unittest.main()
