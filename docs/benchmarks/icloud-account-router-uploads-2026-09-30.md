# iCloud account-router create and replacement acceptance

Registered before any live invocation on 2026-09-30.

Question: can the normal `ICloudWriteProvider`, constructed from an Engine-owned
`WriteContext`, complete create and two-ID replacement through `TransferWorker`,
retain the old ID in Trash, and recover both confirmed receipts after local
state is reopened? Earlier mounted experiments used the separate fixture router.

## Protocol and prediction

The feature-gated mounted-probe binary accepts `--account-uploads RUN_UUID`.
This particular mode does **not mount a filesystem**. It opens a disabled,
new account identity in private state, with its own encrypted session copy,
Engine ownership, index, journal and sealed checkpoint vault. It creates a new
`Cirrove Write Validation-RUN_UUID` remote folder and touches only a new
`Account Router.txt` inside it. No installed daemon, existing account settings,
or ordinary user files are changed. Reusing a run directory is refused.

1. Create known nonempty bytes using the actual account router and transfer worker.
2. Independently hash the exact remote ID/revision and compare expected bytes.
3. Drop the Engine, provider, journal and vault; open fresh instances.
4. Replace that exact owned file through a local working-file snapshot, namespace
   identity and the actual transfer worker. Bare upload enqueue is insufficient
   for the journal's identity-handoff reservation.
5. Independently hash the installed replacement and check the exact old ID in Trash.
6. Drop and reopen local state once more; verify both durable successful receipts.

Prediction: the first attempt either closes this narrow routing gap or exposes
an integration mismatch before general write enablement. No timing improvement
is predicted. Earlier remote integrity checks took minutes; permit up to 20
minutes per run. Failures retain the run state and are never automatically retried.
A missing/uncertain response is not proof that no remote mutation occurred.

Run A is an integration smoke arm. If it passes, a fresh run B repeats the same
protocol, sequentially. Record both elapsed times and their spread. Two passes
would demonstrate this bounded protocol, not broad reliability or FUSE coverage.
Process termination during handoff, root-level replacement, nested folder
operations, large/empty files and concurrent-editor behavior remain separate gates.

Before launching each arm, record command, run UUID, source revision/dirty state,
binary SHA-256, PID, start time, expected duration and disk-backed TMPDIR and
SQLITE_TMPDIR in its private manifest. Assert the temporary filesystem with
`findmnt`; no compilation or competing measurement runs during a live arm.
Never record credentials, raw provider bodies, signed URLs or file contents.
Retain all cloud fixtures and private state for review.

## Results

Arm A started with run `9263bc51-b980-404b-bf6a-a5cc78cec421`.
Private evidence: `.local-state/icloud-account-router-arm-9263bc51-b980-404b-bf6a-a5cc78cec421/`.
The manifest records the binary hash, PID and btrfs temporary filesystem.
Arm A exited 0 after **508.248 seconds**. Create and independent digest passed;
replacement and independent digest passed; the old exact ID was observed in
recoverable Trash; reopening Engine/context/journal retained both successful
receipts. No FUSE mount was used. Phase times were: move-old preflight 5.2 s,
move-old request 19.1 s, postflight 125.2 s, install preflight 62.9 s, install
request 2.5 s and install receipt 130.3 s. These numbers cover one run only;
no within-arm spread is available yet. The repeated backup checks dominate the
observed protocol latency and remain a product performance issue.

Synthetic queue coverage passed. Removing the exact parent binding made the
foreign-source rejection assertion fail (negative control exit 101); restoring
it restores the production guard. Full account write access remains disabled.

Arm B `a84edb9c-3552-4d1a-9e71-27abbecc32b4` used the identical binary and
protocol and exited 0 after **503.121 seconds**, with all the same checks passing.
Private evidence is in `.local-state/icloud-account-router-arm-a84edb9c-3552-4d1a-9e71-27abbecc32b4/`.
Its phase times were 5.0, 19.0, 129.8, 64.2, 2.2 and 127.6 seconds in the same
order as A. The two-run elapsed range is **503.121–508.248 seconds**, a spread of
**5.127 seconds** (about 1.0% of their 505.685-second mean). This is repeated
functional acceptance of this bounded non-mounted protocol, not evidence of
account-wide reliability, a performance improvement, or mounted application saves.
Both processes exited; neither fixture was automatically retried or cleaned up.
