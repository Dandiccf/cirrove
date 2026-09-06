#!/usr/bin/env python3
"""Observe a local Cirrove service; never publish the resulting private log."""
import argparse
from collections import Counter
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import time

STATES = {
    "ready", "starting", "disabled", "indexing", "updating_or_offline",
    "offline", "unavailable", "throttled", "sign_in_required", "rebuilding",
    "account_start_failed",
}


def snapshot(status):
    """Whitelist aggregate fields; account names/IDs and provider messages stay out."""
    accounts = status.get("accounts", [])
    feeds = [feed for account in accounts for feed in account.get("feeds", [])]
    states = Counter(a.get("state") if a.get("state") in STATES else "other" for a in accounts)
    feed_states = Counter(f.get("state") if f.get("state") in STATES else "other" for f in feeds)
    return {
        "reachable": True,
        "configured_accounts": len(accounts),
        "mounted_accounts": sum(a.get("mounted") is True for a in accounts),
        "account_states": dict(states),
        "feed_states": dict(feed_states),
        "ready": bool(accounts) and all(
            a.get("mounted") is True and a.get("state") == "ready" for a in accounts
        ),
    }


def sample(cli):
    try:
        result = subprocess.run([str(cli), "status"], capture_output=True, timeout=8)
        if result.returncode != 0 or len(result.stdout) > 1024 * 1024:
            return {"reachable": False, "ready": False}
        return snapshot(json.loads(result.stdout))
    except (OSError, subprocess.TimeoutExpired, ValueError, TypeError, AttributeError):
        return {"reachable": False, "ready": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=float, default=24 * 60 * 60)
    parser.add_argument("--interval", type=float, default=60)
    args = parser.parse_args()
    if (not math.isfinite(args.seconds) or not math.isfinite(args.interval)
            or args.seconds <= 0 or args.interval < 1 or not args.cli.is_file()):
        parser.error("use an existing CLI, a positive duration and interval >= 1 second")
    args.output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    started = time.monotonic()
    samples = ready = unreachable = 0
    print("Observing Cirrove service health locally; no cloud files are read or changed.", flush=True)
    with os.fdopen(fd, "w") as log:
        def record(value):
            log.write(json.dumps(value, sort_keys=True) + "\n")
            log.flush()
            os.fsync(log.fileno())

        record({"kind": "start", "at": datetime.now(timezone.utc).isoformat(),
                "requested_seconds": args.seconds, "interval_seconds": args.interval})
        while not stopping:
            current = sample(args.cli)
            samples += 1
            ready += int(current["ready"])
            unreachable += int(not current["reachable"])
            record({"kind": "sample", "at": datetime.now(timezone.utc).isoformat(), **current})
            if time.monotonic() - started >= args.seconds:
                break
            wake = min(started + args.seconds, time.monotonic() + args.interval)
            while not stopping and time.monotonic() < wake:
                time.sleep(max(0, min(0.5, wake - time.monotonic())))
        elapsed = time.monotonic() - started
        summary = {"kind": "summary", "completed": elapsed >= args.seconds and not stopping,
                   "elapsed_seconds": round(elapsed, 1), "samples": samples,
                   "ready_samples": ready, "unreachable_samples": unreachable,
                   "note": "Health observations only; controlled recovery tests are recorded separately."}
        record(summary)
        print(json.dumps(summary), flush=True)


if __name__ == "__main__":
    main()
