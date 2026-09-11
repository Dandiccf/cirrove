#!/usr/bin/env python3
"""Judge the registered predictions in docs/benchmarks/trim-cadence.json.

Written before the window closed, deliberately: a result that is scored by code
written after the numbers are visible can be scored into holding. The rules here
are transcriptions of the registration and nothing else.

    scripts/trim-cadence-result.py .local-state/trim-cadence/series.tsv
"""

import argparse
import json
import sys
from pathlib import Path

MIB = 1024.0
# Straight from the registration; changing either is changing the prediction.
P1_CEILING_MIB = 250.0
P2_TOLERANCE = 0.20
CADENCE_SECONDS = 60


def rows(path):
    series = []
    for line in path.read_text().splitlines():
        if line.startswith("#"):
            if "VOID" in line:
                raise SystemExit(f"series is void: {line}")
            continue
        if line.startswith("unix"):
            continue
        unix, rss_kib, trims, retained_kib, *rest = line.split("\t")
        series.append(
            {
                "unix": int(unix),
                "rss_mib": int(rss_kib) / MIB,
                "trims": int(trims),
                "retained_mib": int(retained_kib) / MIB,
            }
        )
    return series


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("series", type=Path)
    args = parser.parse_args()

    series = rows(args.series)
    if len(series) < 2:
        raise SystemExit("not enough samples to judge anything")

    window = series[-1]["unix"] - series[0]["unix"]
    rss = [s["rss_mib"] for s in series]
    peak, floor = max(rss), min(rss)

    # P1, both clauses. "Does not climb monotonically" is read as: the series
    # falls somewhere. A series that only ever rises is the failure this clause
    # names -- the previous design's 320 MiB was reached exactly that way.
    falls = sum(1 for a, b in zip(rss, rss[1:]) if b < a)
    p1_ceiling = peak < P1_CEILING_MIB
    p1_not_monotonic = falls > 0
    p1 = p1_ceiling and p1_not_monotonic

    # P2. The trim counter is process-wide and did not start at zero, so the
    # count is the delta across the window, not the last value.
    trims = series[-1]["trims"] - series[0]["trims"]
    expected = window / CADENCE_SECONDS
    low, high = expected * (1 - P2_TOLERANCE), expected * (1 + P2_TOLERANCE)
    p2 = low <= trims <= high

    # Spread, because AGENTS.md requires a difference to be quoted beside the
    # variation it has to beat.
    mean = sum(rss) / len(rss)
    spread = peak - floor

    result = {
        "samples": len(series),
        "window_seconds": window,
        "rss_mib": {
            "peak": round(peak, 1),
            "floor": round(floor, 1),
            "mean": round(mean, 1),
            "spread": round(spread, 1),
            "first": round(rss[0], 1),
            "last": round(rss[-1], 1),
            "falls": falls,
            "rises": sum(1 for a, b in zip(rss, rss[1:]) if b > a),
        },
        "trims": {
            "in_window": trims,
            "expected": round(expected, 1),
            "accepted_band": [round(low, 1), round(high, 1)],
            "per_minute": round(trims / (window / 60), 2) if window else None,
        },
        "P1": {
            "verdict": "HOLDS" if p1 else "FAILS",
            "under_250_mib": p1_ceiling,
            "not_monotonic": p1_not_monotonic,
        },
        "P2": {"verdict": "HOLDS" if p2 else "FAILS"},
    }
    print(json.dumps(result, indent=2))
    return 0 if p1 and p2 else 1


if __name__ == "__main__":
    sys.exit(main())
