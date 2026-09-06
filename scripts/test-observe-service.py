#!/usr/bin/env python3
"""Synthetic checks for private service-health observation output."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("observe", Path(__file__).with_name("observe-service.py"))
observe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observe)


class ObservationTests(unittest.TestCase):
    def test_private_identity_and_provider_details_are_not_copied(self):
        state = {"accounts": [{"account": "PRIVATE-ID", "drive": "PRIVATE-ID", "mounted": True,
                  "state": "ready", "mount_path": "/PRIVATE-ID", "feeds": [
                  {"state": "ready", "collection": "PRIVATE-ID", "message": "PRIVATE-ID"}]}]}
        report = observe.snapshot(state)
        self.assertNotIn("PRIVATE-ID", json.dumps(report))
        self.assertTrue(report["ready"])
        self.assertEqual(report["mounted_accounts"], 1)
        state["accounts"][0]["state"] = "PRIVATE-ID"
        self.assertNotIn("PRIVATE-ID", json.dumps(observe.snapshot(state)))
        self.assertFalse(observe.snapshot(state)["ready"])

    def test_absent_cli_does_not_claim_success_or_copy_error_text(self):
        with tempfile.TemporaryDirectory() as root:
            self.assertEqual(observe.sample(Path(root) / "absent"), {"reachable": False, "ready": False})
        self.assertFalse(observe.snapshot({"accounts": []})["ready"])


if __name__ == "__main__":
    unittest.main()
