#!/usr/bin/env python3
"""Drive ordinary read activity through the live mount for a long window.

docs/benchmarks/trim-cadence.json predicts resident size over "two hours with
ordinary daemon activity". The run it was registered for did not record what
that activity was, and its driver did not survive the reboot that killed it, so
the series it left cannot be reproduced. This pins the workload down.

The shape is the one the comparable numbers came from -- docs/benchmarks/
daemon-allocator-arenas.json and trim-trigger-reaches-reads.json both read four
files of 60-120 MB end to end through the real mount, buffered, output
discarded, after an unmeasured warm-up pass that puts them in the content cache
so the window measures reading and not downloading.

What this adds is the gap. Those artifacts measured passes back to back; the
cadence needs quiet to fire at all, because the trim is gated on five idle ticks.
A pass every --cycle-seconds with the mount quiet in between is what "ordinary
activity" has to mean for a prediction about a once-a-minute cadence to be
testable: continuous reading would suppress every firing and measure nothing.

Paths come from a file rather than the source. They are one person's real
business documents and do not belong in a published artifact.
"""

import argparse
import sys
import time
from pathlib import Path


def read_through(path, chunk=1 << 20):
    """Read end to end, discard, and return bytes read."""
    total = 0
    with open(path, "rb") as handle:
        while True:
            block = handle.read(chunk)
            if not block:
                return total
            total += len(block)


# No page-cache eviction between passes, and none is needed: the daemon opens
# every file with FOPEN_DIRECT_IO, so the kernel caches no file data for this
# mount and each pass reaches the daemon whatever the host has in memory. This
# was checked rather than assumed -- a warm-up plus one pass took the daemon
# from 39 to 176 MiB resident with retention at 122.8 MiB, which reads that
# never arrived could not do. A fast pass is the daemon serving its own content
# cache off local btrfs, which is exactly the workload the comparable artifacts
# measured, and not evidence that the kernel answered instead.


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--files", required=True, type=Path)
    parser.add_argument("--seconds", type=int, default=7200)
    parser.add_argument("--cycle-seconds", type=int, default=300)
    parser.add_argument("--log", required=True, type=Path)
    args = parser.parse_args()

    paths = [line for line in args.files.read_text().splitlines() if line.strip()]
    missing = [p for p in paths if not Path(p).is_file()]
    if missing:
        raise SystemExit(f"not readable through the mount: {missing}")

    args.log.parent.mkdir(parents=True, exist_ok=True)
    with args.log.open("w", buffering=1) as log:
        started = time.monotonic()
        log.write(f"# {len(paths)} files, cycle {args.cycle_seconds}s, window {args.seconds}s\n")

        # Warm-up, unmeasured: puts the four in the content cache so the window
        # measures reads out of cache rather than downloads.
        warm_started = time.time()
        warm_bytes = sum(read_through(p) for p in paths)
        log.write(f"warmup\t{int(warm_started)}\t{warm_bytes}\t{time.monotonic()-started:.1f}\n")

        passes = 0
        while time.monotonic() - started < args.seconds:
            cycle_started = time.monotonic()
            wall = time.time()
            read = sum(read_through(p) for p in paths)
            elapsed = time.monotonic() - cycle_started
            passes += 1
            log.write(f"pass\t{int(wall)}\t{read}\t{elapsed:.1f}\n")
            # The remainder of the cycle is quiet on purpose: it is the only
            # window in which the cadence can fire.
            quiet = args.cycle_seconds - elapsed
            if quiet > 0:
                time.sleep(min(quiet, args.seconds - (time.monotonic() - started)))
        log.write(f"# {passes} passes\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
