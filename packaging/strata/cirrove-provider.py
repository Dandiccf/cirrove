#!/usr/bin/python3
"""Cirrove's external Strata file provider. JSON lines; never opens selected files."""
import argparse
import json
import os
import socket
import sys
import threading
import time

LIMIT = 200
FRAME = 1024 * 1024
TIMEOUT = 1.0


def exchange(address, verb, body=None):
    line = verb + (" " + json.dumps(body, ensure_ascii=True) if body is not None else "")
    deadline = time.monotonic() + TIMEOUT
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(TIMEOUT)
        stream.connect(address)
        stream.sendall((line + "\n").encode())
        data = bytearray()
        while True:
            stream.settimeout(max(0.001, deadline - time.monotonic()))
            part = stream.recv(65536)
            if not part:
                break
            data.extend(part)
            if len(data) > FRAME or time.monotonic() >= deadline:
                raise ValueError("response limit")
    reply = json.loads(data)
    if not isinstance(reply, dict) or reply.get("refusal"):
        raise ValueError("service refusal")
    return reply


def valid_path(path):
    return (isinstance(path, str) and path.startswith("/") and len(path) <= 16384
            and "\0" not in path and not any(0xD800 <= ord(c) <= 0xDFFF for c in path)
            and all(p not in (".", "..") for p in path.split("/")))


def locate(accounts, path):
    """Lexical mount boundaries; do not stat/resolve symlinks or hydrate paths."""
    matches = []
    for a in accounts:
        mount = a.get("mount_path", "")
        if not a.get("mounted") or not valid_path(mount) or not a.get("account_id") or not a.get("label"):
            continue
        mount = mount.rstrip("/") or "/"
        if path == mount:
            matches.append((len(mount), a, ""))
        elif path.startswith(mount.rstrip("/") + "/"):
            matches.append((len(mount), a, path[len(mount.rstrip("/")) + 1:]))
    if not matches:
        return None
    _, account, relative = max(matches, key=lambda m: m[0])
    return account, relative


