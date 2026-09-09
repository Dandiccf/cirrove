#!/usr/bin/env python3
"""Read a file through a live Cirrove mount and report the read-path counters.

ADR 0004 claims three behaviours -- shared session setup, bounded sequential
windows and safe renewal. The adapter-level validator can reach only the first:
it has no content cache, so staging never exists and the window path cannot run,
and it finishes long before the 60 s session lease could be renewed. Those two
counters were therefore never evidence of anything. This script reads through a
mount, which has both, and reports what the daemon counted.

The workload is deliberately shaped for those two counters: a sequential prefix
to produce windows, and a pause longer than the lease with the file still open,
so that the read after it must renew rather than open a second session.

The pause dominates the runtime. That is the point of it, not a defect.
"""

import argparse
import hashlib
import json
import subprocess
import time


def counters(binary, label):
    out = subprocess.run([binary, "status"], capture_output=True, text=True, check=True).stdout
    for account in json.loads(out)["accounts"]:
        if account["label"] == label:
            return account.get("read_path") or {}
    raise SystemExit(f"no account labelled {label!r} in status")


def read_range(handle, offset, length, digest, chunk=1 << 20):
    handle.seek(offset)
    remaining, first_byte = length, None
    started = time.monotonic()
    while remaining:
        block = handle.read(min(chunk, remaining))
        if not block:
            break
        if first_byte is None:
            first_byte = time.monotonic() - started
        digest.update(block)
        remaining -= len(block)
    return length - remaining, first_byte


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--path", required=True, help="file inside the mount")
    parser.add_argument("--label", required=True)
    parser.add_argument("--binary", default="cirrove")
    parser.add_argument("--arm", required=True, help="name recorded in the artifact")
    parser.add_argument("--prefix-mib", type=int, default=32)
    parser.add_argument("--range-mib", type=int, default=8)
    parser.add_argument(
        "--pause-seconds",
        type=int,
        default=70,
        help="must exceed SESSION_LEASE or renewal cannot be reached",
    )
    parser.add_argument(
        "--pace-seconds",
        type=int,
        default=0,
        help=(
            "instead of pausing, read continuously at a pace that spreads the work "
            "over this many seconds. An idle pause does not reach renewal: the read "
            "after it is served outside the session entirely, so the session is never "
            "asked for bytes with an expired lease. Only continuous use reaches it."
        ),
    )
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    with open(args.path, "rb") as handle:
        handle.seek(0, 2)
        size = handle.tell()
        prefix = args.prefix_mib << 20
        window = args.range_mib << 20
        if size < prefix + 3 * window:
            raise SystemExit(f"file is {size} bytes; too small for this workload")

        before = counters(args.binary, args.label)
        digest = hashlib.sha256()
        started = time.monotonic()

        prefix_read, first_byte = read_range(handle, 0, prefix, digest)
        mid = (size // 2) & ~0xFFFFF
        mid_read, _ = read_range(handle, mid, window, digest)

        paced_reads = 0
        if args.pace_seconds:
            # Keep the session working across the lease rather than letting it idle.
            deadline = time.monotonic() + args.pace_seconds
            offset = mid + window
            step = 1 << 20
            while time.monotonic() < deadline and offset + step < size:
                got, _ = read_range(handle, offset, step, digest)
                offset += step
                paced_reads += 1
                time.sleep(0.25)
        else:
            # The lease is what this waits out. The handle stays open across it, so
            # the next read continues an existing session instead of opening a new one.
            time.sleep(args.pause_seconds)

        late = (size - window) & ~0xFFFFF
        late_read, late_first_byte = read_range(handle, late, window, digest)
        elapsed = time.monotonic() - started
        after = counters(args.binary, args.label)

    delta = {k: after.get(k, 0) - before.get(k, 0) for k in set(before) | set(after)}
    artifact = {
        "arm": args.arm,
        "file_size": size,
        "bytes_read": prefix_read + mid_read + late_read,
        "sha256_of_read_bytes": digest.hexdigest(),
        "first_byte_seconds": first_byte,
        "first_byte_after_pause_seconds": late_first_byte,
        "wall_clock_seconds": round(elapsed, 3),
        "pause_seconds": args.pause_seconds,
        "pace_seconds": args.pace_seconds,
        "paced_reads": paced_reads,
        "counters_before": before,
        "counters_after": after,
        "counters_delta": delta,
    }
    with open(args.out, "w") as out:
        json.dump(artifact, out, indent=2)
        out.write("\n")
    print(json.dumps({k: v for k, v in artifact.items() if k != "counters_before"}, indent=2))


if __name__ == "__main__":
    main()
