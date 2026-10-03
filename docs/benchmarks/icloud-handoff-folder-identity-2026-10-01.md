# Exact-folder iCloud handoff validation

Registered before live execution, 2026-10-01.

Question: can a new non-root replacement inspect its captured folder directly,
without repeating a complete parent inventory, while retaining replacement and
interrupted-upload recovery guarantees?

New handoff plan version 5 binds the folder ID, actual parent ID, display name and
folder kind. One complete exact-folder envelope supplies both that observation
and the children used by existing exact-file/revision/name-conflict checks.
Unrelated sibling folders with the same display name are permitted: mutations
address captured IDs, never resolve the containing folder by name. This is an
explicit new contract, not proof of sibling-name uniqueness. Persisted versions
2 and 3 retain their parent inventory and sibling-name uniqueness rule; root
version 4 is unchanged. Unknown versions and version 5 at root are refused.

Synthetic acceptance: the single-request test was run against the legacy helper
and failed because it requested root instead of the captured folder. After the
change, tests require exactly one target-folder request, reject changed parent,
name or kind, incomplete and ambiguous envelopes, and retain the legacy duplicate
sibling rejection. Existing target-file collision and revision tests remain.

Live arm: rerun the already documented replacement-body interruption protocol
with a fresh Cirrove-owned folder and files, a 64 MiB + 17-byte varied replacement,
and an 8 MiB body-yield boundary. Exit 86 and recover in a new process. Require
preserved old remote bytes and new local bytes, read-only reconciliation, local
export, a fresh successful upload, exact new remote digest, exactly one current
file, and the original ID in recoverable Trash. The provider validates old Trash
bytes; there is no additional independent final Trash-body oracle. No personal
files, permanent deletion, mount or installation changes.

Prediction: the same functional endpoints pass using version 5, without parent
inventory calls from handoff inspection. Setup and other adapter operations may
still list root. This is a functional acceptance arm, not a controlled latency
comparison; previous phase timings cannot support a percentage speedup.

Use the existing replacement-stream supervisor, record binary SHA, PIDs, commands,
disk-backed private TMPDIR/SQLITE_TMPDIR and deadlines before start, 1200 seconds
per process. No concurrent build/measurement. Retain failed arms and all journals.

## Live result

Run `ca19de1f-5b3f-4737-acad-5772c113845e` passed, 2026-10-01
02:23:56–02:28:23 UTC, 267.009 seconds including setup. Binary SHA-256:
`00e3532583a74fbb56836b4d03690b512fcb31a5a6aca3b422bef63ae4f60133`.
The [manifest](icloud-replace-stream-recovery-ca19de1f-5b3f-4737-acad-5772c113845e-live.json)
records exit 86 at exactly 8,388,608 yielded body bytes and fresh recovery exit 0.
The new process independently verified the original remained unchanged before
retry, all 67,108,881 local replacement bytes, and the local export. One inspection
and one reconciliation established the unfinished stage was uncommitted; zero
prohibited replay attempts occurred. A fresh document identity completed, the
independent full remote digest matched, exactly one final file remained visible,
and the original exact ID was in recoverable Trash. The provider verified old
Trash content; the independent original-content check preceded the retry.

This confirms the controlled replacement-body recovery endpoint with the new
folder contract. It is not an installed/FUSE check or a controlled performance
comparison. Both process logs and all source/export/journal evidence remain under
`.local-state/icloud-replace-stream-recovery-ca19de1f-5b3f-4737-acad-5772c113845e/`
and `.local-state/icloud-account-replace-stream-recovery-ca19de1f-5b3f-4737-acad-5772c113845e/`.
Private temporary storage was btrfs. No build or measurement ran concurrently;
no regular service or installation changed.

Complete `scripts/check.sh` passed, 2026-10-01 02:33:39–02:40:10 UTC:
formatting, clippy, workspace and feature tests, synthetic kernel mounts, scripts,
translations, ledger and docs. The checkpoint reconstruction test additionally
confirms both saved v3 and v5 plans retain their version. Check manifest/log:
`.local-state/icloud-handoff-folder-check-2026-10-01/`, private btrfs temporary
storage. Rustdoc still reports the unrelated existing `Writeback::retry_stuck`
intra-doc link warning in the service crate. No GUI changed; window scenarios
were not rerun. This branch's validator changes are not installed.