def describe(state):
    inherited = state.get("pinned") == "inherited"
    if not state.get("pinned"):
        return "On demand"
    text = "Kept offline through a folder" if inherited else "Kept offline"
    if state.get("kind") != "folder" and state.get("resident", 0) < state.get("size", 0):
        text += " · fetching {}%".format(100 * state.get("resident", 0) // max(state.get("size", 0), 1))
    return text


def badge(state):
    if state.get("refusal") or state.get("pinned") not in ("direct", "inherited"):
        return None
    return "kept" if state.get("kind") == "folder" or state.get("resident", 0) >= state.get("size", 0) else "fetching"


def menu(entries, total):
    if len(entries) != total or not entries or len({e[1]["account_id"] for e in entries}) != 1:
        return []
    actions = []
    if all(e[3].get("pinned") == "direct" for e in entries):
        actions.append({"id": "unpin", "label": "Stop keeping offline", "icon": "mark"})
    elif all(e[3].get("can_pin") is True for e in entries):
        folders = all(e[3]["kind"] == "folder" for e in entries)
        label = ("Keep folder offline" if total == 1 else "Keep folders offline") if folders else "Keep offline"
        actions.append({"id": "pin", "label": label, "icon": "mark"})
    actions.append({"id": "availability", "label": "Show availability", "icon": "mark"})
    return actions


class Provider:
    def __init__(self, address, emit=None):
        self.address = address
        self.emit = emit or (lambda _: None)
        self.lock = threading.Lock()
        self.generation = 0
        self.cached = None
        self.cached_at = 0
        self.stop = threading.Event()

    def invalidate(self):
        with self.lock:
            self.generation += 1
            self.cached = None
        self.emit({"version": 1, "event": "invalidate"})

    def mounts(self, fresh=False):
        with self.lock:
            generation = self.generation
            if not fresh and self.cached is not None and time.monotonic() - self.cached_at < 2:
                return self.cached, generation
        caps = exchange(self.address, "capabilities").get("capabilities", {})
        # Never fall back to `paths`: it may fetch/export uncached content.
        if caps.get("paths-cached") != 1:
            return [], generation
        status = exchange(self.address, "status")
        if status.get("protocol_version") != 1:
            return [], generation
        accounts = status.get("accounts", [])
        with self.lock:
            if generation != self.generation:
                return [], self.generation
            self.cached = accounts
            self.cached_at = time.monotonic()
        return accounts, generation

    def states(self, paths, fresh=False):
        accounts, generation = self.mounts(fresh)
        groups = {}
        for path in paths:
            found = locate(accounts, path)
            if found:
                a, relative = found
                groups.setdefault(a["account_id"], []).append((path, a, relative))
        # Bound total socket work as well as the size of each batch.
        if len(groups) > 2:
            return []
        entries = []
        for group in groups.values():
            reply = exchange(self.address, "paths-cached", {"label": group[0][1]["label"], "paths": [e[2] for e in group]})
            rows = {s.get("path"): s for s in reply.get("states", []) if isinstance(s, dict)}
            for path, account, relative in group:
                state = rows.get(relative)
                if state and not state.get("refusal") and state.get("kind") in ("file", "folder") and state.get("item"):
                    entries.append((path, account, relative, state))
        with self.lock:
            return entries if generation == self.generation else []

    def handle(self, request):
        reply = {"version": 1, "id": request.get("id"), "decorations": [], "actions": []}
        paths = request.get("paths")
        method = request.get("method")
        if (request.get("version") != 1 or not isinstance(request.get("id"), int)
                or not isinstance(paths, list) or not 0 < len(paths) <= LIMIT
                or len(set(paths)) != len(paths) or not all(valid_path(p) for p in paths)
                or request.get("background") not in (True, False)
                or (request.get("background") and len(paths) != 1)):
            return reply
        try:
            entries = self.states(paths, fresh=method in ("menu", "activate"))
            if method == "query":
                reply["decorations"] = [{"path": p, "badge": badge(s), "description": describe(s)} for p, _, _, s in entries]
            elif method == "menu":
                reply["actions"] = menu(entries, len(paths))
            elif method == "activate":
                action = request.get("action")
                if action not in {a["id"] for a in menu(entries, len(paths))}:
                    reply["message"] = "This selection is no longer eligible. Reopen the menu after Cirrove refreshes."
                elif action == "availability":
                    reply["message"] = "\n".join("{}: {}".format(os.path.basename(p) or p, describe(s)) for p, _, _, s in entries)[:16000]
                else:
                    # At most 200 explicit operations. No retry of an uncertain pin.
                    # A batch is not a transaction; report partial acceptance.
                    accepted = 0
                    deadline = time.monotonic() + 4
                    for _, a, relative, state in entries:
                        if time.monotonic() >= deadline:
                            break
                        response = exchange(self.address, action, {"label": a["label"], "path": relative, "recursive": action == "pin" and state["kind"] == "folder"})
                        if not response.get("accepted"):
                            break
                        accepted += 1
                    reply["message"] = "Cirrove accepted {} of {} requests. {}".format(accepted, len(entries), "Downloads continue in Cirrove." if action == "pin" else "Inherited folder pins still apply.")
                    self.invalidate()
        except (OSError, ValueError, TypeError, KeyError):
            with self.lock:
                self.cached = None
            if method == "activate":
                reply["message"] = "Cirrove could not confirm the request. Some items may already have been accepted; check availability before retrying."
        return reply

    def watch(self):
        delay = 1
        while not self.stop.is_set():
            try:
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.settimeout(1)
                    stream.connect(self.address)
                    stream.sendall(b"subscribe\n")
                    data = bytearray()
                    levels = {}
                    ready = False
                    while not self.stop.is_set():
                        try:
                            part = stream.recv(65536)
                        except socket.timeout:
                            continue
                        if not part:
                            break
                        data.extend(part)
                        if len(data) > FRAME:
                            break
                        while b"\n" in data:
                            line, _, rest = data.partition(b"\n")
                            data = bytearray(rest)
                            event = json.loads(line)
                            kind = event.get("event")
                            if kind == "ready":
                                ready = True
                                delay = 1
                                self.invalidate()
                            elif kind == "account":
                                key = event.get("account_id")
                                level = (event.get("mounted"), event.get("kept_generation"), event.get("state"))
                                if ready and levels.get(key) != level:
                                    self.invalidate()
                                if len(levels) > 256:
                                    levels.clear()
                                levels[key] = level
                            elif ready and kind in ("mount", "account_removed", "lagged"):
                                self.invalidate()
            except (OSError, ValueError, TypeError):
                pass
            self.invalidate()
            self.stop.wait(delay)
            delay = min(delay * 2, 60)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--socket", default=os.path.join(os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}", "cirrove/control.sock"))
    args = parser.parse_args()
    output_lock = threading.Lock()

    def emit(value):
        with output_lock:
            sys.stdout.write(json.dumps(value, ensure_ascii=True) + "\n")
            sys.stdout.flush()

    provider = Provider(args.socket, emit)
    threading.Thread(target=provider.watch, daemon=True).start()
    try:
        while True:
            line = sys.stdin.buffer.readline(FRAME + 1)
            if not line:
                break
            if len(line) > FRAME or not line.endswith(b"\n"):
                break
            try:
                request = json.loads(line)
                if isinstance(request, dict):
                    emit(provider.handle(request))
            except (ValueError, TypeError, KeyError):
                break
    finally:
        provider.stop.set()


if __name__ == "__main__":
    main()
