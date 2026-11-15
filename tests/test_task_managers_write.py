"""Task-manager write-back: comment and complete. Offline, against tests/fake_basecamp.py.

The fake runs on a thread of this process so a test can read what was posted and
switch its mode after the cache is primed; the CLI is the real scripts/task_managers.py,
run as a subprocess.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path

TESTS = Path(__file__).resolve().parent
ROOT = TESTS.parent
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(TESTS))

import fake_basecamp as fake  # noqa: E402
import task_managers as tm  # noqa: E402
import task_managers_write as write  # noqa: E402

SECRETS = (fake.CLIENT_SECRET, fake.ACCESS_TOKEN, fake.REFRESH_TOKEN, "fake-access-token", "fake-refresh-token", fake.AUTH_CODE)
ACCOUNT = "/" + fake.ACCOUNT_ID


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="harness-task-managers-write."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.config_home = self.tmp / "config"
        self.db = self.tmp / "db"
        self.log = self.tmp / "requests.log"
        self.token_file = self.tmp / "secrets" / "basecamp.oauth"
        self.env_file = self.tmp / "secrets" / "basecamp.env"
        self.outputs = []
        self.state = fake.State("healthy", str(self.log))
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), fake.make_handler(self.state))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.write_config()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)
        for text in self.outputs:
            for secret in SECRETS:
                self.assertNotIn(secret, text, "a secret reached stdout or stderr")

    def write_config(self):
        self.config_home.mkdir(exist_ok=True)
        self.token_file.parent.mkdir(exist_ok=True)
        self.env_file.write_text(
            f"BASECAMP_CLIENT_ID={fake.CLIENT_ID}\nBASECAMP_CLIENT_SECRET={fake.CLIENT_SECRET}\nBASECAMP_REDIRECT_URI={fake.REDIRECT_URI}\n")
        os.chmod(self.env_file, 0o600)
        self.token_file.write_text(json.dumps({"access_token": fake.ACCESS_TOKEN, "refresh_token": fake.REFRESH_TOKEN,
                                               "expires_in": fake.EXPIRES_IN, "obtained_at": 1_900_000_000}))
        os.chmod(self.token_file, 0o600)
        (self.config_home / "task-managers.json").write_text(json.dumps({
            "version": 1, "owner_login": "owner@example.invalid",
            "providers": {"basecamp": {"env_file": str(self.env_file), "token_file": str(self.token_file),
                                       "account_id": fake.ACCOUNT_ID, "api_base": self.base, "auth_base": self.base}},
            "repos": {"demo": "/srv/demo"}, "mappings": {"basecamp:3001": "demo"}}))

    def cli(self, *args, expect=0):
        env = {k: v for k, v in os.environ.items() if k not in ("HARNESS_DB_ROOT", "XDG_CONFIG_HOME")}
        env["HARNESS_CONFIG_HOME"] = str(self.config_home)
        done = subprocess.run([sys.executable, str(ROOT / "scripts" / "task_managers.py"), "--db-root", str(self.db), *args],
                              capture_output=True, text=True, env=env, timeout=60)
        self.outputs += [done.stdout, done.stderr]
        self.assertEqual(done.returncode, expect, f"{args}: {done.stdout}{done.stderr}")
        return done.stdout

    def json(self, *args, expect=0):
        out = self.cli(*args, expect=expect)
        self.assertEqual(len(out.strip().splitlines()), 1, "stdout must be one JSON document")
        return json.loads(out)

    def requests(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def writes(self):
        return [line for line in self.requests() if not line.startswith("GET ")]

    def text_file(self, text, name="comment.txt"):
        file = self.tmp / name
        file.write_bytes(text if isinstance(text, bytes) else text.encode("utf-8"))
        return str(file)

    def posted(self, todo):
        return [c["content"] for c in self.state.comments.get(todo, []) if c["id"] > 5100]


class Comment(Base):
    def test_comment_sends_one_post_with_the_escaped_text(self):
        text = 'Done <script>alert("x")</script> & \'tested\'.\r\nSecond line\n\n<b>bold</b>\n'
        before = self.json("get", "--item", "4002", "--json")["item"]["comments_count"]
        got = self.json("comment", "--item", "4002", "--text-file", self.text_file(text))
        self.assertEqual(got, {"comment_id": "5101", "url": f"https://basecamp.invalid/{fake.ACCOUNT_ID}/buckets/1001/todos/4002#__recording_5101"})
        self.assertEqual(self.writes(), [f"POST {ACCOUNT}/buckets/1001/recordings/4002/comments.json"])
        self.assertEqual(self.posted(4002), [
            "Done &lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#x27;tested&#x27;.<br>Second line<br><br>&lt;b&gt;bold&lt;/b&gt;"])
        sent = [json.loads(line) for line in Path(str(self.log) + ".headers").read_text().splitlines()]
        self.assertEqual([(e["user_agent"], e["authorization"]) for e in sent if e["method"] == "POST"], [(tm.USER_AGENT, True)])
        # The cache was primed before the comment; the next read still shows it.
        self.assertEqual(self.json("get", "--item", "4002", "--json")["item"]["comments_count"], before + 1)
        self.assertEqual(len(self.writes()), 1)

    def test_comment_reaches_an_item_in_another_project(self):
        self.json("comment", "--item", "4006", "--text-file", self.text_file("Plain note"))
        self.assertEqual(self.writes(), [f"POST {ACCOUNT}/buckets/1002/recordings/4006/comments.json"])
        self.assertEqual(self.posted(4006), ["Plain note"])

    def test_empty_oversized_or_unreadable_text_is_a_usage_error_without_any_request(self):
        (self.tmp / "folder").mkdir()
        os.symlink(self.tmp / "folder", self.tmp / "link")
        cases = {
            "empty": self.text_file(""), "blank": self.text_file(" \n\t\r\n"),
            "long": self.text_file("a" * (write.MAX_TEXT_CHARS + 1)),
            "big": self.text_file("a" * (write.MAX_TEXT_FILE_BYTES + 1)),
            "binary": self.text_file(b"\xff\xfe not utf-8"), "control": self.text_file("a\x00b"),
            "missing": str(self.tmp / "absent.txt"), "directory": str(self.tmp / "folder"), "link": str(self.tmp / "link"),
            "device": "/dev/null" if os.path.exists("/dev/null") else str(self.tmp / "folder"),
        }
        for name, file in cases.items():
            got = self.json("comment", "--item", "4002", "--text-file", file, expect=2)
            self.assertIn(got["error"]["code"], ("text", "text_file"), name)
        self.assertEqual(self.json("comment", "--item", "4002", expect=2)["error"]["code"], "usage")
        # argparse reads --text as short for --text-file: the value is a file name, never the text.
        self.assertEqual(self.json("comment", "--item", "4002", "--text", "from argv", expect=2)["error"]["code"], "text_file")
        self.assertEqual(self.json("comment", "--item", "4002", "from argv", expect=2)["error"]["code"], "usage")
        self.assertEqual(self.requests(), [], "a refused text must not reach the provider")
        self.assertEqual(self.posted(4002), [])

    def test_text_at_the_limits_is_accepted(self):
        self.json("comment", "--item", "4002", "--text-file", self.text_file("a" * write.MAX_TEXT_CHARS + "\n"))
        self.json("comment", "--item", "4002", "--text-file", self.text_file("b"))
        self.assertEqual(self.posted(4002), ["a" * write.MAX_TEXT_CHARS, "b"])
        self.assertEqual(len(self.writes()), 2)

    def test_unknown_item_exits_4_without_a_post(self):
        got = self.json("comment", "--item", "4999", "--text-file", self.text_file("note"), expect=4)
        self.assertEqual(got["error"]["code"], "not_found")
        self.assertEqual(self.json("complete", "--item", "4999", expect=4)["error"]["code"], "not_found")
        self.assertTrue(self.requests())
        self.assertEqual(self.writes(), [])

    def test_upstream_500_exits_5_after_one_post_and_no_retry(self):
        self.json("get", "--item", "4002", "--json")  # prime the cache so the item is found without the provider
        self.state.mode = "error"
        got = self.json("comment", "--item", "4002", "--text-file", self.text_file("note"), expect=5)
        self.assertEqual(got["error"]["code"], "upstream")
        self.assertIn("HTTP 500", got["error"]["message"])
        self.assertEqual(self.writes(), [f"POST {ACCOUNT}/buckets/1001/recordings/4002/comments.json"])
        self.assertEqual(self.posted(4002), [])

    def test_refused_token_exits_5(self):
        self.json("get", "--item", "4002", "--json")
        self.state.mode = "unauthorized"
        got = self.json("comment", "--item", "4002", "--text-file", self.text_file("note"), expect=5)
        self.assertEqual(got["error"]["code"], "unauthorized")
        posts = [line for line in self.writes() if line.endswith("/comments.json")]
        self.assertIn(len(posts), (1, 2), "at most the one repeat ctx.http makes after a token refresh")
        self.assertEqual(self.posted(4002), [])


class Complete(Base):
    def test_complete_sends_one_post_and_the_next_get_shows_done(self):
        self.assertEqual(self.json("get", "--item", "4005", "--json")["item"]["status"], "open")
        self.assertEqual(self.json("lists", "--project", "1001")["lists"][1], {"id": "3002", "name": "Bugs", "open_count": 1})
        self.assertEqual(self.json("complete", "--item", "4005"), {"status": "done"})
        self.assertEqual(self.writes(), [f"POST {ACCOUNT}/buckets/1001/todos/4005/completion.json"])
        self.assertTrue(self.state.completed[4005])
        self.assertEqual(self.json("get", "--item", "4005", "--json")["item"]["status"], "done")
        self.assertEqual(self.json("lists", "--project", "1001")["lists"][1]["open_count"], 0)
        self.assertEqual([i["id"] for i in self.json("todos", "--list", "3002")["items"]], [])
        self.assertEqual(len(self.writes()), 1)
        self.assertFalse(self.state.completed[4001], "no other item was touched")

    def test_upstream_500_exits_5_after_one_post_and_no_retry(self):
        self.json("get", "--item", "4005", "--json")
        self.state.mode = "error"
        got = self.json("complete", "--item", "4005", expect=5)
        self.assertEqual(got["error"]["code"], "upstream")
        self.assertEqual(self.writes(), [f"POST {ACCOUNT}/buckets/1001/todos/4005/completion.json"])
        self.assertFalse(self.state.completed[4005])

    def test_other_projects_cache_is_kept(self):
        self.json("get", "--item", "4006", "--json")  # walks both projects
        folder = self.db / "task-managers" / "basecamp"
        urls = lambda: sorted(json.loads(f.read_text())["url"] for f in folder.glob("*.json"))  # noqa: E731
        before = urls()
        self.json("complete", "--item", "4006")
        after = urls()
        self.assertEqual([u for u in before if "/buckets/1002/" in u and u in after], [])
        self.assertEqual([u for u in before if "/buckets/1002/" not in u], after)
        self.assertTrue([u for u in after if "/buckets/1001/" in u])


class Readers(Base):
    def test_reader_commands_send_get_only(self):
        self.json("check")
        self.assertEqual(self.requests(), [], "check needs no network")
        self.json("projects")
        self.json("lists", "--project", "1001")
        self.json("todos", "--list", "3001")
        self.json("todos", "--list", "3001", "--status", "done", "--refresh")
        self.json("get", "--item", "4001", "--json")
        self.cli("get", "--item", "4003", "--refresh")
        self.json("get", "--item", "4999", "--json", expect=4)
        self.assertTrue(self.requests(), "the fake saw no request")
        self.assertEqual(self.writes(), [])
        self.assertEqual(self.posted(4001), [])
        self.assertEqual(self.state.completed, {t["id"]: t["completed"] for t in fake.TODOS})


class Functions(unittest.TestCase):
    def test_comment_content(self):
        self.assertEqual(write.comment_content("a < b\nc"), "a &lt; b<br>c")
        self.assertEqual(write.comment_content("  <br> &amp; \n"), "&lt;br&gt; &amp;amp;")
        for bad in ("", "  \n", "a" * (write.MAX_TEXT_CHARS + 1), "a\x1bb", None, b"bytes"):
            with self.assertRaises(tm.UsageError) as caught:
                write.comment_content(bad)
            self.assertEqual(caught.exception.exit_code, 2)

    def test_write_errors_are_the_readers_classes(self):
        self.assertIs(write.tm.NotFound, tm.NotFound)
        self.assertEqual((write.MAX_TEXT_CHARS, write.MAX_TEXT_FILE_BYTES), (10_000, 65_536))


if __name__ == "__main__":
    unittest.main()
