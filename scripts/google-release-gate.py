#!/usr/bin/env python3
"""Validate and report the Google Drive release gate."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
GATE = ROOT / "docs/google-release-gate.json"
ALLOWED_STATUS = {"closed", "open", "deferred"}


def load_gate() -> tuple[dict, list[str]]:
    errors: list[str] = []
    try:
        gate = json.loads(GATE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return {}, [f"cannot read {GATE.relative_to(ROOT)}: {error}"]

    if not re.fullmatch(r"\d+\.\d+\.\d+", str(gate.get("release", ""))):
        errors.append("release must be a stable semantic version")
    if not str(gate.get("product_scope", "")).strip():
        errors.append("product_scope is missing")

    requirements = gate.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        return gate, errors + ["requirements must be a non-empty list"]

    seen: set[str] = set()
    for index, requirement in enumerate(requirements):
        where = f"requirements[{index}]"
        if not isinstance(requirement, dict):
            errors.append(f"{where} must be an object")
            continue
        identifier = requirement.get("id")
        if not isinstance(identifier, str) or not re.fullmatch(r"[a-z0-9-]+", identifier):
            errors.append(f"{where}.id is invalid")
        elif identifier in seen:
            errors.append(f"duplicate requirement id: {identifier}")
        else:
            seen.add(identifier)
        status = requirement.get("status")
        if status not in ALLOWED_STATUS:
            errors.append(f"{where}.status must be one of {sorted(ALLOWED_STATUS)}")
        if not isinstance(requirement.get("blocks_release"), bool):
            errors.append(f"{where}.blocks_release must be boolean")
        if not str(requirement.get("summary", "")).strip():
            errors.append(f"{where}.summary is missing")
        evidence = requirement.get("evidence")
        if not isinstance(evidence, list) or not evidence:
            errors.append(f"{where}.evidence must be a non-empty list")
        else:
            for value in evidence:
                if not isinstance(value, str) or not value:
                    errors.append(f"{where}.evidence contains an invalid path")
                elif not (ROOT / value).is_file():
                    errors.append(f"{where}.evidence does not exist: {value}")
        if status == "open" and not str(requirement.get("next_action", "")).strip():
            errors.append(f"{where}.next_action is required while open")
        if status == "deferred":
            if requirement.get("blocks_release"):
                errors.append(f"{where}: a deferred requirement cannot block release")
            if not str(requirement.get("rationale", "")).strip():
                errors.append(f"{where}.rationale is required while deferred")
    return gate, errors


def workspace_version() -> str | None:
    source = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r"(?m)^version = \"([^\"]+)\"$", source)
    return match.group(1) if match else None


def release_shape_errors(version: str) -> list[str]:
    errors: list[str] = []
    arch = re.search(
        r"(?m)^pkgver=(\S+)$",
        (ROOT / "packaging/arch/PKGBUILD").read_text(encoding="utf-8"),
    )
    deb = re.search(
        r"^cirrove \(([^)]+)-1\)",
        (ROOT / "packaging/debian/changelog").read_text(encoding="utf-8"),
    )
    rpm = re.search(
        r"(?m)^Version:\s+(\S+)$",
        (ROOT / "packaging/rpm/cirrove.spec").read_text(encoding="utf-8"),
    )
    values = {
        "Cargo.toml": workspace_version(),
        "packaging/arch/PKGBUILD": arch.group(1) if arch else None,
        "packaging/debian/changelog": deb.group(1) if deb else None,
        "packaging/rpm/cirrove.spec": rpm.group(1) if rpm else None,
    }
    for path, actual in values.items():
        if actual != version:
            errors.append(f"{path}: expected release version {version}, found {actual or 'nothing'}")
    changelog = (ROOT / "docs/changelog.md").read_text(encoding="utf-8")
    if not re.search(rf"(?m)^## {re.escape(version)} — \d{{4}}-\d{{2}}-\d{{2}}$", changelog):
        errors.append(f"docs/changelog.md: missing dated {version} release heading")
    metainfo = (ROOT / "packaging/metainfo/io.github.Dandiccf.Cirrove.metainfo.xml").read_text(
        encoding="utf-8"
    )
    if f'<release version="{version}" ' not in metainfo:
        errors.append(f"AppStream metadata: missing {version} release entry")
    return errors


def oauth_site_errors() -> list[str]:
    completed = subprocess.run(
        [sys.executable, str(ROOT / "scripts/check-oauth-site.py"), "--release"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode == 0:
        return []
    return [line.removeprefix("- ") for line in completed.stderr.splitlines() if line.startswith("-")]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--release",
        action="store_true",
        help="fail unless every blocking gate and release artifact condition is closed",
    )
    args = parser.parse_args()

    gate, errors = load_gate()
    if errors:
        print("Google release gate is invalid:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    requirements = gate["requirements"]
    closed = [item for item in requirements if item["status"] == "closed"]
    deferred = [item for item in requirements if item["status"] == "deferred"]
    blockers = [
        item for item in requirements if item["blocks_release"] and item["status"] != "closed"
    ]
    print(
        f"Google Drive {gate['release']} gate: {len(closed)} closed, "
        f"{len(blockers)} blocking, {len(deferred)} explicitly deferred"
    )
    for item in blockers:
        print(f"  BLOCK {item['id']}: {item['next_action']}")
    for item in deferred:
        print(f"  DEFER {item['id']}: {item['rationale']}")

    if not args.release:
        return 0

    release_errors = [f"{item['id']}: {item['summary']}" for item in blockers]
    release_errors.extend(oauth_site_errors())
    release_errors.extend(release_shape_errors(gate["release"]))
    if release_errors:
        print("Google release gate failed:", file=sys.stderr)
        for error in release_errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(f"Google Drive {gate['release']} release gate passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
