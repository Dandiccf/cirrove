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

What it does not check is whether a selected test finishes. A step's `timeout`
kills the whole group, so one slow addition takes every test after it down and
the log simply stops mid-test -- which is how a 426-second capacity fixture went
unnoticed until a docs-only commit went red for it. Selection is proved here;
runtime is not.
"""

import re
import shlex
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/ci.yml"

# Ignored tests deliberately not run by CI. A test belongs here only with a
# reason someone can disagree with.
EXCUSED = {
    "live_google_shared_drive_discovery": (
        "live Workspace grant and exact owned Shared Drive; CI has neither the credential nor test folder"
    ),
    "report_native_doc_and_sheet_export_shape_without_item_identity": (
        "live Google read grant; bounded GET-only export preflight with aggregate output"
    ),
    # Exact live Google helpers need the owner's OAuth grant and are additionally
    # fenced by an explicit state directory plus exact run-owned item identity.
    # Their successful bounded run is recorded in the writable-mount benchmark;
    # CI has neither the credential nor a fixture it is allowed to mutate.
    "rename_the_exact_run_owned_item_outside_the_mount": (
        "live Google grant and exact run-owned item; bounded conflict arm"
    ),
    "exact_remote_bytes_match_the_registered_digest": (
        "live Google grant and exact run-owned item; independent digest readback"
    ),
    "trash_only_the_exact_run_owned_tree": (
        "live Google grant and exact run-owned folder; bounded cleanup"
    ),
    # Needs a live Microsoft account with a write grant, which CI has no way to
    # hold. It records a provider behaviour the rmdir design depends on -- a
    # folder's eTag does not move when a child is added -- so it is run by hand
    # when that assumption is worth re-checking, not on every build.
    "folder_etag_and_mtime_ignore_their_children": (
        "live write-access account; records the provider behaviour rmdir depends on"
    ),
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
    "directories_crash_child": "subprocess entry point, driven by its parent test",
    "ancestry_crash_child": "subprocess entry point, driven by its parent test",
    "allocation_crash_child": (
        "subprocess entry point, driven by "
        "process_death_after_allocation_leaves_saved_uncertainty_without_replay"
    ),
    "interruption_child": (
        "subprocess entry point, driven by "
        "process_exit_records_the_boundary_without_returning_the_next_checkpoint"
    ),
    # Capacity benchmarks. Minutes to hours, and their memory numbers are only
    # comparable on a quiet machine, which a shared runner is not.
    "namespace_capacity_baseline": "capacity benchmark; run deliberately, not on a shared runner",
    "shared_projection_payload_baseline": "capacity benchmark; run deliberately",
    "directory_publication_capacity": "500k-row benchmark; run deliberately",
    "large_directory_read_memory": "500k-entry benchmark; needs CIRROVE_DIRECTORY_* set explicitly",
    "synthetic_latency_report": "reporting fixture, not a pass/fail test",
    "real_writable_namespace_retires_and_stays_bounded": (
        "capacity benchmark; 426 s measured in a debug build, so it never fit the "
        "150 s step it was added to and took the whole group red with it"
    ),
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
            # The three clauses below are shapes CI happened to use. A filter
            # that is simply a test's full name matched none of them, so a step
            # that ran a test by name still counted as covering nothing -- the
            # same silence this script exists to break, one level up.
            plain_name = re.fullmatch(r"[a-z][a-z0-9_]{6,}", token) is not None
            if "::" in token or token.startswith("real_") or "_read_workload" in token or plain_name:
                filters.append(token.replace("${mode}", ""))
                named = True
        if "--ignored" in line and not named:
            for index, token in enumerate(tokens):
                if index and tokens[index - 1] == "--test":
                    unfiltered.add(token)
    return filters, unfiltered



def window_coverage(workflow: str) -> tuple[dict[str, str], list[set[str]]]:
    """Prove the custom GTK harness's scenario union, including exclusions.

    Window scenarios are not #[ignore] functions: their custom main reports
    them ignored without --ignored. Keep this proof separate from the legacy
    substring audit so a skipped scenario cannot be covered by its own skip.
    """
    where = "crates/cirrove-desktop/tests/window.rs"
    source = (ROOT / where).read_text()
    table = source.split("const SCENARIOS:", 1)[1].split("];", 1)[0]
    names = set(re.findall(r'"([a-z][a-z0-9_]+)"', table))
    if not names:
        raise ValueError("custom window harness has no declared scenarios")
    groups: list[set[str]] = []
    for line in workflow.splitlines():
        if "cargo test" not in line or "--no-run" in line:
            continue
        tokens = shlex.split(line)
        start = tokens.index("cargo") + 2
        arguments = tokens[start:]
        separator = arguments.index("--") if "--" in arguments else len(arguments)
        cargo, harness = arguments[:separator], arguments[separator + 1 :]
        if not any(cargo[i : i + 2] == ["-p", "cirrove-desktop"] for i in range(len(cargo))):
            continue
        if not any(cargo[i : i + 2] == ["--test", "window"] for i in range(len(cargo))):
            continue
        if "--ignored" not in harness and "--include-ignored" not in harness:
            continue
        filters: list[str] = []
        skips: list[str] = []
        value_flags = {"-p", "--package", "--test", "--bin", "--example", "--features", "--target", "--profile", "--jobs", "-j"}
        consume = False
        for token in cargo:
            if consume:
                consume = False
            elif token in value_flags:
                consume = True
            elif not token.startswith("-"):
                filters.append(token)
        exact = "--exact" in harness
        values = iter(harness)
        for token in values:
            if token == "--skip":
                skips.append(next(values))
            elif token.startswith("--skip="):
                skips.append(token[7:])
            elif token in {"--test-threads", "--color", "--format", "--logfile", "-Z"}:
                next(values)
            elif not token.startswith("-"):
                filters.append(token)
        def matches(name: str, needle: str) -> bool:
            return name == needle if exact else needle in name
        groups.append({
            name for name in names
            if (not filters or any(matches(name, f) for f in filters))
            and not any(matches(name, skip) for skip in skips)
        })
    covered = set().union(*groups)
    return {name: where for name in names - covered}, groups


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


def matches_ci_filter(name: str, where: str, selected: str) -> bool:
    """Module selectors cover only their declaring source subtree.

    A selector ending in :: has no function-name suffix. Treating that empty
    suffix as a prefix matches every ignored test, even in unrelated modules.
    """
    if selected.endswith("::"):
        parts = Path(where).parts
        if "src" not in parts:
            return False
        modules = list(parts[parts.index("src") + 1 :])
        if not modules:
            return False
        modules[-1] = Path(modules[-1]).stem
        if modules[-1] in {"lib", "main", "mod"}:
            modules.pop()
        declared = "::".join(modules)
        prefix = selected[:-2]
        return bool(prefix) and (declared == prefix or declared.startswith(prefix + "::"))
    return selected in name or name.startswith(selected.split("::")[-1])


def main() -> int:
    filters, unfiltered = ci_filters()
    tests = ignored_tests()
    uncovered = {
        name: where
        for name, where in tests.items()
        if name not in EXCUSED
        and Path(where).stem not in unfiltered
        and not any(matches_ci_filter(name, where, f) for f in filters)
    }
    missing_windows, window_groups = window_coverage(WORKFLOW.read_text())
    uncovered.update(missing_windows)
    stale = sorted(set(EXCUSED) - set(tests))

    print(
        f"{len(tests)} ignored tests, {len(filters)} name filters, "
        f"{len(unfiltered)} unfiltered targets, {len(EXCUSED)} excused"
    )
    print(f"Custom window scenario groups: {[len(group) for group in window_groups]}")
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
