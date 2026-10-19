"""Shared locations for orchestration state; targets keep their own database."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def db_root(project: Path) -> Path:
    project = Path(project).resolve()
    explicit = os.environ.get("HARNESS_DB_ROOT")
    control = os.environ.get("HARNESS_CONTROL_DB_ROOT")
    # A managed launcher supplies the base registry, not a target override.
    if explicit and explicit != control:
        return Path(explicit).resolve()
    developer = os.environ.get("HARNESS_DEVELOPER_ROOT")
    if project == ROOT or developer and project == Path(developer).resolve():
        return Path(control).resolve() if control else ROOT / ".harness-db"
    command = [str(ROOT / "scripts/harness-target.sh")]
    result = subprocess.run([*command, "db-root", str(project)], capture_output=True,
                            text=True, timeout=10, check=False)
    if result.returncode == 0 and result.stdout.strip():
        return Path(result.stdout.strip())
    result = subprocess.run([*command, "register", str(project)], capture_output=True,
                            text=True, timeout=10, check=False)
    if result.returncode != 0 or not result.stdout.strip():
        raise ValueError("Cannot resolve target orchestration database")
    return Path(result.stdout.strip()) / "db"
