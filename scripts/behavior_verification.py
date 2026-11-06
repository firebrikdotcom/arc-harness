#!/usr/bin/env python3
"""Validate explicitly opted-in behavioral verification definitions without running them."""
import argparse
import hashlib
import json
import os
import subprocess
import uuid
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

MAX_BYTES = 262144
ID = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


class Invalid(ValueError):
    pass


def load_json(path):
    if path.stat().st_size > MAX_BYTES:
        raise Invalid(f"{path.name}: file exceeds {MAX_BYTES} bytes")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (ValueError, UnicodeError) as exc:
        raise Invalid(f"{path.name}: invalid JSON: {exc}") from exc


def document(root, value):
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise Invalid("document path must be a nonempty relative path")
    path = Path(value)
    if path.is_absolute() or any(part in (".", "..") for part in value.split("/")):
        raise Invalid(f"unsafe document path: {value}")
    resolved = (root / path).resolve()
    if not resolved.is_relative_to(root) or not resolved.is_file():
        raise Invalid(f"missing or unsafe document: {value}")
    if resolved.stat().st_size > MAX_BYTES:
        raise Invalid(f"document exceeds {MAX_BYTES} bytes: {value}")
    return resolved


def validate(project):
    root = Path(project).resolve(strict=True)
    manifest = root / ".harness-verification.json"
    if not manifest.exists() and not manifest.is_symlink():
        return "not enabled"
    if not manifest.resolve().is_relative_to(root) or not manifest.is_file():
        raise Invalid("manifest is missing, not a file, or escapes project")
    data = load_json(manifest)
    if not isinstance(data, dict) or not isinstance(data.get("enabled"), bool):
        raise Invalid("manifest requires boolean enabled")
    if not data["enabled"]:
        return "disabled"
    if set(data) != {"version", "enabled", "guide", "index", "features"} or type(data["version"]) is not int or data["version"] != 1:
        raise Invalid("enabled manifest requires version 1, guide, index and features")
    document(root, data["guide"])
    index_path = document(root, data["index"])
    index = index_path.read_text(encoding="utf-8")
    features = data["features"]
    if not isinstance(features, list) or not features or len(features) > 100:
        raise Invalid("features must contain 1 to 100 entries")
    seen = set()
    for feature in features:
        if not isinstance(feature, dict) or set(feature) != {"id", "document", "references"}:
            raise Invalid("feature requires id, document and references")
        name = feature["id"]
        if not isinstance(name, str) or not ID.fullmatch(name) or name in seen:
            raise Invalid(f"invalid or duplicate feature ID: {name}")
        seen.add(name)
        feature_path = document(root, feature["document"])
        links = re.findall(r"\]\(([^)#]+)(?:#[^)]*)?\)", index)
        if not any((index_path.parent / link).resolve() == feature_path for link in links if not re.match(r"[a-zA-Z][a-zA-Z0-9+.-]*:", link)):
            raise Invalid(f"feature {name}: index does not link its document")
        refs = feature["references"]
        if not isinstance(refs, list) or not refs or len(refs) > 100:
            raise Invalid(f"feature {name}: references must contain 1 to 100 entries")
        for ref in refs:
            if not isinstance(ref, dict) or set(ref) != {"path", "id"} or not isinstance(ref["id"], str) or not ref["id"] or len(ref["id"]) > 128:
                raise Invalid(f"feature {name}: invalid inventory reference")
            text = document(root, ref["path"]).read_text(encoding="utf-8")
            if not re.search(r"(?<![\w-])" + re.escape(ref["id"]) + r"(?![\w-])", text):
                raise Invalid(f"feature {name}: inventory ID {ref['id']} not found in {ref['path']}")
    return f"validated {len(seen)} features"



