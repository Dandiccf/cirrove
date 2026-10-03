# Mounted iCloud recovery after final installation — 2026-09-30

Registered before the live arms. Question: when Apple has installed the staged
replacement and its complete two-ID receipt has been verified, can a fresh
account-router process recover that save without any additional commit?

The previous mounted interruption stopped after Trash. This arm stops later:
the regular adapter has returned HandoffComplete, but the wrapper exits before
the worker can publish it to the journal or clear its sealed checkpoint. No
orderly session shutdown runs. This is lost local acknowledgement after a known
provider response, not an in-flight network failure or simulated power loss.

Use a fresh Cirrove-owned UUID folder and one small synthetic file. Preserve all
previous fixtures and the installed daemon. Arm A, `--account-mounted-final-interrupt UUID`,
creates/verifies the original and replaces it through FUSE, then fsyncs its
boundary marker and exits 86 immediately before returning the final receipt.
Detach only this arm's disconnected mount, retaining every local file.

Arm B, `--account-mounted-final-recover UUID`, requires the same operation in
VerifyRequired, pending bytes intact, a reserved recovery identity, and the saved
`install_inspected` checkpoint. A read-only preflight must verify that the new
exact ID is already installed and the old exact ID remains recoverable in Trash,
with both full content digests correct. All replacement commit calls are refused
and counted in this arm: inspection alone must recover the final receipt. A
second recovery invocation is refused rather than retrying an uncertain arm.

Prediction: the fresh worker reads the completed cloud state and atomically
publishes both journal identities without another upload, Trash or rename. The
same operation becomes Uploaded; mounted and remounted reads return the final
bytes. A separate read-only journal audit verifies current and hidden recovery
ownership. Any attempted commit or changed digest fails the arm with evidence
retained. Normal iCloud write access stays disabled.

Run sequentially, with no compilation during either live arm. Each private
manifest records command, binary SHA-256, PID, expected 1800-second deadline,
private disk-backed TMPDIR/SQLITE_TMPDIR and filesystem type before execution.
Stop only the recorded child on deadline. Publish only phases, counts and timing;
never checkpoint/session secrets, signed URLs, raw provider bodies or contents.
One sequence supports this particular functional boundary, not repeated
reliability, concurrent-editor safety, quota/session behavior or native package
write support. Those remain part of the full-integration objective.

Synthetic tests verify both process-exit points, refusal of every replacement
commit after final installation, and preservation of the earlier Trash-boundary
rules. Five parent tests passed; the ignored child is invoked explicitly by the
process-exit test for both boundaries.

## Pre-live validation

Full `scripts/check.sh` passed on 2026-09-30 at 18:16:02 UTC, including synthetic
kernel mounts, feature-gated probes, scripts and ledger. The feature-gated probe
build then passed. Private check manifest/log:
`.local-state/icloud-final-recovery-check-2026-09-30/`. The existing rustdoc link
warning remains unrelated to this change. No live outcome was claimed at that checkpoint.

## Observed: final acknowledgement recovered without another commit

Run `ae690a4f-13cd-4618-9bbb-2f7ef91364e1` used commit `b932a7b`, binary SHA-256
`bb22ff0fd96732395e7e5d9d49f915be956856dd406370b1e3f6f4c1be3af27d`, for both arms.
No compilation overlapped either live process.

Arm A created and independently verified the owned original, completed the normal
Trash and new-file installation sequence, verified the final receipt, and exited
86 before that receipt reached the journal. It took 223.968 seconds. Its marker
was present and its isolated mount was detached.
[Interruption result](icloud-mounted-final-recovery-interrupt-live-2026-09-30.json).

Arm B confirmed the retained local payload, reserved old identity and saved
`install_inspected` checkpoint. Independent remote inspection found the new exact
ID already installed and the old exact ID recoverable in Trash; both full digests
matched. The fresh worker recovered the receipt through inspection alone. It
completed the same operation with zero replacement commit attempts, and both the
mounted and remounted final reads passed. Exit 0 after 20.607 seconds; isolated
mount detached.
[Recovery result](icloud-mounted-final-recovery-recover-live-2026-09-30.json).

A separate read-only SQLite audit passed integrity checking and confirmed exactly
two uploaded records, the same replacement operation, one current remote owner and
one distinct hidden remote owner for the old ID in Trash, all in the isolated
account scope.
[Journal audit](icloud-mounted-final-recovery-journal-live-2026-09-30.json).
The audit script is preserved privately as `.local-state/audit-icloud-recovery.py`.

Private manifests/logs: `.local-state/icloud-final-recovery-<run>-interrupt/` and
`-recover/`; retained fixture: `.local-state/icloud-account-mounted-final-recovery-<run>/`.
No personal file, earlier fixture, installed service or default access mode changed.
The single-arm durations support no performance or repeatability claim. This closes
only the registered post-installation/pre-journal-acknowledgement gate. Competing
edits, recovery resolution, in-flight network uncertainty, quota/session failures,
native package writes and installed-release validation remain open.
