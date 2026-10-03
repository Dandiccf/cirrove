# Bounded iCloud commit deadline, 2026-09-29

## Registered before live execution

Question: retaining the complete preflight and postflight integrity checks,
does a 300-second commit deadline avoid the two premature 125-second
cancellations observed in the owned mounted replacement? Only the isolated
iCloud replacement adapter requests 300 seconds; other uploads retain 125.
No ordinary iCloud write route is enabled.

The synthetic test
`provider_commit_deadline_preserves_checkpoint_and_payload_for_recovery`
was first run with the worker's old hard-coded 125-second limit and failed
its two-second outer guard. Restoring provider deadline dispatch made it
pass: a never-completing commit became VerifyRequired at the requested short
deadline, retained payload/checkpoint, and resumed without a second begin.
This proves worker dispatch and preservation, not Apple reliability.

Use fresh owned fixture under private run
`da5fc50b-a316-4d50-ab2f-4f51fd04f1b9`. A creates and independently
verifies a small file. B replaces it with full preflight/postflight checks
and requires zero failed attempts, complete new bytes, exact two-ID receipt
and old identity in recoverable Trash. C remounts in a new process and
verifies without another mutation. Stop the sequence on failure or
uncertainty and retain evidence. Prediction: all arms pass without commit
deadline drops; the longest combined check remains below 300 seconds.

One repetition supports direction only; no within-arm spread is available.
This cannot establish a stable latency improvement or solve ambiguous
response recovery, including the retained failed phase-split fixture.
Each arm records command, binary SHA-256, PID, expected duration and private
disk-backed TMPDIR/SQLITE_TMPDIR before execution. Bounds are 360 seconds
for A/C and 900 for B. No concurrent measurement or compilation runs, and
the installed daemon and normal mounts remain untouched.

## Observation

All three arms passed for owned fixture
`0858d51b-f442-4b31-bd38-7fd6e0d02c13`. Arm A independently verified
the new source. Arm B completed full mounted replacement, exact-byte and
two-ID checks and old-ID recoverable Trash verification. Both upload rows
were `uploaded` with `failed_attempts = 0`. Arm C remounted in a fresh
process and independently verified the result without another mutation.
All test mounts shut down; the ordinary installed service was untouched.

The MoveOld commit completed in 153.2 seconds: preflight 5.0, accepted
Trash request 20.1, postflight 128.1. InstallNew completed in 193.7 seconds:
preflight 64.5, accepted rename 2.2, receipt verification 126.9. Neither
call was dropped. Upload confirmation arrived at 398.0 seconds, independent
listing at 398.5, old-ID Trash check at 427.6 and final bytes at 429.5.
The single pre-change inner-phase run confirmed upload at 478.9 seconds
with two deadline drops, but these are one run per condition with no
within-arm spread. The measured direction is encouraging; no stable speedup
or general reliability claim follows, and total latency is still excessive.

Private arm manifests are in
`.local-state/icloud-handoff-commit-deadline-da5fc50b-a316-4d50-ab2f-4f51fd04f1b9/`.
They record binary SHA-256
`879491f85016387170fcb3a57f51f313b6db3dfe23f31ce80cbffa34dbd6f943`,
PIDs, start times, bounds and separate btrfs-backed temporary directories.
No compilation or second local measurement overlapped. The retained failed
phase-split fixture has not been repaired by this successful fresh run.
