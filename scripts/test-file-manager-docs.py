#!/usr/bin/env python3
"""Hold the file-manager promise to what the code actually offers.

M5 line 292 carries one rule with teeth: no control and no state may be
reachable only through one file manager. Everything either file-manager
integration can do has to be reachable from the command line too, or a person
using another manager loses something.

This asserts the rule rather than the prose. Every user-facing daemon verb the
Files and Dolphin integrations speak must also be a verb the CLI speaks.

The lighter checks are that the decision and the published list still exist:
a deferral nobody can find is indistinguishable from neglect.
"""

import re
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXTENSION = ROOT / "packaging/nautilus/cirrove.py"
DOLPHIN = sorted((ROOT / "packaging/dolphin").glob("*.cpp"))
DESKTOP = ROOT / "docs/desktop.md"
ADR = ROOT / "docs/adr/0010-file-managers-and-dolphin.md"
CLI = ROOT / "crates/cirrove-service/src/bin/cirrove.rs"

# The extension sends a verb as the first word of a line. Both the blocking and
# the asynchronous path build that line the same way, so one pattern finds
# every call site.
VERB = re.compile(r'request(?:_async)?\(\s*(?:"(\w+)"|(\w+)\s+if\s+\w+\s+else\s+"(\w+)")')
# A second form: the conditional above yields two verbs, both of which count.
CONDITIONAL = re.compile(r'"(\w+)"\s+if\s+\w+\s+else\s+"(\w+)"')


def extension_verbs():
    text = EXTENSION.read_text()
    verbs = set()
    for line in text.splitlines():
        if "request(" not in line and "request_async(" not in line:
            continue
        for a, b in CONDITIONAL.findall(line):
            verbs.update({a, b})
        for match in re.findall(r'request(?:_async)?\(\s*"(\w+)"', line):
            verbs.add(match)
    return verbs


def cli_verbs():
    """The CLI's subcommands, as clap renders them in --help."""
    out = subprocess.run(
        ["cargo", "run", "-q", "--bin", "cirrove", "--", "--help"],
        cwd=ROOT, capture_output=True, text=True, timeout=600,
    )
    if out.returncode != 0:
        raise AssertionError(f"cirrove --help failed:\n{out.stderr[-2000:]}")
    verbs = set()
    inside = False
    for line in out.stdout.splitlines():
        if line.startswith("Commands:"):
            inside = True
            continue
        if inside:
            if not line.startswith("  "):
                break
            word = line.strip().split(" ", 1)[0]
            if word:
                verbs.add(word)
    return verbs


def dolphin_verbs():
    """Queries and actions exposed by the KF6 plugins.

    `subscribe` is an invalidation channel, not a user-visible control or state
    of its own, so it does not belong to this parity check.
    """
    source = "\n".join(path.read_text() for path in DOLPHIN)
    return set(re.findall(r'QStringLiteral\("(status|paths|pin|unpin)"\)', source))


class FileManagerPromise(unittest.TestCase):
    def test_the_extension_speaks_no_verb_the_command_line_lacks(self):
        extension = extension_verbs() | dolphin_verbs()
        self.assertTrue(extension, "found no verbs in the extension; the pattern has rotted")
        cli = cli_verbs()
        only_in_files = sorted(extension - cli)
        self.assertEqual(
            only_in_files, [],
            "these are reachable from Files but not from the command line, which "
            "breaks M5's rule that no control or state may be reachable only "
            "through one file manager -- and with it the Dolphin deferral in "
            f"ADR 0010: {only_in_files}",
        )

    def test_the_dolphin_decision_is_findable(self):
        self.assertTrue(ADR.exists(), "ADR 0010 is gone; the Dolphin decision is unrecorded")
        self.assertIn(
            "adr/0010-file-managers-and-dolphin.md", DESKTOP.read_text(),
            "the desktop notes no longer point at the Dolphin decision",
        )

    def test_the_access_guarantee_is_stated(self):
        text = DESKTOP.read_text()
        self.assertIn(
            "Every file manager can use the mount", text,
            "M5 line 292 requires saying that unsupported managers still reach "
            "mounted files; that sentence is gone",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
