#!/usr/bin/env python3
"""Bounded developer-install hold and declared service restart preflight.

Read-only; no credentials, providers, SQLite writers or service mutations.
This does not attest arbitrary build artifacts, discover undeclared historical
windows, reserve a concurrent measurement lease, or authorize deployment.
"""
import argparse
import hashlib
import stat
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
    meta = strict_status(directory, "restart embargo")
    if (meta is None or not stat.S_ISDIR(meta.st_mode)
            or (meta.st_uid == os.getuid() and meta.st_mode & 0o500 != 0o500)):
        raise Refusal("restart embargo inspection is unsafe")
    try:
        declarations = sorted(path for path in directory.iterdir() if path.name.endswith(".json"))
    except OSError:
        raise Refusal("restart embargo inspection is unsafe") from None
    for path in declarations:
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


def strict_status(path, description="installed path"):
    # Path.exists/is_dir/glob may suppress PermissionError (notably Python3.14).
    # Only a genuinely missing path is empty; unknown inspection must refuse.
    try:
        return path.lstat()
    except FileNotFoundError:
        return None
    except OSError:
        raise Refusal(description + " inspection is unsafe") from None


def retained_state(state):
    safe_path(state)
    meta = strict_status(state, "installed state")
    if meta is None:
        return False
    if (not stat.S_ISDIR(meta.st_mode) or meta.st_uid != os.getuid()
            or meta.st_mode & 0o500 != 0o500):
        raise Refusal("installed state inspection is unsafe")
    names = ["accounts.json", "accounts", "metadata.db", "metadata.db-wal", "metadata.db-shm",
             "journal", "uploads.db", "objects", "working"]
    return any(strict_status(state / name, "installed state") is not None for name in names)


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


