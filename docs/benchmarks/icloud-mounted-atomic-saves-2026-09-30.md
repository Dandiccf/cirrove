# Editor-style iCloud atomic saves — 2026-09-30

Registered before the live arm. Question: does the normal iCloud account router
complete two successive editor-style temporary-file replacements through FUSE,
while keeping existing descriptors readable and preserving remote predecessors?

One fresh owned root and isolated writable mount, using the existing authorization
wrapper. The wrapper only confines identity to the fixture; normal journal,
write adapter, upload and mutation workers execute the operations. Existing
accounts, user files and the installed daemon remain untouched.

Sequence: create and independently verify an original; run an external Python
application which writes/fsyncs a temporary file and replaces the original twice,
without waiting for remote confirmation between saves. Keep the original and
first temporary descriptors open and verify their old bytes/link counts. Close
all descriptors, await five upload receipts and two recoverable cleanup receipts,
then independently verify final bytes and that only the final exact ID remains
in the owned cloud folder. All four superseded upload identities must be in Trash.
Unmount, reopen the same isolated account and verify final bytes and journal rows.
No permanent deletion or retry of a previous failed fixture.

Prediction: the local namespace switches immediately, old descriptors retain
old content, upload dependencies serialize each remote replacement and cleanup,
and reopening reads the final version. The existing synthetic kernel test
`real_replacement_preserves_old_descriptors_across_two_atomic_saves` passed in
the previous full check. This live arm tests the real iCloud router/transport,
not a new fix, and does not establish remote server atomicity, concurrent-editor
safety or reliability across repeated runs. Failures retain the journal and
cloud fixture; no uncertain operation is replayed to manufacture a pass.

One measurement at a time, no compilation. The private manifest records command,
binary hash, PID, expected 1800-second duration and disk-backed TMPDIR/SQLITE_TMPDIR.
The validator shuts down its mount even when a phase fails. Public output contains
only phase outcomes, counts and durations; identities and contents stay private.

## First live result: local success, remote blocked

Fixture `7a6fc63e-bf54-4feb-ab7d-7dcc69c5ff82`: original create/readback and
both local atomic saves passed, including old descriptors. Three creates were
confirmed. The first replacement repeatedly remained verify_required with zero
transferred bytes and no checkpoint; its successor and two cleanups stayed pending.
A read-only journal audit confirmed that its namespace owner was already unlinked
by the second local save, and it had a file_replacements record.

The shared identity-handoff reservation explicitly rejects both unlinked owners
and atomic namespace replacements. Its later acknowledgement also assumes that
the operation owner already owns the target's original remote ID, whereas an
editor replacement initially owns the temporary file ID. This is an integration
gap between two existing journal protocols, not an Apple response timeout.

The process was stopped by its manifest PID after 383.219 seconds (exit -15),
and its isolated FUSE mount was detached, verified back on btrfs. No old fixture
was retried and all local/cloud data remains. Public result:
[failed arm](icloud-mounted-atomic-saves-live-2026-09-30.json). Private evidence:
`.local-state/icloud-atomic-arm-7a6fc63e-bf54-4feb-ab7d-7dcc69c5ff82/`.

The correction must reserve the victim's old ID, transfer the newly created
provider ID to the source object, retain a recovery owner for the old target,
and give the temporary source ID to its existing cleanup object in one journal
transaction. A subsequent pending replacement must resolve from that new receipt.
Simply relaxing the existing ownership guards cannot establish these invariants.

## Registered correction and follow-up

A journal regression reproducing the two pending atomic saves failed at handoff
reservation with Stale before the correction. The updated transaction reserves
the replaced victim's identity, commits the new ID to the source, transfers the
temporary ID to cleanup, and records the old target under recovery. The regression
checks both successive receipts, restart after reservation, fencing of the former
attempt, rollback of an invalid receipt, cleanup targets and retained old bytes.
Existing ordinary replacement and handoff tests remain passing.

Rerun the same live sequence on a new owned fixture after this correction. Do not
resume or modify the first partial fixture. Prediction: the previously refused
first handoff completes and the second resolves its target from that new receipt.
A local pass alone is insufficient; all five uploads, two cleanups, four Trash
identities, final independent digest and remounted read must pass.

## Second live result: first handoff passed, successor source missing

Fixture `50dddff1-00f7-4a9d-ac0f-fdd21ec99d86` ended with exit 1 after 264.893
seconds and shut down its isolated mount. The first two-ID replacement completed
and its receipt was committed. The second replacement resolved its target to the
first receipt's new ID, but that ID had not yet appeared in the metadata index.
The account factory required the index and refused this successor with Conflict
before storing a checkpoint or sending its replacement. Four uploads were
confirmed; the fifth was conflicted and cleanup rows remained pending.

