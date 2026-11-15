"""Task-manager provider primitives: list-create, todo-create, todo-update, get --full,
open-walk, me, and check's repo_paths. Offline, against tests/fake_basecamp.py.

The fake runs on a thread of this process (see test_task_managers_write.py) so a test
can read what was sent and change the fake's records; the CLI runs as a subprocess.
"""
import json
import sys
import unittest
from pathlib import Path

TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(TESTS))

from test_task_managers_write import ACCOUNT, Base, fake, tm, write  # noqa: E402

API = f"{ACCOUNT}/buckets"


class Writes(Base):
    def bodies(self):
        return [(method, path.replace(API, ""), body) for method, path, body in self.state.bodies]

    def cached_urls(self):
        folder = self.db / "task-managers" / "basecamp"
        return sorted(json.loads(f.read_text())["url"] for f in folder.glob("*.json"))

    def test_list_create_sends_one_post_and_the_next_read_shows_it(self):
        self.assertEqual([entry["id"] for entry in self.json("lists", "--project", "1001")["lists"]], ["3001", "3002"])
        got = self.json("list-create", "--project", "1001", "--name-file", self.text_file("  Release & fixes\n", "name.txt"),
                        "--description-file", self.text_file("a & b\nline two", "desc.txt"))
        self.assertEqual(got, {"list": {"id": "8001", "name": "Release & fixes",
                                        "url": f"https://basecamp.invalid/{fake.ACCOUNT_ID}/buckets/1001/todolists/8001"}})
        self.assertEqual(self.writes(), [f"POST {API}/1001/todosets/2001/todolists.json"])
        self.assertEqual(self.bodies(), [("POST", "/1001/todosets/2001/todolists.json",
                                          {"name": "Release & fixes", "description": "a &amp; b<br>line two"})])
        # The project's cache was dropped: the new list shows without --refresh.
        self.assertEqual([entry["id"] for entry in self.json("lists", "--project", "1001")["lists"]], ["3001", "3002", "8001"])
        self.assertEqual(len(self.writes()), 1)

    def test_list_create_in_a_project_without_the_todo_tool_is_refused_without_a_write(self):
        got = self.json("list-create", "--project", "1003", "--name-file", self.text_file("X"), expect=4)
        self.assertEqual(got["error"]["code"], "no_todoset")
        self.assertEqual(self.writes(), [])

    def test_todo_create_sends_one_post(self):
        got = self.json("todo-create", "--list", "3002", "--title-file", self.text_file("Fix the hinge"),
                        "--description-file", self.text_file("<b>soon</b>", "d.txt"), "--due", "2031-04-05")["item"]
        self.assertEqual((got["id"], got["title"], got["description_text"], got["due_on"], got["status"], got["list"], got["project"]),
                         ("8001", "Fix the hinge", "<b>soon</b>", "2031-04-05", "open", {"id": "3002", "name": "Bugs"},
                          {"id": "1001", "name": "Synthetic Alpha"}))
        self.assertNotIn("_bucket", got)
        self.assertEqual(self.bodies(), [("POST", "/1001/todolists/3002/todos.json",
                                          {"content": "Fix the hinge", "description": "&lt;b&gt;soon&lt;/b&gt;", "due_on": "2031-04-05"})])
        self.assertEqual([i["id"] for i in self.json("todos", "--list", "3002")["items"]], ["4005", "8001"])
        self.assertEqual(len(self.writes()), 1)

    def test_bad_input_is_refused_before_any_request(self):
        for args in (["todo-create", "--list", "3002", "--title-file", self.text_file("")],
                     ["todo-create", "--list", "3002", "--title-file", self.text_file("two\nlines")],
                     ["todo-create", "--list", "3002", "--title-file", self.text_file("x" * (write.MAX_NAME_CHARS + 1))],
                     ["list-create", "--project", "1001", "--name-file", str(self.tmp / "absent")],
                     ["todo-update", "--item", "4001"]):
            self.assertIn(self.json(*args, expect=2)["error"]["code"], ("text", "text_file", "nothing_to_change"), args)
        for args in (["todo-create", "--list", "3002", "--title-file", "f", "--due", "2031-02-30"],
                     ["todo-create", "--list", "3002", "--title-file", "f", "--due", "tomorrow"],
                     ["todo-update", "--item", "4001", "--due", "2031-01-01", "--clear-due"],
                     ["list-create", "--project", "x", "--name-file", "f"]):
            self.assertEqual(self.json(*args, expect=2)["error"]["code"], "usage", args)
        self.assertEqual(self.requests(), [])

    def test_update_resends_every_untouched_field(self):
        todo = next(t for t in self.state.todos if t["id"] == 4001)
        todo.update({"starts_on": "2031-01-20", "subscribers": ["Cy Fixture"]})
        before = self.json("get", "--item", "4001", "--json")["item"]
        got = self.json("todo-update", "--item", "4001", "--title-file", self.text_file("Spin it faster"))["item"]
        self.assertEqual([line for line in self.writes()], [f"PUT {API}/1001/todos/4001.json"])
        self.assertEqual(self.bodies(), [("PUT", "/1001/todos/4001.json", {
            "content": "Spin it faster", "description": fake.TODOS[0]["description"], "assignee_ids": [7000, 7001],
            "completion_subscriber_ids": [7100], "due_on": "2031-02-03", "starts_on": "2031-01-20"})])
        # The item was read fresh, not from the cache primed above.
        self.assertIn(f"GET {API}/1001/todos/4001.json", self.requests())
        self.assertEqual((todo["assignees"], todo["subscribers"], todo["starts_on"], todo["due_on"], todo["description"]),
                         (["Ada Example", "Bo Sample"], ["Cy Fixture"], "2031-01-20", "2031-02-03", fake.TODOS[0]["description"]))
        after = self.json("get", "--item", "4001", "--json")["item"]
        self.assertEqual({**before, "title": "Spin it faster"}, after)
        self.assertEqual(got["title"], "Spin it faster")

    def test_update_due_description_and_clear(self):
        self.json("todo-update", "--item", "4001", "--due", "2032-01-01", "--description-file", self.text_file("new\ntext"))
        self.json("todo-update", "--item", "4001", "--clear-due")
        self.json("todo-update", "--item", "4001", "--description-file", self.text_file("  \n"))
        sent = [body for _, _, body in self.bodies()]
        self.assertEqual([(b["due_on"], b["description"], b["content"]) for b in sent],
                         [("2032-01-01", "new<br>text", "Make the widget spin"), (None, "new<br>text", "Make the widget spin"),
                          (None, "", "Make the widget spin")])
        self.assertTrue(all("notify" not in b for b in sent))
        self.assertEqual(len(self.writes()), 3)

    def test_reopen_sends_one_delete_only_for_a_done_item(self):
        got = self.json("todo-update", "--item", "4004", "--reopen")["item"]
        self.assertEqual(self.writes(), [f"DELETE {API}/1001/todos/4004/completion.json"])
        self.assertEqual((got["status"], self.state.completed[4004]), ("open", False))
        self.assertEqual(self.json("get", "--item", "4004", "--json")["item"]["status"], "open")
        self.json("todo-update", "--item", "4002", "--reopen")  # open already: nothing to send
        self.assertEqual(len(self.writes()), 1)
        self.state.completed[4005] = True
        self.json("todo-update", "--item", "4005", "--title-file", self.text_file("Door squeaks less"), "--reopen")
        self.assertEqual(self.writes()[1:], [f"PUT {API}/1001/todos/4005.json", f"DELETE {API}/1001/todos/4005/completion.json"])

    def test_a_failed_write_is_not_repeated_and_the_cache_is_dropped(self):
        self.json("get", "--item", "4006", "--json")
        self.state.fail_writes = True
        cases = [(["list-create", "--project", "1002", "--name-file", self.text_file("L")], f"POST {API}/1002/todosets/2002/todolists.json"),
                 (["todo-create", "--list", "3003", "--title-file", self.text_file("T")], f"POST {API}/1002/todolists/3003/todos.json"),
                 (["todo-update", "--item", "4006", "--title-file", self.text_file("T")], f"PUT {API}/1002/todos/4006.json")]
        for args, line in cases:
            self.json("projects")  # prime again
            self.json("lists", "--project", "1002")
            got = self.json(*args, expect=5)
            self.assertEqual(got["error"]["code"], "upstream")
            self.assertIn("not repeated", got["error"]["message"])
            self.assertEqual(self.writes()[-1], line)
            self.assertEqual([u for u in self.cached_urls() if "/buckets/1002/" in u], [], args)
        self.assertEqual(len(self.writes()), 3)
        # A to-do that cannot be read fresh is never changed.
        self.state.fail_writes = False
        self.json("get", "--item", "4006", "--json")  # found through the cache; only the fresh read fails
        self.state.mode = "error"
        self.assertEqual(self.json("todo-update", "--item", "4006", "--title-file", self.text_file("T"), expect=5)["error"]["code"],
                         "unexpected_response")
        self.assertEqual(len(self.writes()), 3)