def file_identity(path, maximum, executable=False):
    """Bounded exact bytes/owner identity; this is not package provenance."""
    safe_path(path)
    before = path.stat()
    if (not stat.S_ISREG(before.st_mode) or before.st_size > maximum
            or before.st_uid not in {0, os.getuid()} or before.st_mode & 0o022
            or (executable and not before.st_mode & 0o111)):
        raise Refusal("package target file ownership or type is unsupported")
    digest = hashlib.sha256()
    observed = 0
    with path.open("rb") as source:
        while chunk := source.read(64 * 1024):
            observed += len(chunk)
            if observed > maximum:
                raise Refusal("package target exceeds its inspection bound")
            digest.update(chunk)
    after = path.stat()
    def stamp(value):
        return (value.st_dev, value.st_ino, value.st_mode, value.st_uid,
                value.st_gid, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
    if stamp(before) != stamp(after):
        raise Refusal("package target changed while being inspected")
    return {"stat": stamp(after), "sha256": digest.hexdigest()}


# All non-comment directives of the known packaged service, including commands
# that could otherwise run before/after ExecStart. Source comments may evolve;
# additional execution/environment/root routing is not silently accepted.
PACKAGE_DIRECTIVES = (
    "[Unit]", "Description=Cirrove cloud filesystem service",
    "Documentation=https://github.com/Dandiccf/cirrove", "StartLimitIntervalSec=60",
    "StartLimitBurst=5", "[Service]", "Type=exec",
    "ExecStart=/usr/bin/cirroved --state-dir %h/.local/state/cirrove --socket %t/cirrove/control.sock",
    "Environment=MALLOC_ARENA_MAX=1", "RuntimeDirectory=cirrove",
    "RuntimeDirectoryMode=0700", "UMask=0077", "Restart=on-failure",
    "RestartSec=5", "TimeoutStopSec=30", "[Install]", "WantedBy=default.target",
)


def package_routing(repo, home):
    runtime = Path("/run/user") / str(os.getuid())
    # These are the standard user-manager search roots. Empty control/generator
    # roots are normal; any relevant candidate there is conservatively refused.
    allowed = {
        home / ".config/systemd/user.control", runtime / "systemd/user.control",
        runtime / "systemd/transient", runtime / "systemd/generator.early",
        home / ".config/systemd/user", Path("/etc/xdg/systemd/user"),
        Path("/etc/systemd/user"), runtime / "systemd/user", Path("/run/systemd/user"),
        runtime / "systemd/generator", home / ".local/share/systemd/user",
        Path("/usr/local/share/systemd/user"), Path("/usr/share/systemd/user"),
        Path("/usr/local/lib/systemd/user"), Path("/usr/lib/systemd/user"),
        runtime / "systemd/generator.late",
    }
    roots = property_value(MANAGER_PATH, "org.freedesktop.systemd1.Manager", "UnitPath", "as")
    fragment = property_value(UNIT_PATH, "org.freedesktop.systemd1.Unit", "FragmentPath", "s")
    dropins = property_value(UNIT_PATH, "org.freedesktop.systemd1.Unit", "DropInPaths", "as")
    transient = property_value(UNIT_PATH, "org.freedesktop.systemd1.Unit", "Transient", "b")
    if (not isinstance(roots, list) or not 1 <= len(roots) <= 32
            or not all(text(root) for root in roots) or len(set(roots)) != len(roots)
            or not text(fragment) or dropins != [] or type(transient) is not bool or transient):
        raise Refusal("package service search path or overrides are unsupported")
    paths = [safe_path(Path(root)) for root in roots]
    if any(path not in allowed for path in paths) or Path("/usr/lib/systemd/user") not in paths:
        raise Refusal("package service search path or overrides are unsupported")
    target = Path("/usr/lib/systemd/user/cirroved.service")
    removable = home / ".config/systemd/user/cirroved.service"
    if fragment not in {"", str(target), str(removable)}:
        raise Refusal("package service current fragment is unsupported")
    if removable.parent in paths and paths.index(removable.parent) > paths.index(target.parent):
        raise Refusal("package service search priority is unsupported")
    inventory = {}
    for root in paths:
        root_meta = strict_status(root, "package service route")
        if root_meta is not None and not stat.S_ISDIR(root_meta.st_mode):
            raise Refusal("package service search root is unsupported")
        candidate = root / UNIT
        if strict_status(candidate, "package service route") is not None:
            if candidate not in {target, removable}:
                raise Refusal("package service has another winning unit candidate")
            inventory[str(candidate)] = file_identity(candidate, 64 * 1024)
        for name in (UNIT + ".d", "service.d"):
            directory = safe_path(root / name)
            meta = strict_status(directory, "package service route")
            if meta is not None:
                if not stat.S_ISDIR(meta.st_mode) or list(directory.iterdir()):
                    raise Refusal("package service has unit or type-wide drop-ins")
                inventory[str(directory)] = {"empty_directory": (meta.st_dev, meta.st_ino, meta.st_mode, meta.st_uid)}
    if str(target) not in inventory or (fragment and fragment not in inventory):
        raise Refusal("package service fragment is unavailable")
    template = safe_path(repo / "packaging/systemd/cirroved.service")
    template_pin = file_identity(template, 64 * 1024)
    expected = template.read_bytes().replace(b"%h/.local/bin/cirroved", b"/usr/bin/cirroved")
    directives = tuple(line.strip() for line in expected.decode().splitlines()
                       if line.strip() and not line.strip().startswith(("#", ";")))
    if directives != PACKAGE_DIRECTIVES or target.read_bytes() != expected:
        raise Refusal("package service template is not the supported exact target")
    # Pin every binary whose presence the switch relies on, including the tray
    # it would launch. Identity/bytes alone cannot attest a journal schema.
    binaries = {str(Path("/usr/bin") / name): file_identity(Path("/usr/bin") / name, 128 * 1024 * 1024, True)
                for name in ("cirroved", "cirrove", "cirrove-tray", "cirrove-desktop")}
    return {"roots": roots, "fragment": fragment, "dropins": dropins,
            "transient": transient, "inventory": inventory, "binaries": binaries,
            "template": template_pin, "future_state": str(safe_path(home / ".local/state/cirrove"))}


def package_preflight(repo):
    policy = source_policy(repo)
    embargo(repo)
    home = safe_path(service_home())
    if "HOME" not in os.environ or safe_path(Path(os.environ["HOME"])) != home:
        raise Refusal("installer home differs from the service owner home")
    before = service_snapshot()
    target = package_routing(repo, home)
    states = {Path(target["future_state"])}
    if before["effective_state"]:
        states.add(Path(before["effective_state"]))
    if before["process"]:
        states.add(Path(before["process"]["state"]))
    if any(retained_state(path) for path in sorted(states)):
        raise Refusal("installation refused: package schema provenance does not attest retained local state")
    if before != service_snapshot() or target != package_routing(repo, home):
        raise Refusal("package service ownership, routing or target changed during preflight")
    if source_policy(repo) != policy:
        raise Refusal("source installation policy changed during preflight")
    embargo(repo)
    # No target schema attestation exists, even when source policy is released.
    # Recheck markers after all target reads; concurrent launch afterward still
    # requires the documented exclusive deployment window, not this snapshot.
    if any(retained_state(path) for path in sorted(states)):
        raise Refusal("installation refused: package schema provenance does not attest retained local state")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--no-build", action="store_true")
    modes.add_argument("--package-switch", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.package_switch:
            package_preflight(args.repo)
        else:
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
