#!/usr/bin/env python3
"""Watch the daemon over a long window and record what would count as failing.

docs/benchmarks/sustained-operation.json registers the run. The milestone 1 row
it answers -- "real restart/outage checks and at least 24 hours of sustained
operation" -- is the only acceptance box in the project with no evidence of any
kind, and the reason is that it is made of elapsed time rather than of work.

What it records, every interval, one row per sample:

    unix          when
    uptime_s      seconds since this sampler started
    pid           the daemon's MainPID -- a change means it was restarted
    rss_kib       VmRSS of that process, not the cgroup's MemoryCurrent, which
                  includes page cache the daemon never allocated
    state         the account's state
    mounted       whether the mount is present
    feeds         one letter per feed: r ready, else the first letter of its state
    items         indexed_items -- a drop to zero is a reindex, not a survival
    stuck         stuck_changes
    listing_ms    time to list the mount root, so a daemon that is alive but no
                  longer serving shows up as latency rather than as nothing

A restart is recorded rather than hidden. The row asks for sustained operation,
and a daemon that was restarted halfway has not sustained anything -- but a
sampler that silently spans the restart would report a clean 24 hours. The `pid`
column is what makes that visible, and `# RESTART` marks it in the file.
"""

import argparse
import json
import subprocess
import time
from pathlib import Path

MOUNT = Path.home() / "Cloud" / "Cirrove-OneDrive"


def main_pid(unit: str) -> int:
    out = subprocess.run(
        ["systemctl", "--user", "show", unit, "--property=MainPID", "--value"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    return int(out or 0)


def rss_kib(pid: int) -> int:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        pass
    return -1


def account(binary: str) -> dict:
    try:
        out = subprocess.run(
            [binary, "status"], capture_output=True, text=True, timeout=30, check=True
        ).stdout
        first = (json.loads(out).get("accounts") or [{}])[0]
        feeds = "".join((f.get("state") or "?")[0] for f in first.get("feeds") or [])
        return {
            "state": first.get("state", "?"),
            "mounted": int(bool(first.get("mounted"))),
            "feeds": feeds or "-",
            "items": first.get("indexed_items", -1),
            "stuck": first.get("stuck_changes", -1),
        }
    except Exception:
        # A daemon that cannot answer is the finding. Recorded, never retried
        # into looking healthy.
        return {"state": "unreachable", "mounted": 0, "feeds": "-", "items": -1, "stuck": -1}


def listing_ms() -> int:
    start = time.monotonic()
    try:
        list(MOUNT.iterdir())
    except OSError:
        return -1
    return int((time.monotonic() - start) * 1000)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--unit", default="cirroved.service")
    parser.add_argument("--binary", default="cirrove")
    parser.add_argument("--hours", type=float, default=24.0)
    parser.add_argument("--interval", type=int, default=60)
    args = parser.parse_args()

    started = time.monotonic()
    deadline = started + args.hours * 3600
    first_pid = main_pid(args.unit)
    if first_pid <= 0:
        raise SystemExit(f"{args.unit} is not running; there is nothing to sustain")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", buffering=1) as sink:
        sink.write(f"# started pid={first_pid} hours={args.hours} interval={args.interval}\n")
        sink.write("unix\tuptime_s\tpid\trss_kib\tstate\tmounted\tfeeds\titems\tstuck\tlisting_ms\n")
        while time.monotonic() < deadline:
            pid = main_pid(args.unit)
            if pid != first_pid:
                sink.write(f"# RESTART was={first_pid} now={pid} at={int(time.time())}\n")
                first_pid = pid
            a = account(args.binary)
            sink.write(
                f"{int(time.time())}\t{int(time.monotonic() - started)}\t{pid}\t{rss_kib(pid)}\t"
                f"{a['state']}\t{a['mounted']}\t{a['feeds']}\t{a['items']}\t{a['stuck']}\t"
                f"{listing_ms()}\n"
            )
            time.sleep(args.interval)
    sink_path = args.out
    print(f"window closed; {sink_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
