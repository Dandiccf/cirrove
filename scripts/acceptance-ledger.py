#!/usr/bin/env python3
"""Keep the acceptance ledger honest against docs/product-milestones.md.

Fifty-one acceptance boxes live in prose across one document, and what stands
between each of them and being ticked lives in several others. That makes "meets
our requirements" unfalsifiable: nobody can say what is left without reading
everything, and a box can be ticked on a feeling.

The ledger gives each box a row. `asserted_by` names the test that would fail if
the claim stopped holding, and this script refuses a ticked box that has none.
`blocker` says what stands in the way -- not how hard the code is, but whether
finishing it needs a live account, root, a reboot or an exclusive machine window.
That distinction is what separates work that can be finished from work that can
only be prepared, and it belongs in a file rather than in someone's head.

    scripts/acceptance-ledger.py           # verify
    scripts/acceptance-ledger.py --sync    # adopt new or reworded boxes
"""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MILESTONES = ROOT / "docs/product-milestones.md"
LEDGER = ROOT / "docs/acceptance-ledger.json"

# Whether an open row stands between the project and a 1.0 release. The
# criteria are in docs/release-procedure.md; "no" always carries a reason,
# because a "no" without one is how a release quietly shrinks its own scope.
RELEASE = {"yes", "no"}

BLOCKERS = {
    "autonomous",
    "needs-live-account",
    "needs-root",
    "needs-reboot",
    "needs-machine-window",
    "out-of-agreed-scope",
    "unclassified",
}


def boxes() -> list[dict]:
    """Every acceptance checkbox, with the milestone it belongs to."""
    found = []
    milestone = None
    open_box = False
    for number, line in enumerate(MILESTONES.read_text().splitlines(), 1):
        heading = re.match(r"^## (\d)\. (.+)$", line)
        if heading:
            milestone = (int(heading.group(1)), heading.group(2))
            open_box = False
            continue
        box = re.match(r"^- \[([ x])\] (.+)$", line)
        if box and milestone:
            found.append(
                {
                    "milestone": milestone[0],
                    "milestone_title": milestone[1],
                    "line": number,
                    "done": box.group(1) == "x",
                    "text": re.sub(r"\s+", " ", box.group(2)).strip(),
                }
            )
            open_box = True
            continue
        # A wrapped box continues on the indented lines directly beneath it.
        # Taking only the first line truncated two of the longest claims --
        # including the one naming the 24-hour churn gate -- so the ledger held
        # half a sentence and would not have noticed the other half being
        # reworded. A blank line, a heading or an unindented line ends the item;
        # without that, the last box swallowed the prose paragraph after it.
        if open_box and re.match(r"^ {2,}\S", line):
            found[-1]["text"] = re.sub(
                r"\s+", " ", found[-1]["text"] + " " + line
            ).strip()
            continue
        open_box = False
    return found