def validate_report(report, receipt):
    """Validate a captured report against an independently supplied attempt receipt.

    This is deliberately not a command runner: actions and observations are data.
    The caller owns receipt persistence and must revalidate artifacts at check time.
    """
    required = {"version", "attempt", "run", "project", "tree", "definition",
                "features", "outcome", "started_at", "ended_at", "source_build",
                "runtime_build", "platform", "configuration", "test_data",
                "readiness", "actions", "cleanup", "artifacts", "reason"}
    if not isinstance(report, dict) or set(report) != required or type(report["version"]) is not int or report["version"] != 1:
        raise Invalid("report requires exactly the version 1 fields")
    for field in ("attempt", "run", "project", "tree", "definition", "started_at", "ended_at",
                  "source_build", "runtime_build", "platform", "configuration", "test_data", "reason"):
        value = report[field]
        if not isinstance(value, str) or len(value) > 4096 or (field != "reason" and not value.strip()):
            raise Invalid(f"report: invalid {field}")
    try:
        start = datetime.fromisoformat(report["started_at"].replace("Z", "+00:00"))
        end = datetime.fromisoformat(report["ended_at"].replace("Z", "+00:00"))
        if start.tzinfo is None or end.tzinfo is None or end < start:
            raise ValueError("timestamps must be ordered and include timezones")
    except ValueError as exc:
        raise Invalid(f"report: invalid timestamps: {exc}") from exc
    for field in ("attempt", "run", "project", "tree", "definition", "features"):
        if report[field] != receipt.get(field):
            raise Invalid(f"report: {field} does not match attempt")
    features = report["features"]
    if not isinstance(features, list) or not features or len(features) > 100 or any(not isinstance(x, str) or not ID.fullmatch(x) for x in features) or len(set(features)) != len(features):
        raise Invalid("report: invalid features")
    outcome = report["outcome"]
    if outcome not in ("passed", "failed", "blocked", "inconclusive"):
        raise Invalid("report: invalid outcome")
    if outcome != "passed" and not report["reason"].strip():
        raise Invalid("report: non-passing outcome requires a reason")
    if outcome == "passed" and (report["tree"] in ("none", "unknown") or report["source_build"] in ("none", "unknown") or report["source_build"] != report["runtime_build"]):
        raise Invalid("report: passing outcome requires known source and matching runtime build")
    for field in ("readiness", "actions", "cleanup"):
        entries = report[field]
        if not isinstance(entries, list) or len(entries) > 100:
            raise Invalid(f"report: invalid {field}")
        if outcome == "passed" and not entries:
            raise Invalid(f"report: passing outcome requires {field}")
        for entry in entries:
            keys = {"action", "expected", "observed"} if field == "actions" else {"description", "observed"}
            if not isinstance(entry, dict) or set(entry) != keys or any(not isinstance(v, str) or not v.strip() or len(v) > 4096 for v in entry.values()):
                raise Invalid(f"report: invalid {field} entry")
    artifacts = report["artifacts"]
    if not isinstance(artifacts, list) or len(artifacts) > 100 or (outcome == "passed" and not artifacts):
        raise Invalid("report: invalid artifacts")
    evidence = Path(receipt["evidence_dir"])
    root = evidence.resolve(strict=True)
    if root != evidence or ("evidence_inode" in receipt and [root.stat().st_dev, root.stat().st_ino] != receipt["evidence_inode"]):
        raise Invalid("attempt evidence directory was replaced")
    if not root.is_dir():
        raise Invalid("attempt evidence directory is missing")
    result = []
    for item in artifacts:
        if not isinstance(item, str) or not item or "\\" in item or "\x00" in item or Path(item).is_absolute() or any(part in ("", ".", "..") for part in item.split("/")):
            raise Invalid("report: unsafe artifact path")
        path = (root / item).resolve(strict=True)
        if not path.is_relative_to(root) or not path.is_file():
            raise Invalid(f"report: missing or unsafe artifact: {item}")
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        result.append({"path": item, "size": path.stat().st_size, "sha256": digest.hexdigest()})
    if len({entry["path"] for entry in result}) != len(result):
        raise Invalid("report: duplicate artifact")
    return result

def identity(project):
    root = Path(project).resolve(strict=True)
    target = os.environ.get("HARNESS_BEHAVIOR_TARGET", "")
    run = os.environ.get("HARNESS_BEHAVIOR_RUN_ID", "")
    run_dir = os.environ.get("HARNESS_BEHAVIOR_RUN_DIR", "")
    if not target or Path(target).resolve() != root or not run or not run_dir:
        raise Invalid("behavior commands require the current project run through harness")
    if validate(root) in ("not enabled", "disabled"):
        raise Invalid("behavior verification is not enabled")
    tree = subprocess.check_output(["sh", str(Path(__file__).with_name("tree-hash.sh")), str(root)], text=True).strip()
    if not tree or tree in ("none", "unknown"):
        raise Invalid("source tree identity is unavailable")
    manifest = load_json(root / ".harness-verification.json")
    paths = {".harness-verification.json", manifest["guide"], manifest["index"]}
    for feature in manifest["features"]:
        paths.add(feature["document"])
        paths.update(ref["path"] for ref in feature["references"])
    digest = hashlib.sha256()
    for path in sorted(paths):
        digest.update(path.encode() + b"\0" + document(root, path).read_bytes() + b"\0")
    return root, run, Path(run_dir), tree, digest.hexdigest(), {f["id"] for f in manifest["features"]}


