#!/usr/bin/python3
"""Cirrove's external Strata file provider. JSON lines; never opens selected files."""
import argparse
import json
import os
import secrets
import socket
import sys
import threading
import time

LIMIT = 200
FRAME = 1024 * 1024
CONTROL_LIMIT = 8192
TIMEOUT = 1.0


class UnsentRequest(ValueError):
    pass


def control_line(verb, body):
    return verb + (" " + json.dumps(body, ensure_ascii=True) if body is not None else "")


def exchange(address, verb, body=None, allow_refusal=False):
    submission_started = False
    try:
        line = control_line(verb, body)
        if len(line.encode("utf-8")) > CONTROL_LIMIT:
            raise ValueError("control request limit")
        wire = (line + "\n").encode()
        deadline = time.monotonic() + TIMEOUT
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(TIMEOUT)
            stream.connect(address)
            submission_started = True
            stream.sendall(wire)
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
        if not isinstance(reply, dict) or (reply.get("refusal") and not allow_refusal):
            raise ValueError("service refusal")
        return reply
    except (OSError, ValueError, TypeError) as error:
        if not submission_started:
            raise UnsentRequest("request not sent") from error
        raise


def path_batches(group):
    label = group[0][1]["label"]
    envelope = len(control_line("paths-cached", {"label": label, "paths": []}).encode("utf-8"))
    batch = []
    size = envelope
    for entry in group:
        item_size = len(json.dumps(entry[2], ensure_ascii=True))
        if envelope + item_size > CONTROL_LIMIT:
            raise ValueError("path exceeds control request limit")
        if size + item_size + (2 if batch else 0) > CONTROL_LIMIT:
            yield batch
            batch = []
            size = envelope
        size += item_size + (2 if batch else 0)
        batch.append(entry)
    if batch:
        yield batch


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


def overlaps(root, path):
    root = root.rstrip("/") or "/"
    path = path.rstrip("/") or "/"
    return root == path or path.startswith(root.rstrip("/") + "/") or root.startswith(path.rstrip("/") + "/")


