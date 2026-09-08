#!/usr/bin/env python3
"""Fail if an ignored test is not run by CI and not explicitly excused.

CI selects its kernel-FUSE tests with substring filters such as
`filesystem::capacity::real_`. That is fragile by construction: a test whose name
lacks the prefix, or which moves into a submodule, silently stops running and
nothing goes red. It has already happened -- `namespace_capacity_baseline` and
`shared_projection_payload_baseline` sit inside a module CI selects, but under a
filter that requires `real_` after it, so neither has ever run automatically.

This turns that silence into a failing build. Every `#[ignore]`d test must either
match a filter CI actually uses, or appear below with a reason. Nothing is
excused by accident.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/ci.yml"

# Ignored tests deliberately not run by CI. A test belongs here only with a
# reason someone can disagree with.
EXCUSED = {
    # Re-exec entry points: the parent test invokes the binary again with
    # --exact <name> --ignored, so these do execute, just not by CI directly.
    "abrupt_exit_mount_fixture": "subprocess entry point, driven by its parent test",
    "writable_crash_mount_fixture": "subprocess entry point, driven by its parent test",
    "working_crash_fixture": "subprocess entry point, driven by its parent test",
    "journal_crash_fixture": "subprocess entry point, driven by its parent test",
    "namespace_crash_child": "subprocess entry point, driven by its parent test",
    "preparation_crash_child": "subprocess entry point, driven by its parent test",
    "handoff_crash_child": "subprocess entry point, driven by its parent test",
    "replacement_crash_child": "subprocess entry point, driven by its parent test",
    "unlinked_crash_child": "subprocess entry point, driven by its parent test",
    # Capacity benchmarks. Minutes to hours, and their memory numbers are only
    # comparable on a quiet machine, which a shared runner is not.
    "namespace_capacity_baseline": "capacity benchmark; run deliberately, not on a shared runner",
    "shared_projection_payload_baseline": "capacity benchmark; run deliberately",
    "directory_publication_capacity": "500k-row benchmark; run deliberately",
    "large_directory_read_memory": "500k-entry benchmark; needs CIRROVE_DIRECTORY_* set explicitly",
    "synthetic_latency_report": "reporting fixture, not a pass/fail test",
}


def ci_filters() -> tuple[list[str], set[str]]:
    """Name substrings CI filters on, and targets it runs unfiltered.

    A step that passes `--ignored` with no name filter runs every ignored test in
    that target, so the whole file is covered and no name needs to match.
    """
    text = WORKFLOW.read_text()
    filters: list[str] = []
    unfiltered: set[str] = set()
    for line in text.splitlines():
        if "cargo test" not in line or "--no-run" in line:
            continue
        # Split at the LAST `--`: a line may carry an earlier one from a wrapper
        # such as `xvfb-run -a dbus-run-session -- cargo test ...`.
        named = False
        before = line.rsplit(" -- ", 1)[0]
        tokens = before.replace('"', " ").replace("'", " ").split()
        skip_next = False
        for index, token in enumerate(tokens):
            if skip_next:
                skip_next = False
                continue
            if token.startswith("-"):
                skip_next = token in {"-p", "--test", "--bin", "--example"}
                continue
            if index and tokens[index - 1] in {"-p", "--test", "--bin"}:
                continue
            if token in {"cargo", "test", "timeout", "run:", "xvfb-run", "dbus-run-session"}:
                continue
            if token.endswith("s") and token[:-1].isdigit():
                continue
            if "::" in token or token.startswith("real_") or "_read_workload" in token:
                filters.append(token.replace("${mode}", ""))
                named = True
        if "--ignored" in line and not named:
            for index, token in enumerate(tokens):
                if index and tokens[index - 1] == "--test":
                    unfiltered.add(token)
    return filters, unfiltered


def ignored_tests() -> dict[str, str]:
    """Every #[ignore]d test name, mapped to the file that declares it."""
    found = {}
    for path in ROOT.glob("crates/**/*.rs"):
        lines = path.read_text().splitlines()
        for index, line in enumerate(lines):
            if "#[ignore" not in line:
                continue
            for following in lines[index + 1 : index + 6]:
                match = re.search(r"\bfn\s+([a-z0-9_]+)\s*\(", following)
                if match:
                    found[match.group(1)] = str(path.relative_to(ROOT))
                    break
    return found


def main() -> int:
    filters, unfiltered = ci_filters()
    tests = ignored_tests()
    uncovered = {
        name: where
        for name, where in tests.items()
        if name not in EXCUSED
        and Path(where).stem not in unfiltered
        and not any(f in name or name.startswith(f.split("::")[-1]) for f in filters)
    }
    stale = sorted(set(EXCUSED) - set(tests))

    print(
        f"{len(tests)} ignored tests, {len(filters)} name filters, "
        f"{len(unfiltered)} unfiltered targets, {len(EXCUSED)} excused"
    )
    if stale:
        print("\nExcused tests that no longer exist -- remove them from EXCUSED:")
        for name in stale:
            print(f"  {name}")
    if uncovered:
        print("\nIgnored tests that CI does not run and that are not excused:")
        for name, where in sorted(uncovered.items()):
            print(f"  {name}  ({where})")
        print(
            "\nAdd a CI step that runs it, or add it to EXCUSED with a reason.\n"
            "A test nobody runs and nobody excused is a test nobody knows is dead."
        )
    return 1 if uncovered or stale else 0


if __name__ == "__main__":
    sys.exit(main())
