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
APP_ID = "io.github.Dandiccf.Cirrove"
# Where an icon theme puts a scalable application icon. Checked on disk rather
# than asked of GtkIconTheme: this runs at import, before Files has a display,
# and a theme lookup there is both unavailable and slower than a stat.
ICON_SEARCH = [
    os.path.join(
        os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share"), "icons"
    ),
    "/usr/local/share/icons",
    "/usr/share/icons",
]
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
# How many files the extension remembers so it can refresh them when what is
# kept offline changes behind Files. A window shows tens; this covers several,
# and the entries are small. Bounded because Files never says a file is gone.
REMEMBERED = 512
# How long to wait before opening the event subscription again. A daemon that
# is not running is the ordinary case at login, and a file manager must not
# spend a socket connect a second on it forever.
RECONNECT = [2.0, 5.0, 15.0, 60.0]


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


def emblem_names(search=None):
    """Which icon names the badges use.

    Cirrove's own where they are installed: they match the tray, they say
    "cloud" where a bare tick says nothing, and they carry a rim so they read
    over a thumbnail of any colour. The freedesktop ones where they are not,
    because Files draws nothing at all for an icon name no theme provides --
    the badge would silently vanish rather than look generic. That happens on a
    machine with only the daemon package, whose icons ship beside the desktop.
    """
    for base in search if search is not None else ICON_SEARCH:
        apps = os.path.join(str(base), "hicolor", "scalable", "apps")
        if os.path.isfile(os.path.join(apps, f"{APP_ID}-kept.svg")) and os.path.isfile(
            os.path.join(apps, f"{APP_ID}-fetching.svg")
        ):
            return {"kept": f"{APP_ID}-kept", "fetching": f"{APP_ID}-fetching"}
    return {"kept": "emblem-ok-symbolic", "fetching": "emblem-synchronizing-symbolic"}


EMBLEMS = emblem_names()


def emblem_for(state, names=None):
    """The badge a state earns, or None for the ordinary on-demand file.

    Most files in a cloud drive are on demand; badging them all would say
    nothing. A pin earns a mark, and the mark says whether the content is
    actually here: a pin the daemon has not fetched yet reserves space and
    keeps nothing, and a badge that called that "available" would be a lie the
    user finds out about on the train.
    """
    names = names if names is not None else EMBLEMS
    if state.get("refusal") or not state.get("pinned"):
        return None
    if state.get("kind") == "folder":
        return names["kept"]
    if state.get("resident", 0) >= state.get("size", 0):
        return names["kept"]
    return names["fetching"]


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


def human_bytes(n):
    """A short human size, so a property reads like the rest of the dialog."""
    n = float(n)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024 or unit == "TB":
            return f"{int(n)} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024


def properties(state, relative):
    """The Cirrove properties for one file, as ``(label, value)`` pairs, for the
    Alt+Enter dialog. Empty when the daemon could not answer (no section shown).

    The dialog already shows name, type, size and times; these are the things
    only Cirrove knows: whether the file is here or on demand, how much of it is
    on this computer, whether a pin keeps it, and where it is in the cloud.
    """
    if not state or state.get("refusal"):
        return []
    folder = state.get("kind") == "folder"
    pairs = [("Availability", describe(state) or "On demand")]
    if not folder:
        size = state.get("size", 0)
        resident = state.get("resident", 0)
        if resident >= size and size > 0:
            # A cached copy without a pin is here now and may be reclaimed;
            # saying only "all 66.2 KB" beside "Availability: On demand" reads
            # as a contradiction and promises something nothing guarantees.
            kept = " " if state.get("pinned") else ", until the space is needed"
            pairs.append(("On this computer", f"all {human_bytes(size)}{kept}".rstrip()))
        elif resident > 0:
            pairs.append(("On this computer", f"{human_bytes(resident)} of {human_bytes(size)}"))
        else:
            pairs.append(("On this computer", "not downloaded yet"))
    pairs.append((
        "Kept offline",
        {"direct": "yes", "inherited": "yes, through a folder above"}.get(state.get("pinned"), "no"),
    ))
    pairs.append(("Cloud location", "/" + relative if relative else "/"))
    if state.get("item"):
        pairs.append(("Provider item", state["item"]))
    return pairs


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


class Refreshes:
    """Decides, from the daemon's event stream, when Files must ask again.

    Files asks about a path when it lists a directory and then keeps the
    answer. So a pin made anywhere else -- the Cirrove window, the command
    line, another session -- left a stale badge sitting in an open Files window
    until something unrelated made it re-list. That is not a delay; it is a
    window that says the opposite of the truth for as long as it is left open.

    The daemon's account event carries ``kept_generation``, a number that moves
    when what the account keeps offline changes: a pin made or released, or one
    of its files arriving. A number rather than a path, because the event
    channel carries no paths -- which is the right decision for a channel a
    tray also reads -- so this says only "ask again about what you are showing".

    Priming is not a change. A subscription opens with current state, and a
    client that treated those as changes would refresh every open window every
    time it reconnected.
    """

    def __init__(self):
        self._kept = {}
        self._priming = True

    def observe(self, event):
        """The account labels whose entries are now stale. Possibly empty."""
        kind = (event or {}).get("event")
        if kind == "ready":
            self._priming = False
            return set()
        if kind == "lagged":
            # Intermediate states were dropped, so nothing held here is known to
            # be current. Everything is stale, and the re-priming that follows
            # is a record rather than a change.
            self._priming = True
            return {label for label, _ in self._kept.values()}
        if kind != "account":
            return set()
        label = event.get("label") or ""
        key = event.get("account_id") or label
        before = self._kept.get(key)
        now = event.get("kept_generation", 0)
        self._kept[key] = (label, now)
        if self._priming or before is None or before[1] == now:
            return set()
        return {label}