def begin(project, features, evidence_dir):
    root, run, run_dir, tree, definition, known = identity(project)
    if not features or len(set(features)) != len(features) or not set(features) <= known:
        raise Invalid("features must be distinct registered IDs")
    path = Path(evidence_dir)
    if not path.is_absolute() or path.exists() or path.is_symlink():
        raise Invalid("evidence directory must be an unused absolute path")
    parent = path.parent.resolve(strict=True)
    path = parent / path.name
    if not parent.is_relative_to(root) or any(part in (".git", ".harness-db") for part in path.relative_to(root).parts):
        raise Invalid("evidence directory must be within the project, outside protected locations")
    ignored = subprocess.run(["git", "-C", str(root), "check-ignore", "-q", "--no-index", str(path)], check=False)
    if ignored.returncode != 0:
        raise Invalid("evidence directory must be ignored by git")
    path.mkdir(mode=0o700)
    attempt = uuid.uuid4().hex
    receipt = {"attempt": attempt, "run": run, "project": str(root), "tree": tree,
               "definition": definition, "features": features, "evidence_dir": str(path),
               "created_at": datetime.now(timezone.utc).isoformat(),
               "evidence_inode": [path.stat().st_dev, path.stat().st_ino]}
    directory = run_dir / "behavior"
    directory.mkdir(mode=0o700, exist_ok=True)
    (directory / (attempt + ".json")).write_text(json.dumps({"receipt": receipt}) + "\n")
    return attempt


def record(project, attempt, report_path):
    root, run, run_dir, tree, definition, _ = identity(project)
    if not re.fullmatch(r"[0-9a-f]{32}", attempt):
        raise Invalid("invalid attempt ID")
    path = run_dir / "behavior" / (attempt + ".json")
    data = load_json(path)
    receipt = data["receipt"]
    if receipt["run"] != run or receipt["project"] != str(root) or receipt["tree"] != tree or receipt["definition"] != definition:
        raise Invalid("attempt is stale or belongs to another run or project")
    if "report" in data:
        raise Invalid("attempt already recorded; begin a new attempt")
    report = load_json(Path(report_path))
    artifacts = validate_report(report, receipt)
    if datetime.fromisoformat(report["started_at"].replace("Z", "+00:00")) < datetime.fromisoformat(receipt["created_at"]):
        raise Invalid("report predates attempt")
    data.update(report=report, artifacts=artifacts)
    tmp = path.with_suffix(".tmp-" + uuid.uuid4().hex)
    tmp.write_text(json.dumps(data) + "\n")
    os.replace(tmp, path)
    return "recorded " + report["outcome"]


def check(project, features):
    root, run, run_dir, tree, definition, known = identity(project)
    if not features or len(set(features)) != len(features) or not set(features) <= known:
        raise Invalid("features must be distinct registered IDs")
    passing = set()
    for path in (run_dir / "behavior").glob("*.json"):
        try:
            data = load_json(path)
            receipt, report = data["receipt"], data["report"]
            if (receipt["run"], receipt["project"], receipt["tree"], receipt["definition"]) != (run, str(root), tree, definition):
                continue
            if report["outcome"] == "passed" and validate_report(report, receipt) == data["artifacts"]:
                passing.update(receipt["features"])
        except (Invalid, OSError, ValueError, KeyError, TypeError):
            continue
    missing = set(features) - passing
    if missing:
        raise Invalid("missing current passing evidence: " + ", ".join(sorted(missing)))
    return "current passing evidence: " + ", ".join(features)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["validate", "begin", "record", "check"])
    parser.add_argument("--project", required=True)
    parser.add_argument("--features", nargs="+")
    parser.add_argument("--feature", action="append", dest="feature")
    parser.add_argument("--evidence-dir")
    parser.add_argument("--attempt")
    parser.add_argument("--report")
    args = parser.parse_args()
    try:
        if args.action == "validate":
            result = validate(args.project)
        elif args.action == "begin":
            if not args.evidence_dir:
                raise Invalid("begin requires --evidence-dir")
            result = begin(args.project, args.features or args.feature, args.evidence_dir)
        elif args.action == "record":
            if not args.attempt or not args.report:
                raise Invalid("record requires --attempt and --report")
            result = record(args.project, args.attempt, args.report)
        else:
            result = check(args.project, args.features or args.feature)
        print(result)
        return 0
    except (Invalid, OSError, UnicodeError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        print(f"behavior verification: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
