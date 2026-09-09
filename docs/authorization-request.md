# What is left, and what it needs from you

Everything in milestones 1 and 2 that can be finished without a person at the
keyboard has been. What remains is not blocked on effort or on understanding; each
item needs something an autonomous agent structurally cannot supply. This is that
list, with the command, the blast radius and the evidence each run would produce,
so that a session with you present can work through it rather than rediscover it.

`docs/acceptance-ledger.json` carries the same information per acceptance box,
with `blocker` naming the class. This document is the operator's version of it.

## Live provider, read only — about 15 minutes plus one wait

Six open boxes turn on a real Microsoft account. No *automated test* contacts one:
every provider in the suites is an in-process fake or a loopback HTTP server the
real adapter talks to, and `AGENTS.md` requires explicit authorization for anything
else.

That is a statement about the suites, and an earlier wording of it -- "nothing in
the repository has ever contacted one" -- was read as a statement about the
product and repeated as such. It is not true of the product. Live evidence against
a real business drive exists from 2026-09-06, and the expanded write sequence ran
on 2026-09-09; both are recorded in `docs/validation.md`. What the boxes below
need is the *remaining* live evidence, not a first contact.

Connect a **separate** grant, not your working account:

    cirrove connect --label validation --state-dir ~/.local/state/cirrove-validation

That opens a browser for consent and writes to the desktop keyring. Then:

    cirrove validate-onedrive-freshness    --label validation --state-dir ...
    cirrove validate-onedrive-notifications --label validation --state-dir ... --check-renewal

The renewal check waits out a real subscription lifetime — about **50 minutes**
during which the machine must stay awake. It is the only way to see a renewal the
provider actually performed rather than one a fixture simulated.

**Blast radius:** reads only. No file is created, changed or deleted.

**Evidence produced:** real request counts and latency distributions on the
conservative and optimized read paths, actual notification delivery latency
separated from local reaction latency, and directory freshness through a mounted
filesystem. Four of the six boxes need exactly this.

## Live provider, writable — about 45 minutes, and it changes cloud data

Milestone 2's expanded sequence **ran on 2026-09-09 and passed**; it is recorded at
the end of `docs/validation.md`. It is repeatable, and it runs in a folder it
creates each time:

    cirrove validate-onedrive-uploads   --label validation --state-dir ... --write-access
    cirrove validate-onedrive-mutations --label validation --state-dir ... --write-access
    cirrove validate-onedrive-writable  --label validation --state-dir ... --write-access

**Blast radius:** a dedicated Cirrove test folder. It creates generated files,
replaces two of them atomically, and conditionally deletes three sources it
created itself. `CONTRIBUTING.md` forbids running these against a production
drive, and the validator refuses an account that is enabled. Nothing outside that
folder is touched — but these are real cloud mutations and they are not
reversible by re-running.

**Evidence produced:** atomic replacement and conditional source cleanup against
the real Graph API, including an online-only source after a remount, which is the
one shape synthetic fixtures cannot construct.

## Physical faults — needs root, and one part needs hardware

The crash box now covers process death at all 31 durable transitions, measured.
What it does not cover is faults below the process:

    # a device that drops writes after N sectors
    sudo dmsetup create flaky --table "0 $SECTORS flakey /dev/loop0 0 8 4"

Torn writes, a lying `fsync` and block-layer `ENOSPC` are reachable with
`dm-flakey` or `dm-error` and root. A true power cut is not scriptable at all and
needs someone at the wall socket, or a machine with a controllable PSU.

**Evidence produced:** whether the journal's durability guarantees hold when the
storage layer lies, which is the failure mode the whole local-edit journal exists
to survive.

## Suspend and resume — ends the session that would observe it

One clause of milestone 1's recovery box. A real S3 or S4 cycle suspends the
machine, so no process running on it can watch itself come back. Its two sibling
clauses, process failure and loss of network access, are covered.

A cgroup-freezer plus a `CLOCK_REALTIME` skew against `CLOCK_MONOTONIC` would make
a reasonable regression test and could be written autonomously. It is not the
evidence the clause asks for, and building it should be a deliberate decision
rather than a substitution.

## Installable service, login startup, reboot — ends the session

`systemctl --user enable cirroved` changes system state, and verifying login
startup means logging out and back in. Neither is something to do to a machine
while working on it.

## The twenty-four hour run — prepared, deliberately not started

`scripts/sustained-run.py` is ready. Run the pilot first; the full profile refuses
to start without an explicit `--tolerance`, because the only sustained datum that
existed before this work was a 65-second debug run rising 9.7 percent per round,
and a rule chosen blind would fail both arms inside the first hour.

    scripts/sustained-run.py pilot --binary <frozen release test binary>
    scripts/sustained-run.py full  --binary <same binary> --tolerance <from the pilot>

**Do not start the full run yet.** It should measure a core that has stopped
changing, and the peak-memory criterion has no remedy: shedding was measured
unable to sustain the required rate, and the two remaining directions are larger
than shedding was. A day of exclusive machine time spent on a core that is about
to change buys nothing.

## What this list is not

It is not a claim that the remaining work is small. Eleven boxes in milestones 1
and 2 are still open: six need the live account, two a reboot, two an exclusive
machine window and one root. The evidence clause of milestone 1 asks for
provider-backed reads and ordinary desktop applications, so even a perfect
synthetic result leaves its boxes open -- which is why "responsive navigation"
is on this list rather than closed by its 500 ms assertion. What the list does
claim is that nothing here is waiting on analysis.
