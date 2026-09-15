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
    """Which state earns a badge, independent of which icons are installed.

    The name set is passed in on purpose. Reading the module's own would make
    these pass or fail by whether the machine running them happens to have
    Cirrove's icons in its home, which is a fact about the machine and not
    about the mapping under test. `Emblems` covers the choice between sets.
    """

    GENERIC = {"kept": "emblem-ok-symbolic", "fetching": "emblem-synchronizing-symbolic"}

    def test_on_demand_files_get_no_badge_and_say_so_in_the_column(self):
        state = {"kind": "file", "pinned": None, "size": 10, "resident": 0}
        self.assertIsNone(ext.emblem_for(state, self.GENERIC))
        self.assertEqual(ext.describe(state), "On demand")

    def test_a_pin_that_is_fetched_is_ok_and_one_still_fetching_is_synchronizing(self):
        kept = {"kind": "file", "pinned": "direct", "size": 10, "resident": 10}
        fetching = {"kind": "file", "pinned": "direct", "size": 10, "resident": 4}
        self.assertEqual(ext.emblem_for(kept, self.GENERIC), "emblem-ok-symbolic")
        self.assertEqual(ext.emblem_for(fetching, self.GENERIC), "emblem-synchronizing-symbolic")
        self.assertEqual(ext.describe(kept), "Kept offline")
        self.assertEqual(ext.describe(fetching), "Kept offline · fetching 40%")

    def test_inherited_pins_say_through_which_folder_and_folders_are_kept(self):
        via = {"kind": "file", "pinned": "inherited", "size": 10, "resident": 10}
        folder = {"kind": "folder", "pinned": "direct", "size": 0, "resident": 0}
        self.assertEqual(ext.describe(via), "Kept offline (folder)")
        self.assertEqual(ext.emblem_for(folder, self.GENERIC), "emblem-ok-symbolic")
        self.assertEqual(ext.describe(folder), "Kept offline")

    def test_a_refusal_draws_nothing_and_says_nothing(self):
        state = {"path": "x", "refusal": "no such path"}
        self.assertIsNone(ext.emblem_for(state, self.GENERIC))
        self.assertEqual(ext.describe(state), "")


