#!/usr/bin/env python3
"""Run the namespace churn fixture in sustained mode, for G7.

G7 asks whether resident memory reaches a plateau over a long session. Until
today the fixture could not answer that at all: sustained rounds are not full
rounds, full rounds are the only ones that settle, so every sample taken during
sustained mode was pre-invalidation and not root-only, and there was no series a
plateau rule could bind to. `CIRROVE_CHURN_SUSTAINED_FULL_EVERY` fixes that; this
script drives it and records what it produces.

Run the pilot first. The plateau tolerance has to come from measured round-to-round
variation on an unchanged tree, not from intuition -- the only sustained datum that
existed before this was a sixty-five second debug run rising 9.7 percent per round,
and a five percent rule chosen blind would have failed both arms inside the first
hour.

    scripts/sustained-run.py pilot   --binary <frozen release test binary>
    scripts/sustained-run.py full    --binary <same binary> --tolerance <percent>

The full run wants a machine to itself for a day. Nothing else should compile or
measure while it does: the quantity it records is exactly the one a concurrent
build disturbs.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = "filesystem::capacity::real_combined_namespace_churn"

PROFILES = {
    # A pilot long enough to see several settled rounds and calibrate from them,
    # short enough to run before committing a day.
    "pilot": {"seconds": 7200, "files": 500_000, "full_every": 4},
    "full": {"seconds": 86_400, "files": 500_000, "full_every": 12},
}


def preflight(tmp: Path) -> dict:
    """Refuse to start a long run on a machine that will spoil it."""
    problems = []

    fstype = subprocess.run(
        ["findmnt", "--target", str(tmp), "-no", "FSTYPE"],
        capture_output=True,
        text=True,
    ).stdout.strip()
    if fstype == "tmpfs":
        problems.append(
            f"TMPDIR is on tmpfs ({tmp}). A RAM-backed temporary directory moves "
            "bytes out of process RSS without reducing host memory, which flatters "
            "every number this run produces."
        )

    if subprocess.run(["pgrep", "-x", "rustc"], capture_output=True).returncode == 0:
        problems.append("rustc is running; a concurrent build perturbs the measurement")

    free_gb = shutil.disk_usage(tmp).free / 2**30
    if free_gb < 50:
        problems.append(f"only {free_gb:.0f} GB free under {tmp}; a 500k run wants 50+")

    # Match the executable, not the command line. `pgrep -f` also matches any
    # shell whose arguments merely mention the binary -- including the one running
    # this check, which made the guard refuse a legitimate run the first time it
    # was used. The same shape as `pkill -f cirrove` matching the live daemon.
    others = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or entry.name == str(os.getpid()):
            continue
        try:
            executable = (entry / "exe").resolve().name
        except OSError:
            continue
        if executable.startswith("cirrove_service-"):
            others.append(entry.name)
    if others:
        problems.append(f"another fixture is already running (pids {' '.join(others)})")

    return {"tmpdir_fstype": fstype, "free_gb": round(free_gb, 1), "problems": problems}


def plateau(samples: list[dict]) -> dict:
    """Round-to-round change in settled, root-only resident memory."""
    settled = [
        s
        for s in samples
        if s.get("phase") == "released" and s.get("retained_views") == 1
    ]
    series = [s["memory"]["pss_kib"] for s in settled]
    steps = [
        100.0 * (b - a) / a for a, b in zip(series, series[1:]) if a
    ]
    return {
        "settled_rounds": len(series),
        "pss_kib": series,
        "step_percent": [round(s, 2) for s in steps],
        "worst_step_percent": round(max(steps), 2) if steps else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("profile", choices=sorted(PROFILES))
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--tolerance", type=float, default=None)
    parser.add_argument("--out", type=Path, default=ROOT / ".local-state/sustained")
    args = parser.parse_args()

    profile = PROFILES[args.profile]
    run = args.out / args.profile
    tmp = run / "tmp"
    tmp.mkdir(parents=True, exist_ok=True)

    checks = preflight(tmp)
    if checks["problems"]:
        for problem in checks["problems"]:
            print(f"refusing to start: {problem}", file=sys.stderr)
        return 1
    if args.profile == "full" and args.tolerance is None:
        print(
            "refusing to start: --tolerance is required for the full run, and it "
            "must come from a pilot rather than from intuition",
            file=sys.stderr,
        )
        return 1

    environment = dict(
        os.environ,
        TMPDIR=str(tmp),
        SQLITE_TMPDIR=str(tmp),
        CIRROVE_CHURN_FILES=str(profile["files"]),
        CIRROVE_CHURN_SECONDS=str(profile["seconds"]),
        CIRROVE_CHURN_SUSTAINED_FULL_EVERY=str(profile["full_every"]),
    )
    (run / "manifest.json").write_text(
        json.dumps(
            {
                "profile": args.profile,
                **profile,
                **checks,
                "binary": str(args.binary),
                "tolerance_percent": args.tolerance,
                "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            },
            indent=1,
        )
        + "\n"
    )

    log = run / "stdout.log"
    with log.open("w") as sink:
        result = subprocess.run(
            [
                str(args.binary),
                "--exact",
                FIXTURE,
                "--nocapture",
                "--ignored",
            ],
            env=environment,
            stdout=sink,
            stderr=subprocess.STDOUT,
        )

    samples = [
        json.loads(line.split("CIRROVE_COMBINED_CHURN", 1)[1])
        for line in log.read_text().splitlines()
        if "CIRROVE_COMBINED_CHURN" in line
    ]
    summary = {"exit": result.returncode, **plateau(samples)}
    if args.tolerance is not None and summary["worst_step_percent"] is not None:
        summary["within_tolerance"] = summary["worst_step_percent"] <= args.tolerance
    (run / "summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps(summary, indent=1))
    return 0 if result.returncode == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
