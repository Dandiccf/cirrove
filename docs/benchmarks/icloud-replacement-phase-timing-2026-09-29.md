# Isolated iCloud replacement phase timing, 2026-09-29

## Registered before live execution

Question: which boundary accounts for the previous small-file replacement
arm's roughly eight-minute elapsed time: mount setup, the FUSE application,
upload worker confirmation, or independent iCloud verification? The prior
exact-operation run records only overall wall time and a journal save time.
This run adds elapsed-time markers at those boundaries. Markers contain no
provider identifiers, URLs, credentials, response bodies or file contents.

The arms use a fresh Cirrove-owned fixture and private run identifier
`84aeca79-efe0-4ae1-9cbf-59ce6d8e0456`. Arm A creates the small source
through FUSE and independently verifies its bytes and receipt. Arm B replaces
it through FUSE, records cumulative elapsed time at each boundary, and
requires the same exact-ID, complete-byte and recoverable-Trash checks as the
prior run. Arm C remounts in a new process and independently verifies the
result without another mutation. Stop on the first failure or uncertainty.

Prediction: arm B spends most of its time before upload confirmation; the
previous journal save time near the start suggests that the late independent
checks alone are less likely to explain eight minutes. This is a hypothesis,
not an observed phase duration. All three arms should pass integrity checks.
The expected bounds are 360 seconds for A and C and 900 seconds for B. Each
arm gets a manifest with command, binary SHA-256, PID, duration and separate
disk-backed `TMPDIR`/`SQLITE_TMPDIR`. No other local measurement or compilation
will overlap. The ordinary installed service and its mounts remain untouched.

One repetition can locate a bottleneck but cannot establish a stable latency
or quantify within-arm spread. Do not infer normal-account write readiness.

## Observation

All three arms passed for owned fixture
`987d13ab-25f4-4ca4-8f7b-e71b7d07a706`. Arm A created and independently
verified the source. Arm B completed the two-ID replacement with independent
new-byte, identity, journal and recoverable-Trash verification. Arm C used a
new process and remount and confirmed the result without another mutation.
The test mounts shut down; the ordinary installed service was not restarted.

Arm B's cumulative markers were: mount ready 1.4 seconds, FUSE save returned
1.9 seconds, uploads confirmed 468.2 seconds, mutations confirmed 468.2
seconds, independent listing 468.6 seconds, old ID confirmed in Trash 498.9
seconds, and final independent bytes verified 500.5 seconds. Thus about 466
seconds elapsed between local save and upload confirmation; the independent
listing cost under one second and the Trash check about 30 seconds. The
prediction that the upload path dominates was supported. The journal row
retained two failed attempts and a retry timestamp, but does not retain their
causes or phase durations. The exact wait inside the upload worker remains
unproven and needs narrower measurement before a performance fix is claimed.

Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are in
`.local-state/icloud-replacement-phase-timing-84aeca79-efe0-4ae1-9cbf-59ce6d8e0456/`.
Each records binary SHA-256
`ccf362c7a7690f267b7b70b9a056213fe696d43c98a65ce531827530f8627836`,
PID, expected duration and btrfs-backed separate temporary directories.
No compilation or second local measurement overlapped the arms. One run per
arm has no within-arm spread and establishes neither latency stability nor
ordinary-account write reliability.