class Emblems(unittest.TestCase):
    """Which icon names the badges use, and what happens when ours are absent."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.theme = pathlib.Path(self.temp.name) / "icons/hicolor/scalable/apps"
        self.theme.mkdir(parents=True)

    def tearDown(self):
        self.temp.cleanup()

    def install_ours(self):
        for name in ("kept", "fetching"):
            (self.theme / f"{ext.APP_ID}-{name}.svg").write_text("<svg/>")

    def test_cirroves_own_emblems_are_used_when_they_are_installed(self):
        self.install_ours()
        names = ext.emblem_names([self.theme.parent.parent.parent])
        self.assertEqual(names["kept"], f"{ext.APP_ID}-kept")
        self.assertEqual(names["fetching"], f"{ext.APP_ID}-fetching")

    def test_the_freedesktop_ones_are_used_when_ours_are_not_installed(self):
        # An icon name nothing provides draws nothing at all, so the badge would
        # simply vanish. A generic tick is worse than ours and far better than
        # no badge.
        names = ext.emblem_names([self.theme.parent.parent.parent])
        self.assertEqual(names["kept"], "emblem-ok-symbolic")
        self.assertEqual(names["fetching"], "emblem-synchronizing-symbolic")

    def test_a_kept_file_that_is_here_and_one_still_arriving_get_different_badges(self):
        self.install_ours()
        names = ext.emblem_names([self.theme.parent.parent.parent])
        here = {"kind": "file", "pinned": "direct", "size": 10, "resident": 10}
        arriving = {"kind": "file", "pinned": "direct", "size": 10, "resident": 4}
        folder = {"kind": "folder", "pinned": "direct", "size": 0, "resident": 0}
        self.assertEqual(ext.emblem_for(here, names), f"{ext.APP_ID}-kept")
        self.assertEqual(ext.emblem_for(arriving, names), f"{ext.APP_ID}-fetching")
        self.assertEqual(ext.emblem_for(folder, names), f"{ext.APP_ID}-kept")
        self.assertIsNone(ext.emblem_for({"kind": "file", "pinned": None}, names))

    def test_the_icons_this_names_are_the_ones_the_tree_ships(self):
        # The names and the files must agree, or the badge is silently absent.
        for name in ("kept", "fetching"):
            shipped = ROOT / f"packaging/icons/scalable/apps/{ext.APP_ID}-{name}.svg"
            self.assertTrue(shipped.is_file(), f"{shipped} is named but not shipped")


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


class Properties(unittest.TestCase):
    def test_a_kept_file_lists_availability_where_it_is_and_the_pin(self):
        state = {"kind": "file", "pinned": "direct", "size": 2048, "resident": 2048, "item": "01ABC"}
        pairs = dict(ext.properties(state, "Dokumente/Report.docx"))
        self.assertEqual(pairs["Availability"], "Kept offline")
        self.assertEqual(pairs["On this computer"], "all 2.0 KB")
        self.assertEqual(pairs["Kept offline"], "yes")
        self.assertEqual(pairs["Cloud location"], "/Dokumente/Report.docx")
        self.assertEqual(pairs["Provider item"], "01ABC")

    def test_an_on_demand_file_says_not_downloaded_and_not_pinned(self):
        state = {"kind": "file", "pinned": None, "size": 1000, "resident": 0}
        pairs = dict(ext.properties(state, "a.txt"))
        self.assertEqual(pairs["Availability"], "On demand")
        self.assertEqual(pairs["On this computer"], "not downloaded yet")
        self.assertEqual(pairs["Kept offline"], "no")

    def test_a_cached_but_unpinned_file_says_the_copy_is_not_guaranteed(self):
        # The contradiction this guards: "Availability: On demand" beside
        # "On this computer: all 66.2 KB" tells the reader nothing about
        # whether the copy will still be there tomorrow. Without a pin it can
        # be reclaimed, and the dialog has to say so.
        state = {"kind": "file", "pinned": None, "size": 67789, "resident": 67789}
        pairs = dict(ext.properties(state, "deck.pptx"))
        self.assertEqual(pairs["Availability"], "On demand")
        self.assertEqual(
            pairs["On this computer"], "all 66.2 KB, until the space is needed"
        )

    def test_a_pinned_file_that_is_here_does_not_hedge(self):
        # A pin is the promise; nothing may suggest it could go away.
        state = {"kind": "file", "pinned": "direct", "size": 67789, "resident": 67789}
        pairs = dict(ext.properties(state, "deck.pptx"))
        self.assertEqual(pairs["On this computer"], "all 66.2 KB")

    def test_a_partly_fetched_file_shows_how_much(self):
        state = {"kind": "file", "pinned": "direct", "size": 4096, "resident": 1024}
        pairs = dict(ext.properties(state, "x"))
        self.assertEqual(pairs["On this computer"], "1.0 KB of 4.0 KB")

    def test_a_folder_omits_on_this_computer_and_an_inherited_pin_says_via_folder(self):
        state = {"kind": "folder", "pinned": "inherited", "size": 0, "resident": 0}
        pairs = dict(ext.properties(state, "Dokumente"))
        self.assertNotIn("On this computer", pairs)
        self.assertEqual(pairs["Kept offline"], "yes, through a folder above")

    def test_a_refused_or_missing_state_shows_no_section(self):
        self.assertEqual(ext.properties({"refusal": "no such path"}, "x"), [])
        self.assertEqual(ext.properties({}, "x"), [])


class StayingCurrent(unittest.TestCase):
    """What the daemon's event stream makes Files ask again about.

    The defect this answers: Files keeps the answer it got when it listed a
    directory, so a pin made in the Cirrove window left an open Files window
    showing the opposite of the truth for as long as it was left open.
    """

    @staticmethod
    def account(label, kept, state="ready"):
        return {
            "event": "account",
            "account_id": f"id-{label}",
            "label": label,
            "state": state,
            "enabled": True,
            "mounted": True,
            "kept_generation": kept,
        }

    def primed(self, *accounts):
        watch = ext.Refreshes()
        for account in accounts:
            self.assertEqual(watch.observe(account), set())
        self.assertEqual(watch.observe({"event": "ready"}), set())
        return watch

    def test_priming_is_not_a_change(self):
        # A subscription opens with current state. A client that treated those
        # as changes would refresh every open window every time it reconnected.
        watch = self.primed(self.account("work", 7), self.account("home", 2))
        self.assertEqual(watch.observe({"event": "ready"}), set())

    def test_what_is_kept_changing_asks_only_that_accounts_entries_again(self):
        watch = self.primed(self.account("work", 7), self.account("home", 2))
        self.assertEqual(watch.observe(self.account("work", 8)), {"work"})
        self.assertEqual(watch.observe(self.account("home", 2)), set())

    def test_an_account_change_that_keeps_nothing_new_asks_nothing(self):
        # The account event travels for several reasons -- a state change, a
        # refused save. Only what is kept offline changes a badge.
        watch = self.primed(self.account("work", 7))
        self.assertEqual(
            watch.observe(self.account("work", 7, state="sign_in_required")), set()
        )

    def test_an_older_daemon_that_does_not_count_asks_nothing(self):
        # No kept_generation reads back as zero, which never moves. An older
        # daemon leaves the extension exactly as it was, rather than refreshing
        # every listing on every event.
        watch = self.primed({"event": "account", "label": "work", "state": "ready"})
        self.assertEqual(
            watch.observe({"event": "account", "label": "work", "state": "ready"}), set()
        )

    def test_a_lagged_stream_treats_everything_as_stale_once(self):
        # Intermediate states were dropped, so nothing held is known to be
        # current; the re-priming that follows is a record, not a change.
        watch = self.primed(self.account("work", 7), self.account("home", 2))
        self.assertEqual(watch.observe({"event": "lagged", "dropped": 9}), {"work", "home"})
        self.assertEqual(watch.observe(self.account("work", 40)), set())
        self.assertEqual(watch.observe({"event": "ready"}), set())
        self.assertEqual(watch.observe(self.account("work", 41)), {"work"})

    def test_an_event_this_build_does_not_know_is_ignored(self):
        watch = self.primed(self.account("work", 7))
        self.assertEqual(watch.observe({"event": "something-newer"}), set())
        self.assertEqual(watch.observe({}), set())


class Remembering(unittest.TestCase):
    """What the extension holds on to so it can refresh it."""

    def test_only_the_named_accounts_entries_come_back(self):
        shown = ext.Shown()
        shown.remember("work", "a.txt", "A")
        shown.remember("home", "b.txt", "B")
        self.assertEqual(shown.under("work"), ["A"])
        self.assertEqual(shown.under("home"), ["B"])
        self.assertEqual(shown.under("nothing"), [])

    def test_a_file_seen_again_is_held_once_with_the_newer_handle(self):
        shown = ext.Shown()
        shown.remember("work", "a.txt", "first")
        shown.remember("work", "a.txt", "second")
        self.assertEqual(shown.under("work"), ["second"])
        self.assertEqual(len(shown), 1)

    def test_the_registry_is_bounded_and_drops_the_oldest(self):
        # Files never says a file has gone, so an unbounded registry in a
        # process that lives as long as a login session is a leak with a
        # schedule.
        shown = ext.Shown(limit=3)
        for i in range(5):
            shown.remember("work", f"f{i}", i)
        self.assertEqual(len(shown), 3)
        self.assertEqual(shown.under("work"), [2, 3, 4])


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
