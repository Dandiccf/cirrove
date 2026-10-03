# Mounted iCloud deletion recovery — 2026-10-01

## Preregistered question

Does the ordinary account router recover a FUSE unlink after process exit between
confirmed recoverable Trash and the mutation worker's journal receipt, without
sending another mutation? This extends the adapter-only lost-response result.

## Arm and endpoint

Use a fresh owned validation folder and mount. Create and fsync one file through
FUSE; await upload and independently verify its digest. Unlink with a held read
handle and verify the old bytes. Record that application result separately.
Exit the process with code 86 immediately after the router returns the exact
Removed receipt, before the worker receives it. Flush the boundary marker first.
Detach only this run's disconnected FUSE mount after process exit.

The second process must observe one VerifyRequired mutation, exact prepared
identity and no journal receipt. Mount the same isolated state with all mutation
dispatch refused. Require Applied with the exact receipt, independent presence in
recoverable Trash and absence from the active parent, and absent pathname through
the mount. Remount once more and verify journal and namespace. No permanent
deletions; no personal files or regular service changes.

Prediction: both recovery and remount succeed with zero replay attempts. The
interrupt process exits 86, recovery exits 0. Each process is bounded to 1800
seconds, uses a private disk-backed TMPDIR/SQLITE_TMPDIR and records executable
SHA, command and PID before work. No other measurement or compilation overlaps.
One functional arm cannot establish repeatability, in-flight network ambiguity,
power-loss durability, all deletion workflows or overall write readiness.

## Result

Passed, run `65f8ce7c-51b1-46c4-b813-c1f46f8edc60`:

- [Interruption](icloud-mounted-delete-65f8ce7c-51b1-46c4-b813-c1f46f8edc60-interrupt-live.json): exit 86 at the registered boundary, 76.641 seconds.
- [Recovery](icloud-mounted-delete-65f8ce7c-51b1-46c4-b813-c1f46f8edc60-recover-live.json): exit 0, 72.731 seconds, zero mutation replay attempts.

Both used binary SHA-256
`cc64b9b2c176905cb882e2dff82b49fba4225fb670397753ba29810042c79122`
and btrfs temporary storage. Held-descriptor bytes after unlink, the exact pending
journal identity before recovery, exact Removed receipt afterward, independent
Trash presence and active-parent absence, and pathname absence after remount all
passed. Both isolated mounts were detached; normal installations were untouched.
These are whole-process durations, not isolated operation latency; no repeated-arm
spread was measured.

Two synthetic tests cover scope, identity, revision, parent, name, size, kind,
package and receipt rejection, plus refusal of both the original and an unexpected
operation ID during recovery. This adds a validation boundary, not a production
behavior fix. The preceding exact-ID Trash implementation supplies the verified
reconciliation path.

Full `scripts/check.sh` passed (exit 0), UTC 2026-09-30 22:39:28–22:45:40:
formatting, clippy, workspace/feature tests, kernel FUSE tests, scripts, ledger and
documentation. Private manifest and log are retained under
`.local-state/icloud-mounted-delete-check-2026-10-01/`. The preexisting rustdoc
`retry_stuck` link warning remains. GUI window tests are outside this command;
no desktop behavior changed. This validation-only change was not installed into
the regular daemon.