class Shown:
    """What has been badged, so it can be badged again when the answer changes.

    Bounded, because Files never says a file has gone: an unbounded registry in
    a process that runs for the length of a login session is a leak with a
    schedule. The oldest entry goes, which is the one least likely still to be
    on screen.
    """

    def __init__(self, limit=None):
        self._limit = REMEMBERED if limit is None else limit
        self._files = {}

    def remember(self, label, relative, file):
        key = (label, relative)
        if key not in self._files and len(self._files) >= self._limit:
            self._files.pop(next(iter(self._files)))
        self._files.pop(key, None)
        self._files[key] = file

    def under(self, label):
        """Everything remembered from one account, newest last."""
        return [file for (shown, _), file in self._files.items() if shown == label]

    def __len__(self):
        return len(self._files)


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

    def subscribe_async(on_event, on_end, path=None):
        """Open the daemon's event stream and call ``on_event(dict)`` per line.

        ``on_end()`` when the stream ends, for a caller that reconnects. Over
        GLib's asynchronous I/O for the same reason everything else here is: a
        Python thread inside Files runs only while the main thread is executing
        Python, which it does only inside a callback.
        """
        client = Gio.SocketClient.new()
        ended = [False]

        def end():
            if ended[0]:
                return
            ended[0] = True
            try:
                on_end()
            except Exception:  # noqa: BLE001 - a callback must not take Files down
                traceback.print_exc(file=sys.stderr)

        def read_line(stream, result, connection):
            try:
                line, _ = stream.read_line_finish_utf8(result)
            except GLib.Error:
                end()
                return
            if line is None:
                end()
                return
            try:
                on_event(json.loads(line))
            except ValueError:
                pass
            except Exception:  # noqa: BLE001
                traceback.print_exc(file=sys.stderr)
            stream.read_line_async(GLib.PRIORITY_DEFAULT, None, read_line, connection)

        def connected(client, result):
            try:
                connection = client.connect_finish(result)
                connection.get_output_stream().write_all(b"subscribe\n", None)
            except GLib.Error:
                end()
                return
            stream = Gio.DataInputStream.new(connection.get_input_stream())
            stream.read_line_async(GLib.PRIORITY_DEFAULT, None, read_line, connection)

        client.connect_async(Gio.UnixSocketAddress.new(path or SOCKET), None, connected)

    class CirroveExtension(
        GObject.GObject,
        Nautilus.InfoProvider,
        Nautilus.ColumnProvider,
        Nautilus.MenuProvider,
        Nautilus.PropertiesModelProvider,
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
            self._shown = Shown()
            self._refreshes = Refreshes()
            self._reconnects = 0
            self._watch()

        # -- staying current ------------------------------------------------

        def _watch(self):
            """Follow the daemon's event stream, reopening it when it ends."""

            def event(payload):
                self._reconnects = 0
                for label in self._refreshes.observe(payload):
                    self._refresh(label)

            def ended():
                # A daemon that restarted, or one that was never there. Back
                # off rather than spin: at login this runs before the service.
                delay = RECONNECT[min(self._reconnects, len(RECONNECT) - 1)]
                self._reconnects += 1
                GLib.timeout_add_seconds(int(delay), self._reopen)

            subscribe_async(event, ended)

        def _reopen(self):
            self._watch()
            return False

        def _refresh(self, label):
            """Ask Files to come back for everything shown from this account."""
            stale = self._shown.under(label)
            if DEBUG:
                print(
                    f"cirrove: {label}: what is kept changed, refreshing {len(stale)} entries",
                    file=sys.stderr,
                )
            for file in stale:
                try:
                    file.invalidate_extension_info()
                except Exception:  # noqa: BLE001
                    traceback.print_exc(file=sys.stderr)



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
                self._shown.remember(label, relative, token[0])
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

        # -- the Alt+Enter properties -------------------------------------

        def get_models(self, files):
            """A "Cirrove" section in the properties dialog for one of our files.

            Synchronous, like every properties provider: the dialog waits for
            it. One file at a time -- the dialog for a multi-file selection has
            no place for per-file cloud state -- and only for a file on a
            mount, asked with one `paths` request (3 s cap, like the rest).
            """
            try:
                if len(files) != 1:
                    return []
                located = self._located(files)
                if DEBUG:
                    print(f"cirrove: get_models for {len(files)} file(s), located={bool(located)}", file=sys.stderr)
                if not located:
                    return []
                _file, label, relative, _is_dir = located[0]
                states = states_for(label, [(relative, None)])
                pairs = properties(states.get(relative, {}), relative)
                if DEBUG:
                    print(f"cirrove: get_models pairs={pairs}", file=sys.stderr)
                if not pairs:
                    return []
                items = Gio.ListStore.new(Nautilus.PropertiesItem)
                for name, value in pairs:
                    items.append(Nautilus.PropertiesItem(name=name, value=value))
                return [Nautilus.PropertiesModel(title="Cirrove", model=items)]
            except Exception:  # noqa: BLE001 - a properties provider must not crash the dialog
                traceback.print_exc(file=sys.stderr)
                return []

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
