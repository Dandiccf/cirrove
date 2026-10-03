# Isolated iCloud replacement provider phases, 2026-09-29

## Registered before live execution

Question: which replacement provider call accounts for the approximately
466 seconds between local FUSE save and upload confirmation in the prior
small-file run, and which calls return errors or are cancelled by the worker
timeout? Its journal retained two failed attempts but not their causes.

The isolated mounted write adapter now reports only phase name (`begin`,
`inspect`, `part`, `stream`, `commit`, `reconcile`), duration and boolean
success; a dropped future is marked separately. It prints no provider body,
token, cursor, signed URL, account, operation or item ID. The existing outer
probe reports mount, FUSE save and independent-check boundaries.

Use a new Cirrove-owned fixture under private run
`f94c8c38-3b61-4e5a-852c-97a836e1e6ad`. Arm A creates the source through
FUSE and verifies exact bytes and receipt. Arm B replaces it through FUSE
and requires the two-ID receipt, correct new bytes and recoverable old ID.
Arm C remounts in a fresh process and verifies without another mutation.
Stop on the first failure or uncertainty. Prediction: `begin` or one staged
handoff `commit`/`inspect` call will dominate the upload time; if a call is
dropped near 125 seconds, the worker's per-call deadline likely contributes
to the failed attempts. This is a hypothesis, not an observed cause.

Each arm must have a manifest written before execution with command, binary
SHA-256, PID, expected duration and separate disk-backed `TMPDIR` and
`SQLITE_TMPDIR`. Bounds are 360 seconds for A and C and 900 seconds for B.
No other local measurement or compilation overlaps. The installed daemon,
normal iCloud connections and other mounts stay untouched. One arm per
condition locates a phase but provides no within-arm spread or reliability
claim. Ordinary iCloud remains read-only.

## Observation

All three arms passed for owned fixture
`506244b4-79c2-40a8-a075-583a931c24e9`. Arm A created and verified the
source. Arm B completed mounted two-ID replacement with independent bytes,
receipt and recoverable old ID; arm C remounted in a new process and verified
the result without mutation. The test mounts shut down and the ordinary
installed service was not restarted.

Arm B's mount was ready at 1.4 seconds and local FUSE save returned at 1.9
seconds. `begin` finished successfully in 3.3 seconds, `inspect` in 1.8,
and `stream` in 2.7. The first `commit` succeeded after 41.1 seconds.
The next `commit` was dropped by the worker at exactly 125.0 seconds.
The following `inspect` succeeded after 67.6 seconds and produced another
commit step; that `commit` was again dropped at exactly 125.0 seconds.
The final `inspect` succeeded after 98.7 seconds, and the upload was
confirmed at 473.7 seconds. The independent listing finished at 474.2,
old-ID Trash confirmation at 504.8, and final byte verification at 506.5
seconds. The replacement journal row retained two failed attempts.

The adapter control flow implies the first timed-out handoff commit was
`MoveOld` and the second was `InstallNew`; each later inspection observed
the remote phase had advanced and recovery completed without a third
commit. This locates the latency in the two handoff commits and their
reconciliation checks. It does **not** identify whether the delay within
either commit was the Apple mutation request, a preflight observation or
a post-mutation observation. That narrower diagnosis is needed before
changing deadlines or removing a correctness check. The prediction that
125-second worker deadlines contribute to the failed attempts was supported.

Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are in
`.local-state/icloud-replacement-provider-phases-f94c8c38-3b61-4e5a-852c-97a836e1e6ad/`.
Each records binary SHA-256
`945a35bfed266f8ff99fca275c2cb8b02b1cb5748b996ca9487106c64a1cabde`,
PID, expected duration and separate btrfs-backed temporary directories.
No compilation or other local measurement overlapped the arms. There is
one run per arm and no within-arm spread; this is phase evidence, not a
reliability or normal-account write-readiness claim.
