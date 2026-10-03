#!/usr/bin/env python3
"""Bounded developer-install hold and declared service restart preflight.

Read-only; no credentials, providers, SQLite writers or service mutations.
This does not attest arbitrary build artifacts, discover undeclared historical
windows, reserve a concurrent measurement lease, or authorize deployment.
"""
import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import pwd
import socket
import subprocess
import sys

UNIT = "cirroved.service"
DESTINATION = "org.freedesktop.systemd1"
MANAGER_PATH = "/org/freedesktop/systemd1"
UNIT_PATH = "/org/freedesktop/systemd1/unit/cirroved_2eservice"
POLICY_DOCUMENT = "docs/development.md#native-working-journal-schema19-held-prerelease-policy"


class Refusal(Exception):
    pass


def integer(value, maximum=2**64 - 1):
    return type(value) is int and 0 <= value <= maximum


def text(value, maximum=4096):
    return isinstance(value, str) and len(value.encode()) <= maximum and not any(ord(c) < 32 for c in value)


def document(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 4 * 1024 * 1024:
        raise Refusal("install preflight declaration is unavailable or unsafe")
    try:
        value = json.loads(path.read_text())
    except (OSError, ValueError):
        raise Refusal("install preflight declaration is invalid") from None
    if not isinstance(value, dict):
        raise Refusal("install preflight declaration is invalid")
    return value


def service_home():
    """Authoritative home of this user service owner; never caller XDG fallback."""
    return Path(pwd.getpwuid(os.getuid()).pw_dir)


def safe_path(path):
    if not path.is_absolute() or any(part in {".", ".."} for part in path.parts):
        raise Refusal("installed state route is not an absolute clean path")
    if any(parent.is_symlink() for parent in (path, *path.parents)):
        raise Refusal("installed state ancestry is unsafe")
    return path


def future_route(repo, home):
    """Recognize only the exact currently shipped future daemon state route."""
    path = repo / "packaging/systemd/cirroved.service"
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 64 * 1024:
        raise Refusal("future service template is unavailable or unsafe")
    section = None
    services = 0
    commands = []
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith(("#", ";")):
            continue
        if line.startswith("["):
            if not line.endswith("]"):
                raise Refusal("future service template is unsupported")
            section = line[1:-1]
            services += section == "Service"
            continue
        if section == "Service" and "=" in line:
            key, value = line.split("=", 1)
            if key.strip() == "ExecStart":
                commands.append(value.strip())
    expected = "%h/.local/bin/cirroved --state-dir %h/.local/state/cirrove --socket %t/cirrove/control.sock"
    if services != 1 or commands != [expected]:
        raise Refusal("future service template state route is unsupported")
    return safe_path(home / ".local/state/cirrove")


def busctl(arguments, signature):
    try:
        result = subprocess.run(["busctl", "--user", "--json=short", *arguments],
                                capture_output=True, timeout=5, check=False)
        if result.returncode != 0 or len(result.stdout) > 64 * 1024:
            raise Refusal("installed service routing could not be inspected")
        reply = json.loads(result.stdout)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        raise Refusal("installed service routing could not be inspected") from None
    if not isinstance(reply, dict) or reply.get("type") != signature or "data" not in reply:
        raise Refusal("installed service routing reply is invalid")
    return reply["data"]


def property_value(path, interface, name, signature):
    return busctl(["get-property", DESTINATION, path, interface, name], signature)


def route(executable, argv):
    """Only the existing daemon's explicit supported state/socket CLI is routable."""
    if (not text(executable) or not Path(executable).is_absolute()
            or Path(executable).name != "cirroved" or not isinstance(argv, list)
            or not 1 <= len(argv) <= 8 or not all(text(arg, 8192) for arg in argv)
            or argv[0] != executable):
        raise Refusal("installed service executable or route is unsupported")
    states = []
    sockets = []
    index = 1
    while index < len(argv):
        arg = argv[index]
        if arg in {"--state-dir", "--socket"}:
            if index + 1 >= len(argv):
                raise Refusal("installed service route is invalid")
            value = argv[index + 1]
            index += 2
        elif arg.startswith("--state-dir=") or arg.startswith("--socket="):
            arg, value = arg.split("=", 1)
            index += 1
        else:
            raise Refusal("installed service route is unsupported")
        path = safe_path(Path(value))
        (states if arg == "--state-dir" else sockets).append(path)
    if len(states) != 1 or len(sockets) > 1:
        raise Refusal("installed service state route is missing or ambiguous")
    return states[0]


def process_identity(pid):
    """Exact live PID identity/routing snapshot; returned internals never printed."""
    try:
        proc = Path("/proc") / str(pid)
        if proc.stat().st_uid != os.getuid():
            raise Refusal("installed service process owner changed")
        exe = os.readlink(proc / "exe")
        if exe.endswith(" (deleted)"):
            exe = exe[:-10]
        stat = (proc / "stat").read_text()
        tail = stat[stat.rindex(")") + 2:].split()
        started = int(tail[19])
        meta = (proc / "exe").stat()
        raw = (proc / "cmdline").read_bytes()
        if len(raw) > 64 * 1024 or not raw.endswith(b"\0"):
            raise Refusal("installed service process route is invalid")
        argv = [arg.decode() for arg in raw[:-1].split(b"\0")]
        state = route(exe, argv)
        return {"pid": pid, "started": started, "exe": exe,
                "exe_device": meta.st_dev, "exe_inode": meta.st_ino,
                "argv": argv, "state": str(state)}
    except (OSError, ValueError, IndexError, UnicodeError):
        raise Refusal("installed service process ownership could not be inspected") from None


def service_snapshot():
    result = busctl(["call", DESTINATION, MANAGER_PATH,
                     "org.freedesktop.systemd1.Manager", "ListUnitsByNames", "as", "1", UNIT],
                    "a(ssssssouso)")
    # Manager methods return one out-argument containing the array of rows.
    if (not isinstance(result, list) or len(result) != 1 or not isinstance(result[0], list)
            or len(result[0]) != 1 or not isinstance(result[0][0], list)):
        raise Refusal("installed service unit selection is ambiguous")
    row = result[0][0]
    if (len(row) != 10 or not all(text(row[i]) for i in [0, 1, 2, 3, 4, 5, 6, 8, 9])
            or not integer(row[7], 2**32 - 1) or row[0] != UNIT or row[5]
            or row[6] != UNIT_PATH):
        raise Refusal("installed service unit selection is invalid")
    if row[2] == "not-found" and row[3] == "inactive" and row[4] == "dead" and row[7] == 0:
        return {"unit": row, "load": "not-found", "pid": 0, "exec": [],
                "invocation": [], "process": None, "effective_state": None}
    load = property_value(UNIT_PATH, "org.freedesktop.systemd1.Unit", "LoadState", "s")
    pid = property_value(UNIT_PATH, "org.freedesktop.systemd1.Service", "MainPID", "u")
    commands = property_value(UNIT_PATH, "org.freedesktop.systemd1.Service", "ExecStart", "a(sasbttttuii)")
    invocation = property_value(UNIT_PATH, "org.freedesktop.systemd1.Unit", "InvocationID", "ay")
    if (load != "loaded" or load != row[2] or not integer(pid, 2**32 - 1)
            or not isinstance(commands, list) or len(commands) != 1
            or not isinstance(commands[0], list) or len(commands[0]) != 10
            or not isinstance(invocation, list) or len(invocation) != 16
            or not all(integer(item, 255) for item in invocation)):
        raise Refusal("installed service ownership or command is unsupported")
    command = commands[0]
    if (type(command[2]) is not bool or command[2]
            or not all(integer(command[i]) for i in [3, 4, 5, 6])
            or not integer(command[7], 2**32 - 1)
            or not all(type(command[i]) is int and -(2**31) <= command[i] < 2**31 for i in [8, 9])):
        raise Refusal("installed service command metadata is invalid")
    state = route(command[0], command[1])
    current = process_identity(pid) if pid else None
    return {"unit": row, "load": load, "pid": pid, "exec": commands,
            "invocation": invocation, "process": current, "effective_state": str(state)}


def embargo(repo):
    directory = repo / "docs/benchmarks"
    if not directory.is_dir() or directory.is_symlink():
        raise Refusal("restart embargo declarations are unavailable")
    for path in sorted(directory.glob("*.json")):
        record = document(path)
        if "restart_embargo" not in record:
            continue
        item = record["restart_embargo"]
        required = {"version", "hostname", "uid", "scope", "unit", "state"}
        optional = {"owner_pid", "owner_started_at"}
        if (not isinstance(item, dict) or not required <= item.keys()
                or item.keys() - required - optional or type(item["version"]) is not int
                or item["version"] != 1 or not text(item["hostname"]) or not item["hostname"]
                or not integer(item["uid"], 2**32 - 1) or item["scope"] != "user"
                or not text(item["unit"]) or not item["unit"].endswith(".service")
                or item["state"] not in {"active", "closed"}):
            raise Refusal("restart embargo declaration is invalid")
        if "owner_pid" in item and (not integer(item["owner_pid"], 2**32 - 1) or item["owner_pid"] == 0):
            raise Refusal("restart embargo owner declaration is invalid")
        if "owner_started_at" in item:
            try:
                stamp = datetime.fromisoformat(item["owner_started_at"].replace("Z", "+00:00"))
                if stamp.tzinfo is None:
                    raise ValueError()
            except (ValueError, AttributeError, TypeError):
                raise Refusal("restart embargo owner declaration is invalid") from None
        if (item["hostname"] == socket.gethostname() and item["uid"] == os.getuid()
                and item["unit"] == UNIT and item["state"] == "active"):
            raise Refusal("installation refused: a declared measurement forbids restarting this service")


def source_policy(repo):
    value = document(repo / "packaging/developer-install-policy.json")
    if (set(value) != {"version", "state", "journal_schema", "metadata_schema", "policy_document"}
            or type(value["version"]) is not int or value["version"] != 1
            or value["state"] not in {"held", "released"}
            or not integer(value["journal_schema"]) or value["journal_schema"] == 0
            or not integer(value["metadata_schema"]) or value["metadata_schema"] == 0
            or value["policy_document"] != POLICY_DOCUMENT
            or (value["state"] == "held" and (value["journal_schema"], value["metadata_schema"]) != (19, 8))):
        raise Refusal("source installation policy is invalid")
    return value


def retained_state(state):
    safe_path(state)
    if not state.exists():
        return False
    if not state.is_dir():
        raise Refusal("installed state location is invalid")
    # Presence suffices under the hold; no settings parsing, SQLite open or
    # credential/spool contents are needed to protect every retained provider.
    names = ["accounts.json", "accounts", "metadata.db", "metadata.db-wal", "metadata.db-shm",
             "journal", "uploads.db", "objects", "working"]
    return any((state / name).exists() or (state / name).is_symlink() for name in names)


def preflight(repo, no_build):
    policy = source_policy(repo)
    embargo(repo)
    home = safe_path(service_home())
    if "HOME" not in os.environ or safe_path(Path(os.environ["HOME"])) != home:
        raise Refusal("installer home differs from the service owner home")
    future = future_route(repo, home)
    if policy["state"] == "held" and no_build:
        raise Refusal("installation refused: held source cannot attest no-build artifact provenance")
    before = service_snapshot()
    states = {future}
    if before["effective_state"]:
        states.add(Path(before["effective_state"]))
    if before["process"]:
        states.add(Path(before["process"]["state"]))
    # Validate every route even for released policy; held policy additionally
    # refuses any existing account/metadata/spool under the complete route union.
    retained = [retained_state(path) for path in sorted(states)]
    after = service_snapshot()
    if before != after:
        raise Refusal("installed service ownership or route changed during preflight")
    if policy["state"] == "held" and any(retained):
        raise Refusal("installation refused: this source holds upgrades of retained local state")
    if source_policy(repo) != policy:
        raise Refusal("source installation policy changed during preflight")
    if future_route(repo, home) != future:
        raise Refusal("future service template route changed during preflight")
    embargo(repo)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args(argv)
    try:
        preflight(args.repo, args.no_build)
    except Refusal as error:
        print(str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, UnicodeError):
        print("installation preflight could not be completed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
