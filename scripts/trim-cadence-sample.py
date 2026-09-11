#!/usr/bin/env python3
"""Sample the live daemon's resident size and trim counter over a long window.

docs/benchmarks/trim-cadence.json registers a two-hour window against the
cadence trigger. The run it was written for died 17 minutes in, to the reboot
that closed the installable-service box, and its sampler was ad-hoc and did not
survive. This is that sampler, written down so the next interruption costs the
window and not the method.

Columns match .local-state/trim-cadence-partial.tsv so the partial series
concatenates: unix, rss_kib, trims, retained_kib, then free_arena_kib appended.

RSS is read from /proc/<pid>/status for the unit's MainPID rather than from
systemd's MemoryCurrent, which is a cgroup figure and includes page cache the
daemon did not allocate. The predictions are about the process.
"""

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path


def main_pid(unit):
    out = subprocess.run(
        ["systemctl", "--user", "show", unit, "--property=MainPID", "--value"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    pid = int(out or 0)
    if pid <= 0:
        raise SystemExit(f"{unit} is not running")
    return pid


def rss_kib(pid):
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1])
    raise SystemExit(f"no VmRSS for pid {pid}")


def counters(binary):
    out = subprocess.run([binary, "status"], capture_output=True, text=True, check=True).stdout
    status = json.loads(out)
    return (
        status["allocator_trims"],
        status["retained_bytes"] // 1024,
        status["free_arena_bytes"] // 1024,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--unit", default="cirroved.service")
    parser.add_argument("--binary", default="cirrove")
    parser.add_argument("--seconds", type=int, default=7200)
    parser.add_argument("--interval", type=int, default=60)
    args = parser.parse_args()

    pid = main_pid(args.unit)
    deadline = time.monotonic() + args.seconds
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", buffering=1) as sink:
        sink.write("unix\trss_kib\ttrims\tretained_kib\tfree_arena_kib\n")
        while time.monotonic() < deadline:
            # A restart resets the trim counter and the resident series both, so
            # the run is void rather than dented. Say so in the file instead of
            # silently splicing two processes into one series.
            if main_pid(args.unit) != pid:
                sink.write(f"# VOID: MainPID changed at {int(time.time())}\n")
                print("daemon restarted; series void", file=sys.stderr)
                return 1
            trims, retained, free_arena = counters(args.binary)
            sink.write(f"{int(time.time())}\t{rss_kib(pid)}\t{trims}\t{retained}\t{free_arena}\n")
            time.sleep(args.interval)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
