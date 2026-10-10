# Durable iCloud handoff phase split, 2026-09-29

## Registered before live execution

Question: can the owned mounted replacement complete without 125-second
worker timeouts by checkpointing the accepted Trash mutation before the
old-ID backup verification and checkpointing the accepted staged rename
before the final receipt verification, while retaining full exact-ID and
byte checks before another mutation or a successful journal receipt?

The preceding `icloud-handoff-inner-phases` arm is the unmodified baseline:
both Apple requests succeeded in 19.7 and 2.2 seconds, but the combined
postflight/receipt awaits were dropped at the worker deadline, producing
two failed attempts and upload confirmation at 478.9 seconds. The new
`VerifyNew` phase is observation-only. The shared worker persists a sealed
checkpoint before each next commit. `InstallNew` still performs the full
old-ID/backup preflight before renaming staged content; `VerifyNew` requires
the full final receipt before upload acknowledgement. An uncertain or lost
response must reconcile from the preceding checkpoint rather than repeat
either remote mutation.

Use a fresh Cirrove-owned fixture under private run
`36543a91-d2b8-477e-8ffb-903fe2e56d82`: A creates and independently
verifies the source file, B replaces it through FUSE and requires no
125-second provider drop, zero journal failed attempts, exact new bytes,
recoverable old ID and a two-ID receipt, and C remounts in a fresh process
to verify without mutation. On any failure or uncertainty stop and retain
the fixture. Prediction: all arms pass; each handoff observation fits a
separate 125-second call and B's upload confirmation is earlier than the
478.9-second baseline. A single arm does not establish a stable performance
gain. The correctness endpoint is strict even if the timing prediction
fails.

Each arm records command, binary SHA-256, PID, expected duration and
separate disk-backed `TMPDIR`/`SQLITE_TMPDIR` before execution. Bounds:
360 seconds for A and C, 900 seconds for B. No other local measurement or
compilation overlaps. Installed service and ordinary mounts stay untouched;
normal iCloud remains read-only.

## Observation

Arm A passed for fresh owned fixture
`e83673ff-dfdf-452d-8284-90c2cf1c8c7c`. Arm B failed the registered
zero-timeout endpoint and was stopped; arm C was not run.

Mount and FUSE save were ready at 1.4 and 1.9 seconds. The accepted MoveOld
commit finished in 25.0 seconds (4.9-second preflight and 20.0-second
request). The following InstallNew preflight itself exceeded 125 seconds;
it was dropped before the rename request was sent. Subsequent full
observations finished in 63.5, 62.7 and 63.0 seconds, but the saved
InstallNew checkpoint and OldAtRecovery observation did not yield a
resumable commit. The worker kept observing instead of advancing. This
is a recovery boundary defect as well as a failed latency prediction.

After this repeated non-advancing observation, SIGINT was sent only to
recorded PID 936888 after checking its exact command line. The probe exited
with code 1 and retained its fixture and journal; the test mount shut down.
The replacement row remained `verify_required` with three failed attempts,
its payload and sealed session reference retained. No completed replacement
or final independent byte check is claimed. The earlier observation reported
the old identity in recovery and the staged replacement; this is not a new
post-shutdown remote audit.

The phase-split implementation was reverted. The next approach retains all
before/after checks and evaluates a longer bounded commit deadline for the
owned iCloud adapter. This does not resolve ambiguous-response recovery;
the retained fixture remains evidence for that open gate.

Private arm A/B manifests are in
`.local-state/icloud-handoff-phase-split-36543a91-d2b8-477e-8ffb-903fe2e56d82/`.
Both record binary SHA-256
`a6dae703c33b43e0810f288a7f5720d791a93c843a6bd2268688b40cd32324cf`
and btrfs-backed temporary directories. No concurrent compilation or second
measurement ran. No reliability or stable performance improvement was shown.
