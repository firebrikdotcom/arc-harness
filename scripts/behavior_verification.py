#!/usr/bin/env python3
"""Validate explicitly opted-in behavioral verification definitions without running them."""
import argparse
import hashlib
import json
import re
import sys
from datetime import datetime
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
    root = Path(receipt["evidence_dir"]).resolve(strict=True)
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

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["validate"])
    parser.add_argument("--project", required=True)
    args = parser.parse_args()
    try:
        print(validate(args.project))
        return 0
    except (Invalid, OSError, UnicodeError, ValueError) as exc:
        print(f"behavior validation: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
