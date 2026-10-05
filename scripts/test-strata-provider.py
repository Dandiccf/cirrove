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
        self.mutations = []
        self.oversized = 0
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
                if len(line.rstrip(b"\n")) > 8192:
                    self.oversized += 1
                    continue
                verb, _, body = line.decode().strip().partition(" ")
                body = json.loads(body) if body else None
                self.requests.append((verb, body))
                if verb == "capabilities":
                    reply = {"capabilities": {"paths-cached": 1, "pin-identity": 1} if self.capable else {"paths": 1}}
                elif verb == "status":
                    reply = {"protocol_version": 1, "accounts": self.accounts}
                elif verb == "paths-cached":
                    reply = {"states": [dict(self.rows.get(p, {"refusal": "missing"}), path=p) for p in body["paths"]]}
                else:
                    reply = self.mutations.pop(0) if self.mutations else {"accepted": True}
                    if reply is None:
                        continue
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
        result = dict({"item": "id", "kind": "file", "size": 10, "resident": 0, "pinned": None, "can_pin": True}, **changes)
        result["identity"] = {"item": result["item"], "mount": "fixture-mount", "scope": {"account": "one", "provider": "fixture", "collection": "drive"}}
        return result
    def ask(self, method="menu", paths=None, **extra):
        if method == "activate" and "context" not in extra:
            actions = self.ask("menu", paths)["actions"]
            extra["context"] = next((a["context"] for a in actions if a["id"] == extra.get("action")), None)
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
        self.assertEqual(self.daemon.requests[-1], ("pin", {"label":"Cloud","path":path,"recursive":False,"expected":self.daemon.rows[path]["identity"]}))
        self.daemon.rows[path]["pinned"] = "inherited"
        self.ask("activate", ["/cloud/" + path], action="unpin")
        self.assertNotIn("unpin", [v for v,_ in self.daemon.requests])
    def test_background_is_recursive_and_disappearance_clears_state(self):
        req = dict(version=1,id=8,method="activate",paths=["/cloud"],background=True,action="pin")
        shown = self.client.handle(dict(req, method="menu"))
        req["context"] = shown["actions"][0]["context"]
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

    def prime_subscription(self):
        for account in self.daemon.accounts:
            self.client.observe_event(dict(account, event="account", kept_generation=0, state="ready"))
            self.client.observe_event(dict(account, event="mount"))
        self.client.observe_event({"event": "ready"})
        self.emitted.clear()

    def test_unrelated_account_events_during_each_reply_preserve_menu_and_badge(self):
        self.daemon.accounts.append({"account_id": "two", "label": "Other", "mounted": True, "mount_path": "/other"})
        self.daemon.rows["file"] = self.state(pinned="direct", resident=10)
        self.prime_subscription()
        original = provider.exchange
        generation = 0
        def during(address, verb, body=None, **kwargs):
            nonlocal generation
            result = original(address, verb, body, **kwargs)
            if verb in ("status", "paths-cached"):
                generation += 1
                self.client.observe_event({"event": "account", "account_id": "two", "mounted": True,
                                           "kept_generation": generation, "state": "ready"})
            return result
        with patch.object(provider, "exchange", during):
            for _ in range(10):
                self.assertEqual(self.ids(), ["unpin", "availability"])
                self.assertEqual(self.ask("query")["decorations"][0]["badge"], "kept")
        self.assertTrue(self.emitted)
        self.assertTrue(all(event.get("paths") == ["/other"] for event in self.emitted))

    def test_relevant_account_mount_and_lost_history_events_reject_crossing_replies(self):
        for event, roots in [
            ({"event": "account", "account_id": "one", "mounted": True, "kept_generation": 1, "state": "ready"}, ["/cloud"]),
            ({"event": "mount", "account_id": "one", "mounted": True, "mount_path": "/moved"}, ["/cloud", "/moved"]),
            ({"event": "account_removed", "account_id": "one"}, ["/cloud"]),
            ({"event": "account", "account_id": "unknown", "mounted": True, "kept_generation": 1}, None),
            ({"event": "lagged"}, None),
        ]:
            with self.subTest(event=event):
                self.client = provider.Provider(self.daemon.path, self.emitted.append)
                self.prime_subscription()
                original = provider.exchange
                def during(address, verb, body=None, **kwargs):
                    result = original(address, verb, body, **kwargs)
                    if verb == "paths-cached":
                        self.client.observe_event(event)
                    return result
                with patch.object(provider, "exchange", during):
                    self.assertEqual(self.ask("query")["decorations"], [])
                self.assertEqual(self.emitted[-1].get("paths"), roots)

    def test_scoped_invalidation_covers_nested_mounts_and_promotes_bounded_history(self):
        original = provider.exchange
        for root, expected in [("/cloud/nested", []), ("/cloudish", ["pin", "availability"])]:
            with self.subTest(root=root):
                def during(address, verb, body=None, **kwargs):
                    result = original(address, verb, body, **kwargs)
                    if verb == "paths-cached":
                        self.client.invalidate([root])
                    return result
                with patch.object(provider, "exchange", during):
                    self.assertEqual(self.ids(paths=["/cloud"]), expected)
        self.emitted.clear()
        for index in range(provider.LIMIT + 1):
            self.client.invalidate([f"/account-{index}"])
        self.assertTrue(any("paths" not in event for event in self.emitted))
        self.assertEqual(self.ids(), ["pin", "availability"])
    def test_selection_bound_and_daemon_absence_fail_closed(self):
        self.assertEqual(self.ids(paths=[f"/cloud/{i}" for i in range(201)]),[])
        self.client.address += "-missing"
        self.assertEqual(self.ids(),[])
    def test_unsupported_pin_keeps_availability_and_no_pin_action(self):
        self.daemon.rows["file"]["can_pin"] = False
        self.assertEqual(self.ids(),["availability"])

    def test_explicit_refusal_reports_confirmed_rejection(self):
        self.daemon.mutations = [{"accepted": False, "refusal": "cache budget exhausted"}]
        result = self.ask("activate", action="pin")
        self.assertEqual(result["outcome"], {"status": "rejected", "accepted": 0, "total": 1})
        self.assertIn("cache budget exhausted", result["message"])
        self.assertEqual(sum(v == "pin" for v, _ in self.daemon.requests), 1)

    def test_partial_refusal_and_lost_reply_preserve_confirmed_count(self):
        self.daemon.rows["other"] = self.state(item="other")
        paths = ["/cloud/file", "/cloud/other"]
        self.daemon.mutations = [{"accepted": True}, {"accepted": False, "refusal": "cache budget exhausted"}]
        result = self.ask("activate", paths, action="pin")
        self.assertEqual(result["outcome"], {"status": "partial", "accepted": 1, "total": 2})
        self.daemon.mutations = [{"accepted": True}, None]
        result = self.ask("activate", paths, action="pin")
        self.assertEqual(result["outcome"], {"status": "unknown", "accepted": 1, "total": 2})
        self.assertEqual(sum(v == "pin" for v, _ in self.daemon.requests), 4)

    def test_failed_connection_distinguishes_unsent_remainder_from_lost_reply(self):
        self.daemon.rows["other"] = self.state(item="other")
        original = provider.exchange
        for confirmed, expected in [(0, "rejected"), (1, "partial")]:
            with self.subTest(confirmed=confirmed):
                remaining = confirmed
                before = sum(v == "pin" for v, _ in self.daemon.requests)
                def disconnected(address, verb, body=None, **kwargs):
                    nonlocal remaining
                    if verb == "pin":
                        if remaining == 0:
                            address += "-gone"
                        else:
                            remaining -= 1
                    return original(address, verb, body, **kwargs)
                with patch.object(provider, "exchange", disconnected):
                    result = self.ask("activate", ["/cloud/file", "/cloud/other"], action="pin")
                self.assertEqual(result["outcome"], {"status": expected, "accepted": confirmed, "total": 2})
                self.assertIn("not sent", result["message"])
                self.assertEqual(sum(v == "pin" for v, _ in self.daemon.requests) - before, confirmed)

    def test_whole_selection_metadata_is_batched_to_real_control_frame_limit(self):
        names = [f"{i}-" + '"\\\n' + "ü" * 90 for i in range(200)]
        self.daemon.rows.update({name: self.state(item=str(i)) for i, name in enumerate(names)})
        paths = ["/cloud/" + name for name in names]
        self.assertEqual(self.ids(paths=paths), ["pin", "availability"])
        rows = self.ask("query", paths)["decorations"]
        self.assertEqual({row["path"] for row in rows}, set(paths))
        availability = self.ask("activate", paths, action="availability")["message"]
        self.assertIn(names[0] + ": On demand", availability)
        self.assertLessEqual(len(availability.encode("utf-8")), 16000)
        self.assertEqual(self.daemon.oversized, 0)

    def test_old_menu_cannot_pin_a_replaced_object_or_mount(self):
        for change in ["item", "mount", "account"]:
            with self.subTest(change=change):
                self.daemon.rows["file"] = self.state()
                self.daemon.accounts[0]["account_id"] = "one"
                action = self.ask("menu")["actions"][0]
                context = action["context"]
                if change == "account":
                    self.daemon.accounts[0]["account_id"] += "-new"
                    self.daemon.rows["file"]["identity"]["scope"]["account"] = self.daemon.accounts[0]["account_id"]
                else:
                    self.daemon.rows["file"]["identity"][change] += "-new"
                    if change == "item":
                        self.daemon.rows["file"]["item"] = self.daemon.rows["file"]["identity"]["item"]
                result = self.ask("activate", action="pin", context=context)
                self.assertEqual(result["outcome"]["status"], "rejected")
                self.assertFalse(any(v == "pin" for v, _ in self.daemon.requests))

    def test_menu_keeps_availability_when_guarded_action_exceeds_control_limit(self):
        name = "/".join(["ü" * 96] * 14)
        self.daemon.rows[name] = self.state()
        paths = ["/cloud/" + name]
        self.assertEqual(self.ids(paths=paths), ["availability"])
        result = self.ask("activate", paths, action="pin")
        self.assertEqual(result["outcome"]["status"], "rejected")
        self.assertFalse(any(v == "pin" for v, _ in self.daemon.requests))
        self.assertEqual(self.daemon.oversized, 0)

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
