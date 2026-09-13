#!/usr/bin/env python3
"""Hold packaging/nautilus/cirrove.py to what it promises, without Files.

The extension's logic lives in plain functions above its GTK classes, so this
imports the file and drives those: where a path lands, what badge a state
earns, what the menu says, and -- against a daemon that is only a socket --
that a listing is asked for in batches of the daemon's ceiling and that a
daemon that refuses draws nothing rather than raising.
"""

import importlib.util
import json
import os
import pathlib
import socket
import sys
import tempfile
import threading
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCE = ROOT / "packaging/nautilus/cirrove.py"


def load():
    spec = importlib.util.spec_from_file_location("cirrove_nautilus", SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ext = load()


class FakeDaemon:
    """Answers `status`, `paths`, `pin` and `unpin`; records what it was asked."""

    def __init__(self, directory, status, states, refuse=False):
        self.path = os.path.join(directory, "control.sock")
        self.status = status
        self.states = states
        self.refuse = refuse
        self.requests = []
        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(self.path)
        self._server.listen(8)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self):
        while True:
            try:
                client, _ = self._server.accept()
            except OSError:
                return
            with client:
                line = b""
                while not line.endswith(b"\n"):
                    data = client.recv(4096)
                    if not data:
                        break
                    line += data
                text = line.decode().rstrip("\n")
                verb, _, body = text.partition(" ")
                self.requests.append(text)
                if self.refuse:
                    reply = {"refusal": "unknown control request"}
                elif verb == "status":
                    reply = self.status
                elif verb == "paths":
                    asked = json.loads(body)["paths"]
                    reply = {
                        "states": [
                            self.states.get(p, {"path": p, "refusal": "no such path"})
                            for p in asked
                        ]
                    }
                else:
                    reply = {"accepted": True, "item": "x"}
                client.sendall(json.dumps(reply).encode())

    def close(self):
        self._server.close()


class Locating(unittest.TestCase):
    def test_the_longest_mount_wins_and_the_root_is_the_empty_path(self):
        accounts = [("/home/a/Cloud", "outer"), ("/home/a/Cloud/inner", "inner")]
        self.assertEqual(ext.locate(accounts, "/home/a/Cloud/x.txt"), ("outer", "x.txt"))
        self.assertEqual(
            ext.locate(accounts, "/home/a/Cloud/inner/y.txt"), ("inner", "y.txt")
        )
        self.assertEqual(ext.locate(accounts, "/home/a/Cloud/inner"), ("inner", ""))
        self.assertEqual(ext.locate(accounts, "/home/a/Cloud"), ("outer", ""))
        self.assertIsNone(ext.locate(accounts, "/home/a/Cloudy/z"), "a prefix of the name is not the mount")
        self.assertIsNone(ext.locate(accounts, "/home/a"))

    def test_only_mounted_accounts_count(self):
        status = {
            "accounts": [
                {"mount_path": "/m/a", "label": "a", "mounted": True},
                {"mount_path": "/m/b", "label": "b", "mounted": False},
            ]
        }
        self.assertEqual(ext.mounted_accounts(status), [("/m/a", "a")])


class Badges(unittest.TestCase):
    def test_on_demand_files_get_no_badge_and_say_so_in_the_column(self):
        state = {"kind": "file", "pinned": None, "size": 10, "resident": 0}
        self.assertIsNone(ext.emblem_for(state))
        self.assertEqual(ext.describe(state), "On demand")

    def test_a_pin_that_is_fetched_is_ok_and_one_still_fetching_is_synchronizing(self):
        kept = {"kind": "file", "pinned": "direct", "size": 10, "resident": 10}
        fetching = {"kind": "file", "pinned": "direct", "size": 10, "resident": 4}
        self.assertEqual(ext.emblem_for(kept), "emblem-ok-symbolic")
        self.assertEqual(ext.emblem_for(fetching), "emblem-synchronizing-symbolic")
        self.assertEqual(ext.describe(kept), "Kept offline")
        self.assertEqual(ext.describe(fetching), "Kept offline · fetching 40%")

    def test_inherited_pins_say_through_which_folder_and_folders_are_kept(self):
        via = {"kind": "file", "pinned": "inherited", "size": 10, "resident": 10}
        folder = {"kind": "folder", "pinned": "direct", "size": 0, "resident": 0}
        self.assertEqual(ext.describe(via), "Kept offline (folder)")
        self.assertEqual(ext.emblem_for(folder), "emblem-ok-symbolic")
        self.assertEqual(ext.describe(folder), "Kept offline")

    def test_a_refusal_draws_nothing_and_says_nothing(self):
        state = {"path": "x", "refusal": "no such path"}
        self.assertIsNone(ext.emblem_for(state))
        self.assertEqual(ext.describe(state), "")