def classification(row: dict) -> str:
    """A row's blocker as a sortable name.

    A row whose `blocker` is null is already reported as a problem above, but the
    summary is printed first and `sorted` on a mix of `None` and `str` raises
    TypeError. The guard then dies with a traceback instead of saying which row
    is unclassified -- a script that fails illegibly is barely better than one
    that does not fail, and this one is what CI shows a person first.
    """
    return row.get("blocker") or "unclassified"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sync", action="store_true")
    parser.add_argument(
        "--blockers",
        action="store_true",
        help="list the open rows that block a 1.0 release, and those that do not",
    )
    args = parser.parse_args()

    current = boxes()
    ledger = json.loads(LEDGER.read_text())
    known = {row["text"]: row for row in ledger["rows"]}

    added = [box for box in current if box["text"] not in known]
    removed = [text for text in known if text not in {b["text"] for b in current}]

    if args.sync:
        rows = []
        for box in current:
            row = known.get(box["text"], {})
            rows.append(
                {
                    **box,
                    "blocker": row.get("blocker", "unclassified"),
                    "blocks_release": row.get("blocks_release"),
                    "release_note": row.get("release_note"),
                    "evidence": row.get("evidence"),
                    "asserted_by": row.get("asserted_by"),
                }
            )
        ledger["rows"] = rows
        ledger["totals"] = {
            "boxes": len(rows),
            "ticked": sum(row["done"] for row in rows),
            "by_blocker": {
                blocker: sum(1 for row in rows if classification(row) == blocker)
                for blocker in sorted({classification(row) for row in rows})
            },
        }
        LEDGER.write_text(json.dumps(ledger, indent=2) + "\n")
        print(f"synced: {len(added)} added, {len(removed)} removed")
        return 0

    problems = []
    for text in sorted(t["text"] for t in added):
        problems.append(f"box not in the ledger: {text[:80]}")
    for text in sorted(removed):
        problems.append(f"ledger row no longer in the document: {text[:80]}")

    for box in current:
        row = known.get(box["text"])
        if row is None:
            continue
        if row["done"] != box["done"]:
            problems.append(
                f"ledger and document disagree on whether this is done: {box['text'][:70]}"
            )
        if box["done"] and not row.get("asserted_by"):
            problems.append(
                f"ticked with nothing asserting it: {box['text'][:70]}\n"
                "    Name the test that would fail if this stopped holding, or untick it."
            )
        if row.get("blocker") not in BLOCKERS:
            problems.append(f"unknown blocker {row.get('blocker')!r}: {box['text'][:60]}")
        # Only open rows can block; a ticked one is already out of the way.
        if not box["done"]:
            decision = row.get("blocks_release")
            if decision not in RELEASE:
                problems.append(
                    f"no release decision on an open row: {box['text'][:70]}\n"
                    "    Set blocks_release to \"yes\" or \"no\"; a \"no\" needs a release_note."
                )
            elif decision == "no" and not row.get("release_note"):
                problems.append(
                    f"said not to block a release without saying why: {box['text'][:70]}\n"
                    "    A \"no\" with no reason is how a release loses scope without anyone choosing to."
                )

    # The totals block is written only by --sync, and nothing used to check it, so
    # ticking a box left the summary behind. Anyone reading the JSON rather than
    # running this script then saw a stale count -- which is the exact failure this
    # file exists to prevent, one level up.
    totals = ledger.get("totals", {})
    expected = {
        "boxes": len(ledger["rows"]),
        "ticked": sum(row["done"] for row in ledger["rows"]),
    }
    for field, value in expected.items():
        if totals.get(field) != value:
            problems.append(
                f"totals.{field} says {totals.get(field)}, the rows say {value}; "
                "run --sync to rewrite the summary"
            )

    counts = {}
    for row in ledger["rows"]:
        counts[classification(row)] = counts.get(classification(row), 0) + 1
    print(
        f"{len(current)} acceptance boxes, {sum(b['done'] for b in current)} ticked; "
        + ", ".join(f"{n} {b}" for b, n in sorted(counts.items()))
    )
    blocking = [r for r in ledger["rows"] if not r["done"] and r.get("blocks_release") == "yes"]
    waived = [r for r in ledger["rows"] if not r["done"] and r.get("blocks_release") == "no"]
    print(
        f"{len(blocking)} open row(s) block a 1.0 release, {len(waived)} do not "
        "(scripts/acceptance-ledger.py --blockers lists them)"
    )
    if args.blockers:
        print("\nWhat stands between this and a 1.0 release:")
        for row in sorted(blocking, key=lambda r: (r["milestone"], r["line"])):
            print(f"  M{row['milestone']}  {row['text'][:96]}")
        print("\nOpen, and deliberately not blocking:")
        for row in sorted(waived, key=lambda r: (r["milestone"], r["line"])):
            print(f"  M{row['milestone']}  {row['text'][:70]}")
            print(f"        {row.get('release_note', '')[:150]}")
    if problems:
        print("\n" + "\n".join(f"  {p}" for p in problems))
        print(
            "\nRun with --sync to adopt document changes, then classify and give "
            "each row its assertion."
        )
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