def describe(state):
    inherited = state.get("pinned") == "inherited"
    if not state.get("pinned"):
        return "On demand"
    text = "Kept offline through a folder" if inherited else "Kept offline"
    if state.get("kind") != "folder" and state.get("resident", 0) < state.get("size", 0):
        text += " · fetching {}%".format(100 * state.get("resident", 0) // max(state.get("size", 0), 1))
    return text


def display_text(text):
    return "".join(f"\\u{ord(c):04x}" if c not in "\r\n\t" and
                   (ord(c) < 32 or 127 <= ord(c) <= 159) else c for c in text)


def badge(state):
    if state.get("refusal") or state.get("pinned") not in ("direct", "inherited"):
        return None
    return "kept" if state.get("kind") == "folder" or state.get("resident", 0) >= state.get("size", 0) else "fetching"


def operation_body(action, account, relative, state):
    return {"label": account["label"], "path": relative,
            "recursive": action == "pin" and state["kind"] == "folder", "expected": state["identity"]}


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
    return [action for action in actions if action["id"] == "availability" or all(
        len(control_line(action["id"], operation_body(action["id"], account, relative, state)).encode("utf-8")) <= CONTROL_LIMIT
        for _, account, relative, state in entries)]


class Provider:
    def __init__(self, address, emit=None):
        self.address = address
        self.emit = emit or (lambda _: None)
        self.lock = threading.Lock()
        self.generation = 0
        self.global_generation = 0
        self.scopes = {}
        self.cached = None
        self.cached_at = 0
        self.contexts = {}
        self.event_levels = {}
        self.event_mounts = {}
        self.subscription_ready = False
        self.stop = threading.Event()

    def invalidate(self, paths=None):
        if paths is not None:
            paths = list(dict.fromkeys(paths))
            if (not paths or len(paths) > LIMIT or any(not valid_path(p) for p in paths)
                    or any(len(p.encode("utf-8")) > 16384 for p in paths)
                    or sum(len(p.encode("utf-8")) for p in paths) > 65536):
                paths = None
        with self.lock:
            self.generation += 1
            self.cached = None
            if paths is not None:
                self.scopes.update((path, self.generation) for path in paths)
                if len(self.scopes) > LIMIT or sum(len(p.encode("utf-8")) for p in self.scopes) > 65536:
                    paths = None
            if paths is None:
                self.global_generation = self.generation
                self.scopes.clear()
        event = {"version": 1, "event": "invalidate"}
        if paths is not None:
            event["paths"] = paths
        self.emit(event)

    def observe_event(self, event):
        kind = event.get("event")
        key = event.get("account_id")
        if kind == "ready":
            self.subscription_ready = True
            self.invalidate()
        elif kind == "account":
            level = (event.get("mounted"), event.get("kept_generation"), event.get("state"))
            if self.subscription_ready and self.event_levels.get(key) != level:
                root = self.event_mounts.get(key)
                self.invalidate([root] if root else None)
            self.event_levels[key] = level
        elif kind == "mount":
            previous = self.event_mounts.get(key)
            root = event.get("mount_path")
            if valid_path(root):
                self.event_mounts[key] = root
            else:
                self.event_mounts.pop(key, None)
            if self.subscription_ready:
                self.invalidate([p for p in (previous, root) if p] if valid_path(root) else None)
        elif kind == "account_removed":
            root = self.event_mounts.pop(key, None)
            self.event_levels.pop(key, None)
            if self.subscription_ready:
                self.invalidate([root] if root else None)
        elif kind == "lagged":
            self.subscription_ready = False
            self.event_levels.clear()
            self.event_mounts.clear()
            self.invalidate()
        if len(self.event_levels) > 256 or len(self.event_mounts) > 256:
            self.event_levels.clear()
            self.event_mounts.clear()
            self.invalidate()

    def mounts(self, fresh=False):
        with self.lock:
            generation = self.generation
            if not fresh and self.cached is not None and time.monotonic() - self.cached_at < 2:
                return self.cached, generation
        caps = exchange(self.address, "capabilities").get("capabilities", {})
        # Never fall back to `paths`: it may fetch/export uncached content.
        if caps.get("paths-cached") != 1 or caps.get("pin-identity") != 1:
            return [], generation
        status = exchange(self.address, "status")
        if status.get("protocol_version") != 1:
            return [], generation
        accounts = status.get("accounts", [])
        with self.lock:
            if generation == self.generation:
                self.cached = accounts
                self.cached_at = time.monotonic()
        return accounts, generation

    def states(self, paths, fresh=False):
        deadline = time.monotonic() + 4
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
            for batch in path_batches(group):
                if time.monotonic() >= deadline:
                    raise ValueError("selection query time limit")
                reply = exchange(self.address, "paths-cached", {"label": group[0][1]["label"], "paths": [e[2] for e in batch]})
                rows = {s.get("path"): s for s in reply.get("states", []) if isinstance(s, dict)}
                for path, account, relative in batch:
                    state = rows.get(relative)
                    identity = state.get("identity") if state else None
                    if (state and not state.get("refusal") and state.get("kind") in ("file", "folder") and state.get("item")
                            and isinstance(identity, dict) and identity.get("item") == state["item"]
                            and identity.get("mount") and isinstance(identity.get("scope"), dict)
                            and identity["scope"].get("account") == account["account_id"]):
                        entries.append((path, account, relative, state))
        with self.lock:
            current = (generation >= self.global_generation and not any(
                changed > generation and any(overlaps(root, path) for path in paths)
                for root, changed in self.scopes.items()))
            return entries if current else []

    def selection_identity(self, entries, request):
        return {"paths": request["paths"], "background": request["background"],
                "identities": [state["identity"] for _, _, _, state in entries]}

    def contextual_actions(self, entries, request):
        actions = menu(entries, len(request["paths"]))
        if not actions:
            return []
        identity = self.selection_identity(entries, request)
        with self.lock:
            token = next((token for token, saved in self.contexts.items() if saved == identity), None)
            if token is None:
                token = secrets.token_urlsafe(24)
                self.contexts[token] = identity
                if len(self.contexts) > 32:
                    self.contexts.pop(next(iter(self.contexts)))
        return [dict(action, context=token) for action in actions]

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
        accepted = 0
        attempted = False
        operation_deadline = time.monotonic() + 6
        try:
            entries = self.states(paths, fresh=method in ("menu", "activate"))
            if method == "query":
                reply["decorations"] = [{"path": p, "badge": badge(s), "description": describe(s)} for p, _, _, s in entries]
            elif method == "menu":
                reply["actions"] = self.contextual_actions(entries, request)
            elif method == "activate":
                action = request.get("action")
                with self.lock:
                    saved = self.contexts.get(request.get("context")) if isinstance(request.get("context"), str) else None
                if (saved is None or saved != self.selection_identity(entries, request)
                        or action not in {a["id"] for a in menu(entries, len(paths))}):
                    reply["message"] = "This selection is no longer eligible. Reopen the menu after Cirrove refreshes."
                    reply["outcome"] = {"status": "rejected", "accepted": 0, "total": len(paths)}
                elif action == "availability":
                    reply["message"] = "\n".join("{}: {}".format(os.path.basename(p) or p, describe(s)) for p, _, _, s in entries)[:16000]
                else:
                    # At most 200 explicit operations. No retry of an uncertain pin.
                    # A batch is not a transaction; report partial acceptance.
                    deadline = operation_deadline
                    reason = ""
                    job = None
                    for _, a, relative, state in entries:
                        if time.monotonic() >= deadline:
                            reason = "The time budget ended; remaining requests were not sent."
                            break
                        attempted = True
                        try:
                            response = exchange(self.address, action, operation_body(action, a, relative, state), allow_refusal=True)
                        except UnsentRequest:
                            attempted = False
                            raise
                        if response.get("accepted") is False:
                            reason = str(response.get("refusal") or "Cirrove declined the request.")
                            break
                        if response.get("accepted") is not True:
                            raise ValueError("missing acceptance")
                        accepted += 1
                        if len(entries) == 1 and isinstance(response.get("job"), str):
                            job = response["job"]
                    status = "accepted" if accepted == len(entries) else "partial" if accepted else "rejected"
                    reply["outcome"] = {"status": status, "accepted": accepted, "total": len(paths)}
                    if job and len(job.encode("utf-8")) <= 128 and not any(ord(c) < 32 or ord(c) == 127 for c in job):
                        reply["outcome"]["job"] = job
                    reply["message"] = "Cirrove accepted {} of {} requests. {}".format(accepted, len(entries), reason or ("Downloads continue in Cirrove." if action == "pin" else "Inherited folder pins still apply."))
                    self.invalidate([a["mount_path"] for _, a, _, _ in entries])
        except (OSError, ValueError, TypeError, KeyError):
            with self.lock:
                self.cached = None
            if method == "activate":
                reply["outcome"] = {"status": "unknown" if attempted else "partial" if accepted else "rejected", "accepted": accepted, "total": len(paths)}
                if attempted:
                    reply["message"] = "Cirrove confirmed {} of {} requests before losing the reply. Other items may already have been accepted; check availability before retrying.".format(accepted, len(paths))
                else:
                    reply["message"] = ("Cirrove accepted {} of {} requests. Remaining actions were not sent.".format(accepted, len(paths)) if accepted else "Cirrove is unavailable. This action was not sent.")
        if "message" in reply:
            reply["message"] = display_text(reply["message"]).encode("utf-8")[:16000].decode("utf-8", errors="ignore")
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
                    self.event_levels.clear()
                    self.event_mounts.clear()
                    self.subscription_ready = False
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
                            self.observe_event(event)
                            if event.get("event") == "ready":
                                delay = 1
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
