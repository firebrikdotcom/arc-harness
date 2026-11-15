#!/usr/bin/env python3
"""Stand-in for `scripts/harness dispatch` in the task-manager run tests.

  python3 tests/fake_agent.py create --project DIR --session-id S --plan FILE
  python3 tests/fake_agent.py run|status|cancel --run ID
  python3 tests/fake_agent.py integrate --run ID --worker W

It launches nothing. `create` checks the plan has the shape the real dispatcher
requires, `run` answers with a canned report for the plan's one worker (a plan
with proposed paths, an edit, a passing review), and `integrate` writes a small
file inside the worker's first owned path in the project. Output shapes follow
scripts/role_dispatch.py.

Environment:
  FAKE_AGENT_STATE      directory for run records and calls.log (required)
  FAKE_AGENT_ARTIFACTS  JSON list the plan worker proposes (default ["src/feature.txt"])
  FAKE_AGENT_FAIL       worker id whose report is `failed`
  FAKE_AGENT_STRAY=1    integrate also writes stray-outside-owned.txt at the project root
  FAKE_AGENT_HOLD       path: `run` waits (at most 60 s) while this file exists
  FAKE_AGENT_WRITE      JSON list of project-relative files integrate also writes
  FAKE_AGENT_GIT        JSON list of git argument lists integrate then runs in the project
                        (for example [["add", "stray-outside-owned.txt"]] to stage a file)
"""
import argparse
import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

WORKER_FIELDS = {"id", "role", "responsibility", "acceptance", "owned_paths", "context", "dependencies", "access", "profiles"}
ASSESSMENT_FIELDS = {"task_size", "strategy", "reason", "model_fit", "latency_tradeoff"}
PLAN_TEXT = "Plan: write one line to src/feature.txt and check that the file exists."
EDIT_TEXT = "delivered by the fake agent\n"
STRAY = "stray-outside-owned.txt"


def state_dir() -> Path:
    directory = Path(os.environ["FAKE_AGENT_STATE"])
    directory.mkdir(parents=True, exist_ok=True)
    return directory


def log(line: str) -> None:
    with (state_dir() / "calls.log").open("a") as stream:
        stream.write(line + "\n")


def load(run_id: str) -> dict:
    return json.loads((state_dir() / (run_id + ".json")).read_text())


def save(run: dict) -> None:
    (state_dir() / (run["id"] + ".json")).write_text(json.dumps(run))


def view(run: dict) -> dict:
    return {"run": {key: run[key] for key in ("id", "project", "session", "status")}, "workers": [run["worker"]], "runtime_actions": []}


def create(args) -> dict:
    plan = json.loads(args.plan.read_text())
    workers = plan.get("workers")
    if set(plan) - {"workers", "rationale", "acceptance", "request_id", "assessment"} or not isinstance(workers, list) or len(workers) != 1:
        raise ValueError("plan requires exactly one worker here")
    worker = workers[0]
    if set(worker) != WORKER_FIELDS or worker["access"] not in ("read", "edit") or not worker["profiles"] or not worker["acceptance"]:
        raise ValueError("invalid worker contract")
    if worker["access"] == "edit" and not worker["owned_paths"]:
        raise ValueError("editing worker must own paths")
    if set(plan.get("assessment") or {}) != ASSESSMENT_FIELDS or plan["assessment"]["strategy"] != "delegate":
        raise ValueError("assessment requires exactly task_size, strategy, reason, model_fit and latency_tradeoff")
    if not args.session_id or not args.project.is_dir():
        raise ValueError("create requires --plan, --project and exact --session-id")
    run = {"id": uuid.uuid4().hex, "project": str(args.project.resolve()), "session": args.session_id, "status": "active",
           "worker": {**worker, "status": "queued"}}
    save(run)
    log(f"create {worker['id']} {worker['access']} session={args.session_id}")
    return {"run_id": run["id"], "status": "active"}


def run_worker(args) -> dict:
    run = load(args.run)
    worker = run["worker"]
    log(f"run {worker['id']}")
    hold = os.environ.get("FAKE_AGENT_HOLD")
    deadline = time.monotonic() + 60
    while hold and Path(hold).exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    passed = os.environ.get("FAKE_AGENT_FAIL") != worker["id"]
    if worker["id"] == "plan":
        evidence, artifacts = PLAN_TEXT, json.loads(os.environ.get("FAKE_AGENT_ARTIFACTS") or '["src/feature.txt"]')
    elif worker["access"] == "edit":
        evidence, artifacts = "Edited the owned paths.", list(worker["owned_paths"])
        worker["changed_paths"] = list(worker["owned_paths"])
    else:
        evidence, artifacts = ("The change matches the request." if passed else "REVIEW-FINDING: the change is incomplete."), []
    worker["result"] = {"status": "completed" if passed else "failed", "evidence": evidence, "artifacts": artifacts,
                        "acceptance": [{"criterion": c, "evidence": "checked", "passed": passed} for c in worker["acceptance"]]}
    worker["status"] = run["status"] = "completed" if passed else "failed"
    save(run)
    log(f"done {worker['id']} {worker['status']}")
    return view(run)


def integrate(args) -> dict:
    run = load(args.run)
    worker = run["worker"]
    if worker["id"] != args.worker or worker["access"] != "edit" or worker["status"] != "completed":
        raise ValueError("integration requires a verified completed editing worker")
    project = Path(run["project"])
    target = project / worker["owned_paths"][0]
    if target.is_dir():
        target = target / "fake-agent.txt"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(EDIT_TEXT)
    if os.environ.get("FAKE_AGENT_STRAY") == "1":
        (project / STRAY).write_text("not owned\n")
    for name in json.loads(os.environ.get("FAKE_AGENT_WRITE") or "[]"):
        (project / name).parent.mkdir(parents=True, exist_ok=True)
        (project / name).write_text(EDIT_TEXT)
    for git_args in json.loads(os.environ.get("FAKE_AGENT_GIT") or "[]"):
        subprocess.run(["git", "-C", str(project), *git_args], check=True, capture_output=True, timeout=60)
    log(f"integrate {worker['id']}")
    return {"status": "integrated", "worker_id": worker["id"], "project": run["project"]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("create", "run", "status", "integrate", "cancel"))
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--session-id")
    parser.add_argument("--plan", type=Path)
    parser.add_argument("--run")
    parser.add_argument("--worker")
    args = parser.parse_args()
    try:
        if args.action == "create":
            value = create(args)
        elif args.action == "run":
            value = run_worker(args)
        elif args.action == "integrate":
            value = integrate(args)
        elif args.action == "status":
            value = view(load(args.run))
        else:
            log(f"cancel {args.run}")
            value = {"status": "cancelled", "handoffs": []}
        print(json.dumps(value, indent=2))
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    sys.exit(main())
