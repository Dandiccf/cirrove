#!/usr/bin/env python3
"""Every string a user reads must be translatable, and every translation must
keep the values the sentence was built to carry.

Two failures this guards against. A string added to the window and never marked
is one nobody can translate, and nothing else would ever say so -- the program
works perfectly and quietly stays English. And a translation that drops a `{}`
turns "Bericht.docx is no longer kept offline" into a sentence with no file in
it, which is worse than the English.

The template is regenerated from the source here rather than trusted, because a
stale template is exactly how the first failure hides.
"""

import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PO = sorted((ROOT / "po").glob("*.po"))
SOURCES = sorted((ROOT / "crates/cirrove-desktop/src").rglob("*.rs"))


def entries(path):
    """msgid -> msgstr, both joined across continuation lines."""
    text = Path(path).read_text()
    found = {}
    for block in re.finditer(
        r'^msgid ((?:"(?:[^"\\]|\\.)*"\s*)+)^msgstr ((?:"(?:[^"\\]|\\.)*"\s*)+)',
        text,
        re.M,
    ):
        join = lambda part: "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', part))
        key = join(block.group(1))
        if key:
            found[key] = join(block.group(2))
    return found


def extract_from_source():
    with tempfile.NamedTemporaryFile(suffix=".pot") as pot:
        subprocess.run(
            ["xgettext", "--language=C", "--from-code=UTF-8",
             "--keyword=gettext", "--keyword=n", "-o", pot.name, *map(str, SOURCES)],
            check=True, capture_output=True,
        )
        return entries(pot.name)


@unittest.skipUnless(shutil.which("xgettext"), "needs gettext tools")
class TranslationsAreCompleteAndKeepTheirValues(unittest.TestCase):
    def test_the_template_matches_what_the_program_actually_says(self):
        in_source = set(extract_from_source())
        in_template = set(entries(ROOT / "po/cirrove.pot"))
        missing = sorted(in_source - in_template)
        self.assertEqual(
            missing, [],
            "these strings are in the program but not in po/cirrove.pot, so nobody "
            "can translate them -- run scripts/build-translations.sh:\n  "
            + "\n  ".join(missing),
        )

    def test_every_catalogue_translates_everything_and_keeps_the_placeholders(self):
        self.assertTrue(PO, "no catalogues found")
        template = entries(ROOT / "po/cirrove.pot")
        for po in PO:
            catalogue = entries(po)
            untranslated, mangled = [], []
            for msgid in template:
                translation = catalogue.get(msgid, "")
                if not translation:
                    untranslated.append(msgid)
                elif translation.count("{}") != msgid.count("{}"):
                    mangled.append(f"{msgid!r} -> {translation!r}")
            self.assertEqual(
                untranslated, [],
                f"{po.name} leaves these untranslated:\n  " + "\n  ".join(untranslated),
            )
            self.assertEqual(
                mangled, [],
                f"{po.name} loses or invents a value placeholder:\n  " + "\n  ".join(mangled),
            )

    def test_no_message_id_carries_the_wreckage_of_a_line_continuation(self):
        """A `\\` continuation in a Rust literal does not survive extraction.

        Rust strips the newline *and* the next line's indentation; xgettext
        reads the file as C, which keeps the indentation. So a string written
        across two lines with a backslash is extracted with a run of spaces in
        the middle that the running program never looks up -- the entry sits in
        the catalogue, translated, and can never match. Two banner strings were
        exactly this, and nothing would have said so: the tests passed, the
        catalogue was complete, and the window stayed English.

        A run of three or more spaces inside a message id is the signature.
        """
        suspicious = [
            msgid for msgid in entries(ROOT / "po/cirrove.pot") if "   " in msgid
        ]
        self.assertEqual(
            suspicious, [],
            "these message ids contain a run of spaces, which almost always means a `\\` "
            "line continuation in the source that xgettext kept and Rust did not -- put "
            "the string on one line:\n  " + "\n  ".join(repr(m) for m in suspicious),
        )

    def test_no_catalogue_carries_a_fuzzy_guess(self):
        # A fuzzy entry is gettext's way of saying "this was matched to a
        # changed string and nobody has looked". It is shown to the user anyway.
        for po in PO:
            fuzzy = [l for l in po.read_text().splitlines() if l.startswith("#,") and "fuzzy" in l]
            self.assertEqual(fuzzy, [], f"{po.name} has {len(fuzzy)} fuzzy entries; review them")


if __name__ == "__main__":
    unittest.main(verbosity=2)
