# Lost iCloud registration confirmation through the account router

Registered before live execution, 2026-10-01. Question: can the ordinary account
router and transfer journal recover a remotely committed create whose registration
confirmation was not accepted, without submitting a second upload/registration?

One fresh Cirrove-owned folder and a 64 MiB + 17-byte position-dependent payload.
The feature-gated router enables the existing adapter response-discard mechanism
for this exact operation UUID only. Normal binaries have no switch. Require the
worker to persist `verify_required`, without a remote receipt, and retain the
allocated document identity plus completed-body receipt in its encrypted checkpoint.
Before exit 86, independently list the owned folder and hash the exact reserved
file/revision. There must be precisely one file and all bytes must match. If that
proof fails, retain the failed fixture and do not start recovery.

A fresh process reopens the same isolated account, verifies the pending scope,
intent, size/hash and checkpoint, and independently verifies the same remote
identity/revision again. Export and verify the local sealed generation before
reconciliation. A guard permits inspection/reconciliation only for that exact
request and completed-body checkpoint; begin, stream, part and commit calls are
refused and counted. Require `uploaded`, the original remote ID and ETag, zero
refused replay attempts, at least one read-only verification call, no duplicate,
independent final remote digest, and cleared completed checkpoint.

Prediction: inspection recognizes the committed exact ID and acknowledges it
locally. It need not call reconciliation if inspection already returns Complete.
This is a lost-confirmation/restart arm after uncertainty has been persisted,
not process death inside the TCP request or a physical power-loss test. The
independent observation proves remote commitment even if a real transport error
occurs before the configured response discard; it does not itself prove HTTP
response arrival. No speed, repeatability or general release claim.

1200 seconds per process, with recorded command/PID/binary SHA and private
disk-backed TMPDIR/SQLITE_TMPDIR before start. No concurrent compilation or
measurement. Recovery starts only after exit 86 and the remote-registration proof
marker. No mount, regular daemon, installation, other-account mutation or deletion.
All failed attempts and local/cloud fixtures remain retained.

## Live result

Run `ece8507e-6347-4c0a-aff1-47d0052c3719` passed, 03:23:02–03:24:58 UTC,
115.199 seconds including setup. Binary SHA-256:
`fdd0575bea80faea35a80cad53c316b2488d0e657cd3a6c68f497e31a441698a`.
The [manifest](icloud-registration-recovery-ece8507e-6347-4c0a-aff1-47d0052c3719-live.json)
records first-process exit 86 with an independently hash-verified registered
67,108,881-byte file and local `verify_required`. Fresh recovery exited 0 after
one inspection, zero reconciliation calls (inspection already returned Complete),
and zero prohibited replay attempts. The same document ID and ETag reached
`uploaded`; exactly one remote file and its full independent digest matched.
The local export passed receipt/size/hash verification before inspection and the
completed transport checkpoint was cleared. No registration or upload was replayed
by the recovery worker.

Private source/export/journal evidence is retained under
`.local-state/icloud-account-registration-recovery-ece8507e-6347-4c0a-aff1-47d0052c3719/`;
process logs/manifests are under
`.local-state/icloud-registration-recovery-ece8507e-6347-4c0a-aff1-47d0052c3719/`.
Temporary storage was btrfs. No concurrent build or measurement ran, and no
ordinary service, mount or installation changed. Three synthetic guard tests
also pass, separating incomplete-body and completed-body checkpoint boundaries;
the actual adapter still validates every receipt field and exact request.

Complete `scripts/check.sh` passed, 03:26:03–03:32:17 UTC: formatting, clippy,
workspace/feature/kernel tests, scripts, translations, ledger and docs. Manifest
and log: `.local-state/icloud-registration-check-2026-10-01/`, private btrfs
TMPDIR/SQLITE_TMPDIR. The existing service rustdoc link warning remains. No GUI
changed in this step, so native-window scenarios were not rerun. No ordinary
service installation changed.