class Readers(Base):
    def test_full_description(self):
        long = " ".join(f"word{n}" for n in range(1500))
        self.state.todos.append({"id": 4100, "list": 3003, "title": "Long one", "completed": False, "due_on": None, "starts_on": None,
                                 "description": f"<div>{long}</div>", "assignees": [], "subscribers": [], "comments_count": 0})
        self.state.completed[4100] = False
        self.assertGreater(len(long), 4000)
        self.assertEqual(self.json("get", "--item", "4100", "--json")["item"]["description_text"], long[:4000])
        self.assertEqual(self.json("get", "--item", "4100", "--json", "--full")["item"]["description_text"], long)
        lines = self.cli("get", "--item", "4100", "--full").splitlines()
        self.assertTrue(lines[1].startswith("<<<TASK-MANAGER-UNTRUSTED sha256="))
        self.assertEqual((lines[-2], lines[-1]), (long, "<<<END-TASK-MANAGER-UNTRUSTED>>>"))
        huge = "x" * (tm.FULL_DESCRIPTION_BYTES + 100)
        next(t for t in self.state.todos if t["id"] == 4100)["description"] = huge
        got = self.json("get", "--item", "4100", "--json", "--full", "--refresh")["item"]["description_text"]
        self.assertEqual(len(got.encode()), tm.FULL_DESCRIPTION_BYTES)

    def test_open_walk(self):
        got = self.json("open-walk")
        self.assertEqual(set(got), {"items", "truncated", "limit"})
        self.assertEqual([i["id"] for i in got["items"]], ["4001", "4002", "4003", "4005", "4006"])
        self.assertEqual((got["truncated"], got["limit"]), (False, tm.OPEN_WALK_DEFAULT))
        first = got["items"][0]
        self.assertEqual((first["project"], first["list"], first["assignees"], first["due_on"], first["status"]),
                         ({"id": "1001", "name": "Synthetic Alpha"}, {"id": "3001", "name": "Backlog"},
                          [{"id": "7000", "name": "Ada Example"}, {"id": "7001", "name": "Bo Sample"}], "2031-02-03", "open"))
        self.assertNotIn("description_text", first)
        cut = self.json("open-walk", "--limit", "2")
        self.assertEqual(([i["id"] for i in cut["items"]], cut["truncated"]), (["4001", "4002"], True))
        self.assertEqual(self.json("open-walk", "--limit", "5")["truncated"], False)
        self.assertEqual([i["id"] for i in self.json("open-walk", "--list", "3002")["items"]], ["4005"])
        self.assertEqual([i["id"] for i in self.json("open-walk", "--project", "1002")["items"]], ["4006"])
        self.assertEqual(self.json("open-walk", "--list", "3999", expect=4)["error"]["code"], "not_found")
        self.assertEqual(self.json("open-walk", "--project", "1999", expect=4)["error"]["code"], "not_found")
        self.assertEqual(self.json("open-walk", "--limit", "0", expect=2)["error"]["code"], "usage")
        self.assertEqual(self.writes(), [])

    def test_me_and_check_repo_paths(self):
        self.assertEqual(self.json("me"), {"person": {"id": "7000", "name": "Ada Example"}})
        count = len(self.requests())
        self.json("me")
        self.assertEqual(len(self.requests()), count, "me is cached like every read")
        got = self.json("check")
        self.assertEqual((got["repos"], got["repo_paths"]), (["demo"], {"demo": "/srv/demo"}))
        self.assertEqual(self.writes(), [])

    def test_check_ssh_destination(self):
        file = self.config_home / "task-managers.json"
        config = json.loads(file.read_text())
        self.assertIsNone(self.json("check")["ssh"])
        for value in ("me@box", "box", "dev_1@host-2.example.invalid", "a" * 200):
            file.write_text(json.dumps({**config, "ssh": value}))
            self.assertEqual(self.json("check")["ssh"], value)
        # It is printed inside a shell command: anything but a plain [user@]host is dropped.
        for value in ("", "-oProxyCommand=x", "me@box; rm -rf /", "me@box -p 22", "$(id)@box", "me@box'", "me@box\n", "bóx",
                      "host:22", "a" * 201, 7, None, ["me@box"], {"host": "box"}, True):
            file.write_text(json.dumps({**config, "ssh": value}))
            self.assertIsNone(self.json("check")["ssh"], value)
        self.assertEqual(self.requests(), [])

    def test_new_commands_parse(self):
        parser = tm.build_parser()
        for argv in (["open-walk", "--project", "1", "--list", "2", "--limit", "9", "--refresh"], ["me", "--json"],
                     ["get", "--item", "1", "--full"], ["todo-update", "--item", "1", "--clear-due", "--reopen"]):
            parser.parse_args(argv)
        for gone in ("associate", "unassociate", "preset", "open", "agent", "attach"):
            self.assertEqual(self.json(gone, expect=2)["error"]["code"], "usage", gone)


if __name__ == "__main__":
    unittest.main()
