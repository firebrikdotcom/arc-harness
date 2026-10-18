import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import jev_context


class ContextTests(unittest.TestCase):
    def context(self):
        return {"task_id":"task", "request_id":"prompt", "task_summary":{"description":"Improve decision routing context"},
                "active_todo_id":"edit", "evidence_revision":3,
                "todos":{"edit":{"description":"Add bounded action metadata", "criterion":"Provider context contains only redacted facts", "status":"in_progress"},
                         "inspect":{"status":"completed", "evidence":"Read producer and reducer definitions"},
                         "blocked":{"status":"blocked", "description":"Find missing category field"}},
                "checks":{"verify":{"exit_code":0}}}

    def test_task_action_evidence_and_unknowns_are_distinct(self):
        result = jev_context.build({"tool_name":"exec_command","tool_input":{"cmd":"python3 -m unittest discover -s tests"}}, self.context())
        text = json.dumps(result)
        for wanted in ("Improve decision routing context", "Add bounded action metadata", "run_checks", "Read producer and reducer definitions", "Find missing category field", "verify: passed", "review: unknown", "not independently established"):
            self.assertIn(wanted, text)
        self.assertEqual(result["origin"], "automatic_tool")
        self.assertEqual(result["context_version"], "tool-context-1")
        self.assertLess(len(text.encode()), 6000)
        self.assertIn("unknown", json.dumps(jev_context.build({}, {})))

    def test_raw_prompts_source_paths_contacts_secrets_and_command_arguments_do_not_escape(self):
        context = self.context()
        context.update(prompt="PRIVATE PROMPT", model="PRIVATE MODEL", cwd="/private/folder", transcript="PRIVATE SOURCE")
        context["task_summary"]["description"] = "Contact Alice Smith at alice@example.org https://private.example/path password=hidden-value /private/account"
        result = jev_context.build({"tool_name":"exec_command","tool_input":{"cmd":"curl -H 'Authorization: Bearer private-credential' https://private.example/api"}}, context)
        text = json.dumps(result)
        for private in ("Alice Smith", "alice@example", "private.example", "hidden-value", "/private/", "PRIVATE PROMPT", "PRIVATE SOURCE", "private-credential", "Authorization"):
            self.assertNotIn(private, text)
        self.assertIn("http_request", text)
        self.assertEqual(jev_context.summary('def code():\n return "private"'), "withheld: not bounded prose")

    def test_patch_metadata_contains_types_operations_and_count_only(self):
        patch = '*** Begin Patch\n*** Update File: /private/person/summary.rs\n@@\n+PRIVATE SOURCE\n*** Add File: private/test_context.py\n+PASSWORD=private\n*** End Patch'
        text = json.dumps(jev_context.action_facts({"tool_name":"apply_patch","tool_input":{"input":patch}}))
        self.assertIn("update Rust logic", text)
        self.assertIn("add Python logic", text)
        self.assertIn("count: 2", text)
        for private in ("/private/", "summary.rs", "test_context.py", "PRIVATE SOURCE", "PASSWORD"):
            self.assertNotIn(private, text)

    def test_read_results_are_bounded_scoped_and_never_copy_output(self):
        context = self.context()
        for _ in range(20):
            jev_context.observe_read({"tool_name":"Read","tool_response":{"content":"PRIVATE SOURCE"}}, context)
        self.assertEqual(len(context["routing_observations"]), 12)
        text = json.dumps(jev_context.build({}, context))
        self.assertIn("file_read:returned", text)
        self.assertNotIn("PRIVATE SOURCE", text)
        context["request_id"] = "other"
        self.assertNotIn("Recent read results", json.dumps(jev_context.build({}, context)))

    def test_plan_purpose_is_available_without_task_metadata_and_resets(self):
        from types import SimpleNamespace
        import workflow_todos
        context = {}
        workflow_todos.handle(SimpleNamespace(todo_action="plan", reason="Improve routing metadata",
            items=json.dumps([{"id":"edit","description":"Add category fields","criterion":"Categories render"}])),
            "session", context, lambda *args, **kwargs: None)
        self.assertEqual(context["plan_summary"], "Improve routing metadata")
        self.assertIn("Improve routing metadata", json.dumps(jev_context.build({}, context)))
        context["task_summary"] = {"description":"old task"}
        workflow_todos.reset_task(context)
        self.assertNotIn("plan_summary", context)
        self.assertNotIn("task_summary", context)

    def test_service_mutations_and_ci_inspections_have_distinct_intents(self):
        self.assertEqual(jev_context.command_intent(['systemctl','--user','restart','private-service']), 'restart_service')
        self.assertEqual(jev_context.command_intent(['systemctl','--user','show','private-service']), 'inspect_service_status')
        self.assertEqual(jev_context.command_intent(['gh','run','watch','private-run']), 'inspect_ci_status')
        self.assertEqual(jev_context.command_intent(['systemctl','--user','restart','harness-audit.service']), 'restart_project_audit_dashboard')

    def test_updated_inspection_evidence_is_not_lost_to_plan_order(self):
        context = self.context()
        context['todos']['inspect'].update(updated_at='2026-10-18T02:00:00Z', evidence='Current installed service target verified')
        for n in range(4):
            context['todos'][str(n)] = {'status':'completed', 'evidence':'Older unrelated evidence', 'updated_at':'2026-10-17T01:00:00Z'}
        self.assertIn('Current installed service target verified', json.dumps(jev_context.build({}, context)))


if __name__ == "__main__":
    unittest.main()
