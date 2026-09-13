"""Cirrove in Files: badges for what is kept offline, and the menu to change it.

Talks to the daemon's control socket the way the CLI does -- one line, one JSON
reply -- and asks it three things: ``status`` for where the drives are mounted,
``paths`` for the state of the files a listing shows, ``pin``/``unpin`` for the
menu. Files asks about a listing's entries one at a time, waiting for each answer,
so a badge costs one local round trip per file; ``paths`` takes many at once
for a client that can batch, and the menu uses it that way. Everything the
daemon does not answer degrades to no badge, never to a Files that hangs:
every call has a short timeout.

The badge work goes through GLib's asynchronous socket I/O, not Python
threads. Inside Files the interpreter is embedded, and a Python thread only
runs while the main thread is executing Python -- which it does only inside a
callback. A thread started from one ran for a moment and then stood still, the
socket reply in hand and no way to get the interpreter back. Gio callbacks are
main-thread callbacks and have it.

The functions above the classes carry the logic and know nothing of GTK; the
classes are the thin part Files calls. ``scripts/test-nautilus-extension.py``
holds the functions to what they promise, against a daemon that is only a
socket.
"""

import json
import os
import socket
import sys
import time
import traceback

SOCKET = os.path.join(
    os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}",
    "cirrove",
    "control.sock",
)
TIMEOUT = 3.0
# How long a status answer is trusted for the mount list. Mounts change when
# a person clicks Mount, which is not often; asking on every listing would be
# one more round trip in front of each.
STATUS_TTL = 5.0
# The daemon's ceiling per request. Larger listings go in several.
PATHS_PER_REQUEST = 200
ICON = "io.github.Dandiccf.Cirrove-symbolic"
# One line per listing to stderr when set; off by default because a line per
# directory opened is noise in a journal that is read for other reasons.
DEBUG = bool(os.environ.get("CIRROVE_NAUTILUS_DEBUG"))


def request(verb, body=None, path=None):
    """One exchange with the daemon: the verb, an optional JSON body, one reply."""
    line = verb if body is None else f"{verb} {json.dumps(body)}"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(TIMEOUT)
        stream.connect(path or SOCKET)
        stream.sendall((line + "\n").encode())
        chunks = []
        while True:
            data = stream.recv(65536)
            if not data:
                break
            chunks.append(data)
    return json.loads(b"".join(chunks))


def mounted_accounts(status):
    """``(mount_path, label)`` for every account the daemon has mounted."""
    return [
        (account["mount_path"], account["label"])
        for account in status.get("accounts", [])
        if account.get("mounted") and account.get("mount_path")
    ]


def locate(accounts, path):
    """The account a path lies in, and the path relative to its mount.

    The longest mount wins, so a drive mounted inside another's folder is not
    mistaken for a file of the outer one. The mount root itself is ``""``.
    """
    best = None
    for mount, label in accounts:
        mount = mount.rstrip("/")
        if path == mount:
            relative = ""
        elif path.startswith(mount + "/"):
            relative = path[len(mount) + 1 :]
        else:
            continue
        if best is None or len(mount) > len(best[0]):
            best = (mount, label, relative)
    return None if best is None else (best[1], best[2])


def emblem_for(state):
    """The badge a state earns, or None for the ordinary on-demand file.

    Most files in a cloud drive are on demand; badging them all would say
    nothing. A pin earns a mark, and the mark says whether the content is
    actually here: a pin the daemon has not fetched yet reserves space and
    keeps nothing, and a badge that called that "available" would be a lie the
    user finds out about on the train.
    """
    if state.get("refusal") or not state.get("pinned"):
        return None
    if state.get("kind") == "folder":
        return "emblem-ok-symbolic"
    if state.get("resident", 0) >= state.get("size", 0):
        return "emblem-ok-symbolic"
    return "emblem-synchronizing-symbolic"


def describe(state):
    """The words for the Cirrove column."""
    if state.get("refusal"):
        return ""
    pinned = state.get("pinned")
    if not pinned:
        return "On demand"
    via = " (folder)" if pinned == "inherited" else ""
    if state.get("kind") == "folder":
        return "Kept offline" + via
    if state.get("resident", 0) >= state.get("size", 0):
        return "Kept offline" + via
    fetched = state.get("resident", 0)
    total = max(state.get("size", 0), 1)
    return f"Kept offline{via} · fetching {100 * fetched // total}%"


def group_by_label(entries):
    """``[(label, relative, token)]`` -> ``{label: [(relative, token)]}``."""
    groups = {}
    for label, relative, token in entries:
        groups.setdefault(label, []).append((relative, token))
    return groups


def chunked(items, size):
    for start in range(0, len(items), size):
        yield items[start : start + size]


