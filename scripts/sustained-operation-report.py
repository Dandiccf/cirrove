#!/usr/bin/env python3
"""Read a sustained-operation sample file and answer its five predictions.

The window is twenty-four hours of one row a minute, and the question it
answers is not "what happened" but "did each of the five things we said before
it started turn out to be true". Doing that by eye is how a window ends up
reported as clean when it had a gap in it, so this does it from the file and
says which clause each answer belongs to.

Usage:
    scripts/sustained-operation-report.py SAMPLES.tsv [--injections LOG]
    scripts/vm/run.sh ubuntu get '~/window/*' docs/benchmarks/   # to fetch them
"""

import argparse
import pathlib
import sys
import time

# P2's ceiling, from the registered prediction: the end within 20 percent of
# the value one hour in, once the index is warm.
DRIFT_ALLOWED = 0.20
WARM_AFTER_S = 3600
# P4's ceiling.
LISTING_CEILING_MS = 1000


def read(path: pathlib.Path):
    rows, markers = [], []
    for line in path.read_text().splitlines():
        if line.startswith("#"):
            markers.append(line)
            continue
        if line.startswith("unix\t"):
            continue
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) != 10:
            markers.append(f"# MALFORMED ROW: {line!r}")
            continue
        rows.append(
            {
                "unix": int(parts[0]),
                "uptime": int(parts[1]),
                "pid": parts[2],
                "rss": int(parts[3]),
                "state": parts[4],
                "mounted": parts[5] == "1",
                "feeds": parts[6],
                "items": int(parts[7]),
                "stuck": int(parts[8]),
                "listing": int(parts[9]),
            }
        )
    return rows, markers


def clock(unix):
    return time.strftime("%a %H:%M", time.localtime(unix))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("samples", type=pathlib.Path)
    parser.add_argument("--injections", type=pathlib.Path)
    args = parser.parse_args()

    rows, markers = read(args.samples)
    if not rows:
        print("no samples", file=sys.stderr)
        return 2

    voided = [m for m in markers if "void" in m.lower() or "PROBE-LOST" in m]
    restarts = [m for m in markers if m.startswith("# RESTART")]
    span = (rows[-1]["unix"] - rows[0]["unix"]) / 3600

    print(f"{len(rows)} samples, {clock(rows[0]['unix'])} to {clock(rows[-1]['unix'])} ({span:.1f} h)")
    for m in markers:
        print(f"  {m}")
    print()

    verdicts = []

    # P1: one process throughout, apart from a deliberate restart; mount present.
    pids = [r["pid"] for r in rows]
    distinct = sorted(set(pids))
    unmounted = [r for r in rows if not r["mounted"]]
    p1 = len(distinct) <= 1 + len(restarts) and not unmounted
    verdicts.append(
        (
            "P1",
            p1,
            f"{len(distinct)} process id(s) {distinct} with {len(restarts)} recorded restart(s); "
            f"{len(unmounted)} sample(s) with the mount absent",
        )
    )

    # P2: the end within 20 percent of the value an hour in.
    warm = [r for r in rows if r["uptime"] >= WARM_AFTER_S]
    if warm:
        base, end = warm[0]["rss"], rows[-1]["rss"]
        drift = (end - base) / base if base else 0.0
        p2 = abs(drift) <= DRIFT_ALLOWED
        detail = (
            f"{base / 1024:.0f} MiB at one hour, {end / 1024:.0f} MiB at the end, "
            f"{drift * 100:+.1f} percent (allowed +/-{DRIFT_ALLOWED * 100:.0f}); "
            f"peak {max(r['rss'] for r in rows) / 1024:.0f} MiB"
        )
    else:
        p2, detail = None, "the window did not reach one hour, so there is no warm baseline"
    verdicts.append(("P2", p2, detail))

    # P3: the index never emptied.
    counted = [r["items"] for r in rows if r["items"] >= 0]
    p3 = bool(counted) and min(counted) > 0
    verdicts.append(
        ("P3", p3, f"indexed items between {min(counted)} and {max(counted)}" if counted else "never read")
    )

    # P4: a listing stayed under a second, including during the outage.
    worst = max(rows, key=lambda r: r["listing"])
    p4 = worst["listing"] < LISTING_CEILING_MS and worst["listing"] >= 0
    verdicts.append(
        (
            "P4",
            p4,
            f"slowest listing {worst['listing']} ms at {clock(worst['unix'])} "
            f"(ceiling {LISTING_CEILING_MS}); mean "
            f"{sum(r['listing'] for r in rows) / len(rows):.1f} ms",
        )
    )

    # P5 is about what happened around the injections, which the injection log
    # records in words; this reports what the samples can corroborate.
    after_restart = None
    if restarts:
        at = int(restarts[-1].split("at=")[-1].split()[0])
        later = [r for r in rows if r["unix"] > at]
        recovered = next((r for r in later if r["state"] == "ready" and r["mounted"]), None)
        after_restart = (
            f"ready and mounted again {(recovered['unix'] - at) // 60} min after the restart"
            if recovered
            else "never returned to ready and mounted in the samples after the restart"
        )
    verdicts.append(
        (
            "P5",
            None if not restarts else bool(after_restart and "never" not in after_restart),
            after_restart or "no restart was recorded; see the injection log",
        )
    )

    # Context, not a verdict. An account reads updating_or_offline whenever any
    # feed is not ready, which includes the periodic recovery refresh
    # re-indexing a collection -- ordinary operation, and easy to misread as an
    # outage next to a column of "ready". What separates the two is whether the
    # mount stayed and the index held, which P1 and P3 answer.
    from collections import Counter

    states = Counter(r["state"] for r in rows)
    if len(states) > 1:
        print("states seen: " + ", ".join(f"{n}x {s}" for s, n in states.most_common()))
        for state, _ in states.most_common()[1:]:
            sample = [r for r in rows if r["state"] == state]
            print(
                f"  while {state}: mount present in {sum(1 for r in sample if r['mounted'])}"
                f"/{len(sample)}, indexed items"
                f" {min(r['items'] for r in sample)}-{max(r['items'] for r in sample)},"
                f" slowest listing {max(r['listing'] for r in sample)} ms"
            )
        print()

    for name, ok, detail in verdicts:
        mark = "HOLDS " if ok else ("FAILS " if ok is False else "UNKNOWN")
        print(f"{name} {mark} {detail}")

    if args.injections and args.injections.exists():
        print("\ninjection log:")
        for line in args.injections.read_text().splitlines():
            print(f"  {line}")

    if voided:
        print("\nThis window carries a void or probe-lost marker; nothing is claimed from it.")
        return 1
    failed = [n for n, ok, _ in verdicts if ok is False]
    if failed:
        print(f"\n{', '.join(failed)} did not hold. Read the decision rule before deciding what that means.")
        return 1
    unknown = [n for n, ok, _ in verdicts if ok is None]
    if unknown:
        print(f"\n{', '.join(unknown)} could not be answered from the samples alone.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
