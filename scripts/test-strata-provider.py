#!/usr/bin/python3
"""Behavioral protocol tests; no GTK, cloud credentials or real mount needed."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

provider = load("provider", "packaging/strata/cirrove-provider.py")
installer = load("installer", "scripts/install-strata.py")

class Daemon:
    def __init__(self, path):
        self.path = str(path)
        self.accounts = [{"account_id": "one", "label": "Cloud", "mounted": True, "mount_path": "/cloud"}]
        self.rows = {}
        self.requests = []
        self.capable = True
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(self.path)
        self.server.listen()
        self.server.settimeout(0.1)
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.run)
        self.thread.start()
    def run(self):
        while not self.stopped.is_set():
            try:
                client, _ = self.server.accept()
            except socket.timeout:
                continue
            with client:
                line = client.makefile("rb").readline()
                verb, _, body = line.decode().strip().partition(" ")
                body = json.loads(body) if body else None
                self.requests.append((verb, body))
                if verb == "capabilities":
                    reply = {"capabilities": {"paths-cached": 1} if self.capable else {"paths": 1}}
                elif verb == "status":
                    reply = {"protocol_version": 1, "accounts": self.accounts}
                elif verb == "paths-cached":
                    reply = {"states": [dict(self.rows.get(p, {"refusal": "missing"}), path=p) for p in body["paths"]]}
                else:
                    reply = {"accepted": True}
                client.sendall(json.dumps(reply).encode())
    def close(self):
        self.stopped.set()
        self.thread.join(2)
        self.server.close()

class Protocol(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.daemon = Daemon(Path(self.temp.name) / "socket")
        self.emitted = []
        self.client = provider.Provider(self.daemon.path, self.emitted.append)
        self.daemon.rows = {"file": self.state(), "folder": self.state(kind="folder"), "": self.state(kind="folder")}
    def tearDown(self):
        self.daemon.close()
        self.temp.cleanup()
    def state(self, **changes):
        return dict({"item": "id", "kind": "file", "size": 10, "resident": 0, "pinned": None, "can_pin": True}, **changes)
    def ask(self, method="menu", paths=None, **extra):
        return self.client.handle(dict(version=1, id=1, method=method, paths=paths or ["/cloud/file"], background=False, **extra))
    def ids(self, **kw):
        return [a["id"] for a in self.ask(**kw)["actions"]]
    def test_direct_inherited_and_mixed_pins(self):
        self.assertEqual(self.ids(), ["pin", "availability"])
        self.daemon.rows["file"]["pinned"] = "direct"
        self.assertEqual(self.ids(), ["unpin", "availability"])
        self.daemon.rows["file"]["pinned"] = "inherited"
        self.assertEqual(self.ids(), ["pin", "availability"])
        self.assertEqual(self.ids(paths=["/cloud/file", "/cloud/folder"]), ["pin", "availability"])
    def test_mixed_unrelated_missing_and_cross_account_selections_are_silent(self):
        for paths in [["/cloud/file", "/elsewhere"], ["/cloudish/file"], ["/cloud/missing"], ["/cloud/../file"]]:
            self.assertEqual(self.ids(paths=paths), [])
        self.daemon.accounts.append({"account_id": "two", "label": "Nested", "mounted": True, "mount_path": "/cloud/nested"})
        self.assertEqual(self.ids(paths=["/cloud/file", "/cloud/nested/file"]), [])
        self.assertEqual(provider.locate(self.daemon.accounts,"/cloud/nested/file")[0]["label"], "Nested")
    def test_badges_follow_bytes_and_pins_not_extensions(self):
        for pin, resident, expected in [(None, 10, None), ("direct", 2, "fetching"), ("inherited", 10, "kept")]:
            self.daemon.rows["file"] = self.state(pinned=pin,resident=resident)
            self.assertEqual(self.ask("query")["decorations"][0]["badge"], expected)
        self.daemon.rows["file"] = {"refusal":"unknown"}
        self.assertEqual(self.ask("query")["decorations"], [])
    def test_cached_capability_required_and_no_mutation_from_rendering(self):
        self.ask("query"); self.ask("menu")
        self.assertTrue(all(v in ("capabilities","status","paths-cached") for v,_ in self.daemon.requests))
        self.daemon.capable = False
        self.client.invalidate()
        self.assertEqual(self.ids(), [])
        self.assertNotIn("paths", [v for v,_ in self.daemon.requests])
    def test_activation_rechecks_state_and_preserves_literal_names(self):
        path = "quotes ' $(echo bad)\nü.txt"
        self.daemon.rows[path] = self.state()
        result = self.ask("activate", ["/cloud/" + path], action="pin")
        self.assertIn("accepted 1 of 1",result["message"])
        self.assertEqual(self.daemon.requests[-1], ("pin", {"label":"Cloud","path":path,"recursive":False}))
        self.daemon.rows[path]["pinned"] = "inherited"
        self.ask("activate", ["/cloud/" + path], action="unpin")
        self.assertNotIn("unpin", [v for v,_ in self.daemon.requests])
    def test_background_is_recursive_and_disappearance_clears_state(self):
        req = dict(version=1,id=8,method="activate",paths=["/cloud"],background=True,action="pin")
        self.client.handle(req)
        self.assertEqual(self.daemon.requests[-1][1]["recursive"],True)
        self.daemon.accounts[0]["mounted"] = False
        self.client.invalidate()
        self.assertEqual(self.ask("query")["decorations"],[])
    def test_generation_change_during_reply_rejects_old_badge(self):
        original = provider.exchange
        def during(address, verb, body=None):
            result = original(address,verb,body)
            if verb == "paths-cached": self.client.invalidate()
            return result
        with patch.object(provider,"exchange",during):
            self.assertEqual(self.ask("query")["decorations"],[])
    def test_selection_bound_and_daemon_absence_fail_closed(self):
        self.assertEqual(self.ids(paths=[f"/cloud/{i}" for i in range(201)]),[])
        self.client.address += "-missing"
        self.assertEqual(self.ids(),[])
    def test_unsupported_pin_keeps_availability_and_no_pin_action(self):
        self.daemon.rows["file"]["can_pin"] = False
        self.assertEqual(self.ids(),["availability"])

class Installation(unittest.TestCase):
    def test_install_update_and_remove_only_owned_files(self):
        with tempfile.TemporaryDirectory() as root:
            config = Path(root) / "config"
            installer.install(config)
            target=config / "strata/providers/cirrove"
            self.assertTrue((target/"provider.json").exists())
            installer.install(config)
            (config/"strata/preferences.json").write_text("keep me")
            installer.install(config,remove=True)
            self.assertFalse(target.exists())
            self.assertEqual((config/"strata/preferences.json").read_text(),"keep me")
    def test_user_modified_or_symlink_installation_is_preserved(self):
        with tempfile.TemporaryDirectory() as root:
            config=Path(root)/"config"
            installer.install(config)
            target=config/"strata/providers/cirrove"
            (target/"cirrove-provider.py").write_text("modified")
            with self.assertRaises(ValueError): installer.install(config,remove=True)
            self.assertTrue(target.exists())
            other=Path(root)/"other";other.mkdir();(other/"strata").symlink_to(config/"strata")
            with self.assertRaises(ValueError): installer.install(other)
            with self.assertRaises(ValueError): installer.install(other,remove=True)

if __name__ == "__main__": unittest.main()