def states_for(label, entries, path=None):
    """Ask the daemon about these relatives; ``{relative: state}``.

    A request the daemon refuses as a whole (no such account, not running)
    yields nothing, which draws no badge. A path it refuses individually
    comes back with its refusal and draws none either.
    """
    found = {}
    for chunk in chunked(entries, PATHS_PER_REQUEST):
        try:
            reply = request(
                "paths", {"label": label, "paths": [relative for relative, _ in chunk]}, path
            )
        except (OSError, ValueError):
            continue
        for state in reply.get("states", []):
            found[state.get("path")] = state
    return found


def menu_label(states, folders):
    """What the one menu item should say for a selection.

    Every selected item pinned in its own right -> offer to stop. Anything
    else -> offer to keep, which also covers a mix: keeping what is already
    kept is harmless, while un-keeping something the user did not mean is a
    loss they notice offline.
    """
    if states and all(s.get("pinned") == "direct" for s in states):
        return "Stop keeping offline"
    if folders and len(states) == len(folders):
        return "Keep folder offline" if len(folders) == 1 else "Keep folders offline"
    return "Keep offline"


class Mounts:
    """The daemon's mount list, trusted for ``STATUS_TTL`` after each answer."""

    def __init__(self, socket_path=None):
        self._path = socket_path
        self._at = 0.0
        self._accounts = []

    def fresh(self):
        return time.monotonic() - self._at <= STATUS_TTL

    def cached(self):
        """What is known, without asking."""
        return list(self._accounts)

    def adopt(self, status):
        """Take a status answer, or None for a daemon that did not answer."""
        self._accounts = mounted_accounts(status) if status else []
        self._at = time.monotonic()

    def accounts(self):
        """The list, asked for synchronously when stale."""
        if not self.fresh():
            try:
                self.adopt(request("status", path=self._path))
            except (OSError, ValueError):
                self.adopt(None)
        return self.cached()

    def locate(self, path):
        return locate(self.accounts(), path)


# Everything below is what Files calls; nothing above needs it.
try:
    from gi import require_version

    require_version("Nautilus", "4.1")
    from gi.repository import Gio, GLib, GObject, Nautilus
except (ImportError, ValueError):  # pragma: no cover - the tests import without Files
    Nautilus = None


