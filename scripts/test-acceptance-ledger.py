#!/usr/bin/env python3
"""Synthetic scope checks; never read or write the repository's real ledger."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "acceptance_ledger", Path(__file__).with_name("acceptance-ledger.py"))
LEDGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LEDGER)


class ReleaseScopeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="cirrove-ledger-test-")
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.source = root / "milestones.md"
        self.ledger = root / "ledger.json"
        self.source.write_text("## 1. Historical foundation\n- [x] Historical accepted behavior.\n")
        self.paths = patch.multiple(LEDGER, MILESTONES=self.source, LEDGER=self.ledger)
        self.paths.start()
        self.addCleanup(self.paths.stop)
        self.write_rows()

    def write_rows(self):
        rows = []
        for box in LEDGER.boxes():
            rows.append({**box, "blocker": "autonomous",
                         "blocks_release": None if box["done"] else "yes",
                         "release_note": None, "evidence": "synthetic evidence",
                         "asserted_by": "synthetic assertion" if box["done"] else None})
        self.ledger.write_text(json.dumps({"rows": rows, "totals": {
            "boxes": len(rows), "ticked": sum(r["done"] for r in rows)}}))

    def add_icloud(self):
        with self.source.open("a") as stream:
            stream.write("\n<!-- acceptance-release-scope: full-icloud -->\n"
                         "## 7. Full iCloud acceptance\n"
                         "- [ ] Native replacement remains open.\n"
                         "- [ ] Installed recovery remains open.\n")
        self.write_rows()

    def invoke(self, *args):
        output = io.StringIO()
        with patch.object(sys, "argv", ["acceptance-ledger.py", *args]), contextlib.redirect_stdout(output):
            result = LEDGER.main()
        return result, output.getvalue()

    def test_legacy_rows_without_scope_keep_onedrive_meaning(self):
        self.assertNotIn("release_scope", json.loads(self.ledger.read_text())["rows"][0])
        result, output = self.invoke()
        self.assertEqual(result, 0)
        self.assertIn("0 open row(s) block OneDrive 1.0", output)
        self.assertIn("0 open row(s) block full iCloud", output)

    def test_separate_scope_reports_blockers_even_with_zero_historical_blockers(self):
        self.add_icloud()
        result, output = self.invoke("--blockers")
        self.assertEqual(result, 0)
        self.assertIn("0 open row(s) block OneDrive 1.0", output)
        self.assertIn("2 open row(s) block full iCloud", output)
        historical, full = output.split("What stands between this and full iCloud:")
        self.assertNotIn("M7", historical)
        self.assertIn("M7  Native replacement remains open.", full)
        self.assertIn("M7  Installed recovery remains open.", full)

    def test_row_cannot_silently_reassign_source_scope(self):
        self.add_icloud()
        value = json.loads(self.ledger.read_text())
        value["rows"][1]["release_scope"] = "onedrive-1.0"
        self.ledger.write_text(json.dumps(value))
        result, output = self.invoke()
        self.assertEqual(result, 1)
        self.assertIn("disagree on release scope", output)
        value["rows"][1]["release_scope"] = "unknown-release"
        self.ledger.write_text(json.dumps(value))
        result, output = self.invoke()
        self.assertEqual(result, 1)
        self.assertIn("unknown release scope", output)

    def test_sync_retains_scope_evidence_and_legacy_defaults(self):
        self.add_icloud()
        result, _ = self.invoke("--sync")
        self.assertEqual(result, 0)
        rows = json.loads(self.ledger.read_text())["rows"]
        self.assertNotIn("release_scope", rows[0])
        self.assertEqual(rows[1]["release_scope"], "full-icloud")
        self.assertEqual(rows[1]["blocks_release"], "yes")
        self.assertEqual(rows[1]["evidence"], "synthetic evidence")
        self.assertFalse(rows[1]["done"])
        self.assertIsNone(rows[1]["asserted_by"])


if __name__ == "__main__":
    unittest.main()
