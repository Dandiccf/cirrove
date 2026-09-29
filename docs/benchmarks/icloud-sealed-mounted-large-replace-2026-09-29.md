# Isolated iCloud mounted large replacement with sealed checkpoints, 2026-09-29

## Registered before live execution

Question: does the sealed-session mounted upload path complete a streamed
5 MiB Create and 6 MiB two-ID Replace on a new Cirrove-owned iCloud fixture,
then retain the result after a fresh-process remount? The earlier small-file
sequence passed once; that does not prove larger streamed transfers. The
transport sends one HTTP request per file, streaming it in 64 KiB buffers;
it does not implement resumable network chunks.

Arm A runs `--large-create` in a fresh UUID-named test root under the
isolated `iCloudGuiValidation` account. A separate mounted application writes
80 deterministic 64 KiB chunks and fsyncs. Require exact journal and
independent iCloud listing receipts, complete streamed and ranged SHA-256,
and a confirmed empty sibling folder. Arm B runs `--large-replace` for that
same owned fixture. It writes 96 different deterministic 64 KiB chunks and
fsyncs. Require a durable two-ID handoff, exact old ID in recoverable Trash,
new ID at the original name, complete streamed and ranged SHA-256, and no
visible staged item. Arm C runs `--large-resume-after-replace` in a new
process; require the mounted read and independent remote/journal checks to
pass without another cloud mutation. No claim is made about bytes inside
Trash because this probe does not read them there.

Prediction: the sealed checkpoint vault permits all three arms to pass while
larger content moves through the bounded stream. This does not imply the
checkpoint itself grows with content size. Each arm uses the already-built binary, a private manifest
with command, binary SHA-256, PID and expected duration, and private
disk-backed `TMPDIR`/`SQLITE_TMPDIR`. There is no concurrent local
measurement or compilation. Arm B has a 900-second bound; A and C have
360-second bounds. One passing sequence would establish feasibility on this
fixture, not general timeout or concurrent-editor reliability. Any uncertain
outcome stops the sequence without retrying the mutation. The normal account
and installed daemon remain untouched and read-only.

## Observation

All three arms passed on fresh owned fixture
`0a75ef4b-3880-4880-b9b3-bf89a54a7ef3`. Arm A's mounted fsync and
independent iCloud ranged/streamed SHA-256 checks confirmed all 5 MiB and
the exact Create receipt. Arm B's mounted fsync, independent listing and
ranged/streamed SHA-256 confirmed all 6 MiB under a new ID; the original
exact ID was in recoverable Trash and no visible staged or recovery file
remained in the test folder. Arm C mounted in a fresh process, read the
replacement through FUSE and rechecked the journal, exact identities and
remote bytes without another mutation. All test mounts shut down. The
private manifests are `arm-a.json`, `arm-b.json` and `arm-c.json` under
`.local-state/icloud-sealed-large-replace-bf099188-3ec3-4631-a469-b6937ca42bad/`;
each records binary SHA-256
`220a8bbe41fc7a94fdd917d7c9fcc9effbb721cd4a73e29d5a2193e487840064`,
its PID, the actual expected window and btrfs-backed temporary directories.
No compilation or second measurement overlapped the arms.

This is one successful larger-file sequence, not a reliability result.
The old file's bytes were independently verified before replacement, but
were not downloaded from Trash afterward. Concurrent edits, network failure,
session expiry, quota errors and truly large files remain untested here.