if Nautilus is not None:

    def request_async(verb, body, on_reply, path=None):
        """``request`` on GLib's loop: ``on_reply(dict)``, or ``on_reply(None)``
        for a daemon that did not answer, always from the main thread."""
        line = verb if body is None else f"{verb} {json.dumps(body)}"
        client = Gio.SocketClient.new()
        client.set_timeout(int(TIMEOUT))
        received = bytearray()

        def finish(reply):
            try:
                on_reply(reply)
            except Exception:  # noqa: BLE001 - a callback must not take Files down
                traceback.print_exc(file=sys.stderr)

        def read_more(stream, result, connection):
            try:
                data = stream.read_bytes_finish(result).get_data()
            except GLib.Error:
                finish(None)
                return
            if data:
                received.extend(data)
                stream.read_bytes_async(65536, GLib.PRIORITY_DEFAULT, None, read_more, connection)
                return
            try:
                connection.close(None)
            except GLib.Error:
                pass
            try:
                finish(json.loads(bytes(received)))
            except ValueError:
                finish(None)

        def connected(client, result):
            try:
                connection = client.connect_finish(result)
                # A short line into a local socket: the kernel takes it whole.
                connection.get_output_stream().write_all((line + "\n").encode(), None)
            except GLib.Error:
                finish(None)
                return
            connection.get_input_stream().read_bytes_async(
                65536, GLib.PRIORITY_DEFAULT, None, read_more, connection
            )

        client.connect_async(Gio.UnixSocketAddress.new(path or SOCKET), None, connected)

    class CirroveExtension(
        GObject.GObject,
        Nautilus.InfoProvider,
        Nautilus.ColumnProvider,
        Nautilus.MenuProvider,
    ):
        def __init__(self):
            super().__init__()
            # One line at load, so "is it even loaded" has an answer in the
            # journal; nothing per file.
            print(f"cirrove: Files extension loaded, daemon at {SOCKET}", file=sys.stderr)
            self._mounts = Mounts()
            self._pending = []
            self._flush_scheduled = False
            self._cancelled = set()

        # -- badges ---------------------------------------------------------

        def update_file_info_full(self, provider, handle, closure, file):
            location = file.get_location()
            path = location.get_path() if location else None
            if not path:
                return Nautilus.OperationResult.COMPLETE
            if DEBUG:
                print(f"cirrove: asked about {path!r}", file=sys.stderr)
            # The common case -- a file nowhere near a mount, mounts known --
            # is answered here with no I/O at all.
            if self._mounts.fresh() and locate(self._mounts.cached(), path) is None:
                return Nautilus.OperationResult.COMPLETE
            # The provider Files handed over is what the completion must name;
            # the Python `self` may be a different wrapper of the same object,
            # and a completion under the wrong one leaves the listing's state
            # machine waiting on the first file forever.
            self._pending.append((path, (file, handle, closure, provider)))
            if not self._flush_scheduled:
                self._flush_scheduled = True
                # Files asks about a directory's entries one at a time and waits
                # for each answer before the next, so there is no burst to
                # collect; the next loop turn is soon enough, and a wait here
                # would be paid once per file.
                GLib.idle_add(self._flush)
            return Nautilus.OperationResult.IN_PROGRESS

        def cancel_update(self, provider, handle):
            self._cancelled.add(handle)

        def _flush(self):
            batch, self._pending = self._pending, []
            self._flush_scheduled = False
            if self._mounts.fresh():
                self._dispatch(batch)
            else:

                def adopted(status):
                    self._mounts.adopt(status)
                    self._dispatch(batch)

                request_async("status", None, adopted)
            return False

        def _dispatch(self, batch):
            per_label = {}
            for path, token in batch:
                located = locate(self._mounts.cached(), path)
                if located is None:
                    self._complete(token, {})
                    continue
                label, relative = located
                per_label.setdefault(label, []).append((relative, token))
            for label, entries in per_label.items():
                for chunk in chunked(entries, PATHS_PER_REQUEST):
                    self._ask(label, chunk)

        def _ask(self, label, chunk):
            def answered(reply):
                states = {s.get("path"): s for s in (reply or {}).get("states", [])}
                if DEBUG:
                    badged = sum(1 for s in states.values() if emblem_for(s))
                    print(
                        f"cirrove: {label}: asked about {len(chunk)}, "
                        f"{len(states)} answered, {badged} badged",
                        file=sys.stderr,
                    )
                for relative, token in chunk:
                    self._complete(token, states.get(relative, {}))

            request_async(
                "paths", {"label": label, "paths": [relative for relative, _ in chunk]}, answered
            )

        def _complete(self, token, state):
            file, handle, closure, provider = token
            if handle in self._cancelled:
                self._cancelled.discard(handle)
                return
            if DEBUG:
                print(f"cirrove: completing {file.get_name()!r}: {describe(state)!r}", file=sys.stderr)
            try:
                emblem = emblem_for(state)
                if emblem:
                    file.add_emblem(emblem)
                file.add_string_attribute("cirrove_status", describe(state))
            except Exception:  # noqa: BLE001
                traceback.print_exc(file=sys.stderr)
            Nautilus.info_provider_update_complete_invoke(
                closure, provider, handle, Nautilus.OperationResult.COMPLETE
            )

        # -- the column -----------------------------------------------------

        def get_columns(self):
            return [
                Nautilus.Column(
                    name="CirroveNautilus::status",
                    attribute="cirrove_status",
                    label="Cirrove",
                    description="Whether the file is kept offline",
                )
            ]

        # -- the menu -------------------------------------------------------

        def _located(self, files):
            located = []
            for file in files:
                location = file.get_location()
                path = location.get_path() if location else None
                if not path:
                    continue
                where = self._mounts.locate(path)
                if where is None:
                    continue
                label, relative = where
                located.append((file, label, relative, file.is_directory()))
            return located

        def _item(self, located):
            """One menu item for the selection, or none if it is not ours."""
            if not located:
                return None
            labels = {label for _, label, _, _ in located}
            if len(labels) != 1:
                # Two drives in one selection: which account would the pin
                # go to is not a question a menu item should guess at.
                return None
            label = labels.pop()
            states = states_for(label, [(relative, None) for _, _, relative, _ in located])
            ordered = [states.get(relative, {}) for _, _, relative, _ in located]
            folders = [entry for entry in located if entry[3]]
            text = menu_label(ordered, folders)
            item = Nautilus.MenuItem(
                name="CirroveNautilus::keep_offline",
                label=text,
                icon=ICON,
            )
            item.connect(
                "activate",
                self._on_activate,
                label,
                located,
                text.startswith("Stop"),
            )
            return item

        def get_file_items(self, *args):
            files = args[0] if len(args) == 1 else args[1]
            item = self._item(self._located(files))
            return [item] if item else []

        def get_background_items(self, *args):
            folder = args[0] if len(args) == 1 else args[1]
            item = self._item(self._located([folder]))
            return [item] if item else []

        def _on_activate(self, _menu, label, located, unpin):
            for file, _, relative, is_dir in located:
                body = {"label": label, "path": relative}
                if not unpin:
                    body["recursive"] = is_dir

                def changed(reply, file=file):
                    if DEBUG:
                        print(f"cirrove: {'unpin' if unpin else 'pin'} -> {reply}", file=sys.stderr)
                    # A fresh badge, whatever the daemon decided.
                    file.invalidate_extension_info()

                request_async("unpin" if unpin else "pin", body, changed)
