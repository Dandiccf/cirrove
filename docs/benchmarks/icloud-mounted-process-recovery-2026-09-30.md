# Mounted iCloud replacement after process interruption — 2026-09-30

Registered before the live run. Question: can the regular account router recover
an interrupted FUSE replacement when Apple has accepted the old-file Trash step,
but the worker still has the preceding `move_old` checkpoint?

## Scope and arms

Use one new UUID-named Cirrove-owned folder and one small synthetic text file.
The isolated disabled account uses the regular writer, encrypted checkpoint vault,
journal and FUSE workers. The existing ownership guard restricts all mutations to
confirmed fixture identities. No installed daemon, ordinary account, previous
failed fixture, native document package or permanent deletion is involved.

Arm A (`--account-mounted-interrupt UUID`) creates and independently hashes the
original, then truncates/refills it through the mounted filesystem. The probe
waits for the normal writer's conditional Trash request and its read-only
postflight to succeed. Before returning the next checkpoint to the worker it
fsyncs a non-secret boundary marker and exits the entire process with code 86,
without orderly shutdown. This tests process interruption after a confirmed
response, not a request still in flight or power loss.

Detach only this arm's disconnected FUSE mount if it remains attached. Do not
traverse or delete its contents. Retain the journal, sealed checkpoint and files.

Arm B (`--account-mounted-recover UUID`) runs in a fresh process. It requires the
original receipt, the same replacement operation in VerifyRequired, retained
pending bytes, a reserved old identity and the preceding `move_old` checkpoint.
Before starting a writer it independently verifies both exact remote identities
and full content digests at the expected old-in-Trash/new-at-staging positions.
The probe refuses any commit that would re-enter Trash or staging and counts such
attempts. Recovery must finish the same operation without any refused attempt,
verify both full byte versions and their exact identities, read the final file
through FUSE, then unmount/reopen and read it again. A create-new marker prohibits
rerunning an uncertain recovery arm to manufacture a pass.

Prediction: journal opening fences the dead worker, reconciliation discovers the
already accepted Trash operation and advances to installation of the staged ID.
No new upload or second Trash call is required. The final journal owns the new ID
and retains the old ID as recoverable. If observations disagree, the arm must
fail with all local and remote evidence retained.

Endpoint: Arm A exit 86 with the marker; Arm B exit 0 with two confirmed uploads,
the same replacement operation, preserved original/current digests, zero attempted
Trash/staging replays and a successful remount read. One controlled sequence does
not close all recovery timings, repeated reliability, concurrent editors, session
expiry, quota failures or recovery UX. Normal write access remains disabled.

## Execution discipline

Run one process at a time with no compilation. Before launch, record command,
binary SHA-256, PID, private disk-backed TMPDIR/SQLITE_TMPDIR, expected 1800-second
duration per arm and result path in a private manifest. Assert btrfs (not tmpfs).
Stop only the recorded child PID on a deadline. Never print session/checkpoint
secrets, URLs, raw provider bodies or file contents. Public results contain only
counts, phases, exit status and timings; no performance comparison is claimed.

Synthetic checks cover rejection of unknown/legacy/oversized checkpoint shapes,
refusal of destructive replay, unaffected unrelated commits, and a real child
process exiting before it can return the next checkpoint. These test the fault
harness; only the registered live sequence can establish the provider outcome.

## Pre-live checks

The four synthetic recovery-harness tests passed (including the explicitly
spawned exit-86 child); the child-only test remains ignored in ordinary test
selection. Full `scripts/check.sh` passed at 17:45:24 UTC on 2026-09-30, including
kernel mounts and the feature-gated iCloud tests. Private check manifest/log:
`.local-state/icloud-process-recovery-check-2026-09-30/`. The subsequent feature-gated
probe build passed. No installed daemon was changed. Live results remain pending.
