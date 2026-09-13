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

    # P2: the end within 20 percent of the value an hour in -- per continuous
    # run, because a deliberate restart is part of the plan and a fresh process
    # starts small. Comparing a post-restart process against the previous one's
    # warm baseline measures the restart, not a leak: it read -28.7 percent the
    # first time, which is the daemon working exactly as intended.
    runs, current = [], []
    for row in rows:
        if current and row["pid"] != current[-1]["pid"]:
            runs.append(current)
            current = []
        current.append(row)
    if current:
        runs.append(current)

    judged, notes = [], []
    for run in runs:
        start = run[0]["unix"]
        warm = [r for r in run if r["unix"] - start >= WARM_AFTER_S]
        if not warm:
            notes.append(
                f"pid {run[0]['pid']}: {(run[-1]['unix'] - start) / 60:.0f} min, too short to judge"
            )
            continue
        base, end = warm[0]["rss"], run[-1]["rss"]
        drift = (end - base) / base if base else 0.0
        judged.append(abs(drift) <= DRIFT_ALLOWED)
        notes.append(
            f"pid {run[0]['pid']}: {base / 1024:.0f} -> {end / 1024:.0f} MiB, "
            f"{drift * 100:+.1f} percent"
        )
    p2 = all(judged) if judged else None
    detail = "; ".join(notes) + (
        f"; peak {max(r['rss'] for r in rows) / 1024:.0f} MiB overall"
        if rows
        else ""
    )
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
    # Matched on the process the restart produced, not on a timestamp. The
    # sampler writes the marker and that minute's row together, so they carry
    # the same second and a strictly-later comparison missed the only row there
    # was -- reporting a recovery that had already happened as never happening.
    after_restart = None
    if restarts:
        marker = restarts[-1]
        at = int(marker.split("at=")[-1].split()[0])
        new_pid = marker.split("now=")[-1].split()[0]
        later = [r for r in rows if r["pid"] == new_pid]
        recovered = next((r for r in later if r["state"] == "ready" and r["mounted"]), None)
        if recovered:
            minutes = max(recovered["unix"] - at, 0) // 60
            after_restart = (
                f"pid {new_pid} was ready and mounted "
                + ("in the first sample after the restart" if minutes == 0 else f"{minutes} min after it")
                + f"; {len(later)} sample(s) since"
            )
        else:
            after_restart = f"pid {new_pid} never reached ready and mounted in {len(later)} sample(s)"
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
