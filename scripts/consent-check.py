#!/usr/bin/env python3
"""Capture what revoking a Microsoft grant must not cost, and compare it after.

docs/benchmarks/revoked-grant-and-reauthentication.json registers the run. Its
point is not only that the daemon notices: it is that a credential problem costs
the user nothing they already have. So the state is written to disk before the
grant is revoked and compared after reauthentication, because "it looks the same"
from memory is not a comparison.

    scripts/consent-check.py before      # then revoke the grant
    scripts/consent-check.py watch       # while you wait, optional
    scripts/consent-check.py after       # after cirrove reauth

`watch` prints one line every five seconds so the moment the daemon notices is
observed rather than guessed at.
"""

import json
import pathlib
import subprocess
import sys
import time

STATE = pathlib.Path.home() / ".cache" / "cirrove-consent-check.json"
MOUNT = pathlib.Path.home() / "Cloud" / "Cirrove-OneDrive"


def run(*args: str) -> str:
    try:
        return subprocess.run(args, capture_output=True, text=True, timeout=30).stdout.strip()
    except Exception as error:
        return f"<unavailable: {error}>"


def snapshot() -> dict:
    account: dict = {}
    try:
        first = (json.loads(run("cirrove", "status")).get("accounts") or [{}])[0]
        account = {
            "state": first.get("state"),
            "mounted": first.get("mounted"),
            "enabled": first.get("enabled"),
            "indexed_items": first.get("indexed_items"),
            "stuck_changes": first.get("stuck_changes"),
            "feeds": [f.get("state") for f in first.get("feeds") or []],
        }
    except Exception as error:
        account = {"unreadable": str(error)}
    # A cached listing is the half that matters most: losing the grant must not
    # cost the user the metadata they already hold.
    try:
        entries = len(list(MOUNT.iterdir()))
    except OSError as error:
        entries = f"<{error}>"
    return {
        "at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "account": account,
        "mount_present": bool(run("findmnt", "-t", "fuse.cirrove")),
        "root_entries": entries,
        "pins": run("cirrove", "pins"),
    }


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode not in {"before", "watch", "after"}:
        print(__doc__)
        return 2

    if mode == "watch":
        print("state           mounted  entries  feeds")
        while True:
            now = snapshot()
            a = now["account"]
            print(
                f"{str(a.get('state')):<15} {str(a.get('mounted')):<8} "
                f"{str(now['root_entries']):<8} {a.get('feeds')}"
            )
            time.sleep(5)

    now = snapshot()
    if mode == "before":
        STATE.parent.mkdir(parents=True, exist_ok=True)
        STATE.write_text(json.dumps(now, indent=2) + "\n")
        print(json.dumps(now, indent=2))
        print(f"\nrecorded to {STATE}")
        print("Now revoke the application's access in the Microsoft account.")
        return 0

    if not STATE.exists():
        print(f"no before state at {STATE}; run `before` first", file=sys.stderr)
        return 1
    before = json.loads(STATE.read_text())
    verdict = 0
    for field in ("indexed_items", "state", "mounted"):
        b, a = before["account"].get(field), now["account"].get(field)
        same = b == a
        print(f"{'same    ' if same else 'CHANGED '} account.{field}: {a}" + ("" if same else f"   (was {b})"))
        if field == "indexed_items" and not same:
            # Reindexing 184,000 items after a sign-in is usable; losing them is
            # a different claim and the one this field is here to catch.
            verdict = 1
    print(f"{'same    ' if before['root_entries'] == now['root_entries'] else 'CHANGED '} root entries: {now['root_entries']}   (was {before['root_entries']})")
    if before["pins"] != now["pins"]:
        verdict = 1
        print("CHANGED  pins:")
        print("  before:", before["pins"].replace("\n", "\n  "))
        print("  after :", now["pins"].replace("\n", "\n  "))
    else:
        print("same     pins")
    print(json.dumps({"before": before, "after": now}, indent=2))
    return verdict


if __name__ == "__main__":
    raise SystemExit(main())
