#!/usr/bin/env python3
"""Capture the daemon's state across a suspend, and compare it afterwards.

A process on the machine being suspended cannot observe its own suspend: it is
frozen for the part that matters and wakes with no way to tell how long it was
gone or whether it was restarted. So the state is written to disk before and
compared after, which is the only form this evidence can take.

    scripts/suspend-check.py before     # then suspend
    scripts/suspend-check.py after      # once it is awake

Registered in docs/benchmarks/deep-suspend-beyond-token-lifetime.json. The
figure that answers "was it really asleep, and for how long" is CLOCK_BOOTTIME
minus CLOCK_MONOTONIC: boottime counts suspended time and monotonic does not.
/proc/uptime is boottime, so comparing it against wall clock reads zero across a
suspend and says nothing -- a discriminator that was tried and did not work.
"""

import json
import pathlib
import subprocess
import sys
import time

STATE = pathlib.Path.home() / ".cache" / "cirrove-suspend-check.json"


def run(*args: str) -> str:
    try:
        return subprocess.run(args, capture_output=True, text=True, timeout=30).stdout.strip()
    except Exception as error:  # a missing tool is a finding, not a crash
        return f"<unavailable: {error}>"


def slept_seconds() -> float:
    """Time spent suspended since boot."""
    return time.clock_gettime(time.CLOCK_BOOTTIME) - time.clock_gettime(time.CLOCK_MONOTONIC)


def snapshot() -> dict:
    status = run("cirrove", "status")
    account: dict = {}
    try:
        parsed = json.loads(status)
        first = (parsed.get("accounts") or [{}])[0]
        account = {
            key: first.get(key)
            for key in ("state", "mounted", "indexed_items", "stuck_changes", "label")
        }
        account["feeds"] = [feed.get("state") for feed in first.get("feeds") or []]
    except Exception as error:
        account = {"unreadable": str(error), "raw_prefix": status[:120]}
    mount = run("findmnt", "-n", "-o", "TARGET,FSTYPE", "-t", "fuse.cirrove")
    entries = run("bash", "-lc", "ls -1 ~/Cloud/Cirrove-OneDrive 2>/dev/null | wc -l")
    return {
        "wall_clock": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "slept_seconds_since_boot": round(slept_seconds(), 1),
        "boot_id": pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
        "mem_sleep": pathlib.Path("/sys/power/mem_sleep").read_text().strip(),
        "main_pid": run("systemctl", "--user", "show", "cirroved.service", "-p", "MainPID"),
        "active_since": run(
            "systemctl", "--user", "show", "cirroved.service", "-p", "ActiveEnterTimestamp"
        ),
        "account": account,
        "mount": mount,
        "mount_entries": entries,
    }


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in {"before", "after"}:
        print(__doc__)
        return 2
    now = snapshot()
    if sys.argv[1] == "before":
        STATE.parent.mkdir(parents=True, exist_ok=True)
        STATE.write_text(json.dumps(now, indent=2) + "\n")
        print(json.dumps(now, indent=2))
        print(f"\nrecorded to {STATE}")
        print("Now: echo deep > /sys/power/mem_sleep, then rtcwake -m mem -s 5400 (as root).")
        return 0

    if not STATE.exists():
        print(f"no before state at {STATE}; run `before` first", file=sys.stderr)
        return 1
    before = json.loads(STATE.read_text())
    slept = now["slept_seconds_since_boot"] - before["slept_seconds_since_boot"]
    print(f"time actually suspended: {slept:.1f} s")
    if before["boot_id"] != now["boot_id"]:
        print("REBOOTED, not suspended: this measures a different question.")
    # Fields that must be identical, and the one that must not be.
    verdict = 0
    for field in ("main_pid", "active_since", "mount"):
        same = before[field] == now[field]
        print(f"{'same' if same else 'CHANGED'}  {field}: {now[field]}")
        if not same:
            verdict = 1
    for field in ("state", "mounted", "indexed_items", "feeds"):
        b, a = before["account"].get(field), now["account"].get(field)
        note = "" if b == a else f"   (was {b})"
        print(f"account.{field}: {a}{note}")
    if slept < 60:
        print("\nWARNING: shorter than a minute. Too short to outlive a token or a subscription.")
        verdict = 1
    print(json.dumps({"before": before, "after": now}, indent=2))
    return verdict


if __name__ == "__main__":
    raise SystemExit(main())