class Menu(unittest.TestCase):
    def test_all_pinned_offers_to_stop_and_anything_else_offers_to_keep(self):
        direct = {"pinned": "direct"}
        none = {"pinned": None}
        self.assertEqual(ext.menu_label([direct, direct], []), "Stop keeping offline")
        self.assertEqual(ext.menu_label([direct, none], []), "Keep offline")
        self.assertEqual(ext.menu_label([none], []), "Keep offline")
        # Inherited is not the item's own pin; un-keeping it would have to
        # touch the folder above, which the user did not select.
        self.assertEqual(ext.menu_label([{"pinned": "inherited"}], []), "Keep offline")

    def test_folders_are_named_as_folders(self):
        self.assertEqual(ext.menu_label([{"pinned": None}], ["one"]), "Keep folder offline")
        self.assertEqual(
            ext.menu_label([{"pinned": None}, {"pinned": None}], ["one", "two"]),
            "Keep folders offline",
        )


class AgainstADaemon(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        status = {"accounts": [{"mount_path": "/m/work", "label": "work", "mounted": True}]}
        states = {
            f"dir/f{i}": {"path": f"dir/f{i}", "kind": "file", "pinned": None, "size": 1, "resident": 0}
            for i in range(450)
        }
        states["dir/kept"] = {"path": "dir/kept", "kind": "file", "pinned": "direct", "size": 1, "resident": 1}
        self.daemon = FakeDaemon(self.temp.name, status, states)

    def tearDown(self):
        self.daemon.close()
        self.temp.cleanup()

    def test_a_listing_is_asked_for_in_batches_of_the_daemons_ceiling(self):
        entries = [(f"dir/f{i}", None) for i in range(450)] + [("dir/kept", None), ("dir/gone", None)]
        found = ext.states_for("work", entries, path=self.daemon.path)
        asked = [r for r in self.daemon.requests if r.startswith("paths ")]
        self.assertEqual(len(asked), 3, "452 paths at 200 per request is three requests")
        for line in asked:
            self.assertLessEqual(len(json.loads(line[6:])["paths"]), ext.PATHS_PER_REQUEST)
        self.assertEqual(found["dir/kept"]["pinned"], "direct")
        self.assertEqual(found["dir/gone"]["refusal"], "no such path")
        self.assertEqual(len(found), 452)

    def test_the_mount_list_comes_from_status_and_is_cached(self):
        mounts = ext.Mounts(self.daemon.path)
        self.assertEqual(mounts.locate("/m/work/dir/f1"), ("work", "dir/f1"))
        self.assertEqual(mounts.locate("/m/work/dir/f2"), ("work", "dir/f2"))
        self.assertEqual(
            sum(1 for r in self.daemon.requests if r == "status"),
            1,
            "two lookups inside the TTL are one status request",
        )

    def test_a_daemon_that_refuses_draws_nothing_rather_than_raising(self):
        self.daemon.refuse = True
        found = ext.states_for("work", [("dir/f1", None)], path=self.daemon.path)
        self.assertEqual(found, {})
        mounts = ext.Mounts(self.daemon.path)
        self.assertIsNone(mounts.locate("/m/work/dir/f1"))

    def test_no_daemon_at_all_is_the_same_as_a_refusing_one(self):
        missing = os.path.join(self.temp.name, "absent.sock")
        self.assertEqual(ext.states_for("work", [("a", None)], path=missing), {})
        self.assertIsNone(ext.Mounts(missing).locate("/m/work/a"))


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v"])
