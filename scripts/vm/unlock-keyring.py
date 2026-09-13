#!/usr/bin/env python3
"""Ask the session's Secret Service to unlock the login keyring, and hold its
prompt open until it is answered.

Run inside the VM's session. The prompt lives only as long as the D-Bus
connection that asked for it, so asking and waiting are one process; the
answer is typed into the prompt from outside, through QEMU's monitor
(scripts/vm/exercise.sh does that). `gnome-keyring-daemon --unlock` from ssh
would start a second daemon instead of talking to the session's, which is
why this exists.
"""
import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

LOGIN = "/org/freedesktop/secrets/collection/login"

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
service = Gio.DBusProxy.new_sync(
    bus, 0, None, "org.freedesktop.secrets", "/org/freedesktop/secrets",
    "org.freedesktop.Secret.Service", None,
)
unlocked, prompt = service.call_sync(
    "Unlock", GLib.Variant("(ao)", ([LOGIN],)), 0, -1, None
).unpack()
print("unlocked", unlocked, "prompt", prompt, flush=True)
if prompt != "/":
    proxy = Gio.DBusProxy.new_sync(
        bus, 0, None, "org.freedesktop.secrets", prompt, "org.freedesktop.Secret.Prompt", None
    )
    loop = GLib.MainLoop()

    def completed(_proxy, _sender, signal, params):
        if signal == "Completed":
            print("completed", params.unpack(), flush=True)
            loop.quit()

    proxy.connect("g-signal", completed)
    proxy.call_sync("Prompt", GLib.Variant("(s)", ("",)), 0, -1, None)
    GLib.timeout_add_seconds(60, loop.quit)
    loop.run()