[Second result](icloud-mounted-atomic-source-gap-live-2026-09-30.json). Private
evidence: `.local-state/icloud-atomic-arm-50dddff1-00f7-4a9d-ac0f-fdd21ec99d86/`.
No retries or manual changes were made to this retained fixture.

## Registered third arm: confirmed predecessor source

The router must obtain a just-confirmed source from the operation's resolved
journal predecessor when it is not yet indexed. The predecessor must have an
earlier sequence, the same scope, a completed receipt and the exact requested
ID/ETag; parent ancestry and independent remote preflight remain mandatory.
An unrelated upload must not borrow that receipt. Test the missing-index case
synthetically before the correction, then repeat the original live sequence on
a fresh fixture. All original acceptance assertions remain required.

## Third live result: both handoffs passed, cleanup comparison refused

Fixture `c1d0f572-c8d2-488e-90ab-4bcdb20777eb` confirmed all five uploads,
including both handoffs. Both cleanup rows repeatedly remained Pending with no
prepared item. Their source IDs were present in observed metadata. Comparing
only field equality (without logging values) identified `content_version` as the
sole difference between each request's confirmed receipt and its directory node.
The normal listing leaves this internal field empty, while the upload receipt
provides one. The file mutation planner incorrectly compared the entire Node.

Stopped by recorded PID at 608.640 seconds (exit -15); isolated mount detached
and verified on btrfs. [Third result](icloud-mounted-atomic-cleanup-gap-live-2026-09-30.json).
Private evidence: `.local-state/icloud-atomic-arm-c1d0f572-c8d2-488e-90ab-4bcdb20777eb/`.
All fixture data remains; no cleanup was manually retried.

## Registered fourth arm: provider revision versus projection revision

For ordinary iCloud files, mutation source comparison must use provider identity,
ETag and the other metadata fields without mistaking the local content-version
annotation for a remote change. Keep package refusal and independent pre-mutation
digest verification. Prove the mismatch with a failing unit regression, then
rerun all original live assertions on another new fixture. This correction must
not relax ETag, name, size or package checks. Earlier partial fixtures remain
unchanged and do not count as completed recovery.

## Fourth live result: complete sequence passed

Fixture `b9592cbd-05a5-472a-b93e-861b2bb5a7e1` completed with exit 0 in
744.747 seconds. Both consecutive atomic saves passed the local descriptor
assertions; five uploads and two cleanup mutations were confirmed. Independent
checks verified the final digest and four superseded identities in Trash. A fresh
mount of the same isolated account read the final contents successfully. The
probe exited normally and its mount was detached (the path resolves to btrfs).

[Fourth result](icloud-mounted-atomic-saves-passed-live-2026-09-30.json).
Private evidence: `.local-state/icloud-atomic-arm-b9592cbd-05a5-472a-b93e-861b2bb5a7e1/`.
This is one passing controlled sequence, not repeated reliability evidence.
The earlier partial fixtures remain retained and their recovery is not resolved.

## Pause and resume boundary

Paused at the user's request after the fourth arm completed. The atomic-save
validator and three corrections remain uncommitted. Targeted regressions were
shown to fail before their respective corrections and pass afterwards. The full
`scripts/check.sh` has not yet run on these changes; run it before any commit.
No installed daemon was changed and normal iCloud writes remain disabled.

Resume with review of the dirty changes and the pending full check. Investigate
the separate intermittent writable-session CI failure from run 36667518761
(`real_a_directory_created_and_removed_again_is_gone_from_the_provider`); the
subsequent green run 36669600314 does not explain that failure. No additional
live arm is required merely to reach this pause boundary. All four fixtures and
private manifests must remain intact.

## Resumed validation

On resumption after the Strata sidequest, the dirty changes were reviewed and
`scripts/check.sh` passed in full, including the workspace, feature-gated iCloud
probes, kernel mounts, scripts, ledger, clippy, formatting and documentation.
The consecutive atomic-save kernel test passed. No further cloud arm was run;
the fourth live arm above remains the live evidence and its limitations stand.
Private check manifest/log: `.local-state/icloud-resume-check-short-2026-09-30/`;
started 17:03:45 UTC, finished 17:10:23 UTC, exit 0, btrfs-backed private temporary
storage. A preceding check stopped on an overly long temporary socket path, not
on the iCloud behavior; its failed log is retained separately.

The intermittent directory-removal investigation produced an independent
failing-before regression for cached folder creation receipts. Its correction and
validation are recorded in `folder-handoff-receipt-regression-2026-09-30.md`.
Normal iCloud writes remain disabled; all four original fixtures are retained.
