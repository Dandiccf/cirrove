# Mounted iCloud rename after a file move — 2026-09-29

Question: can a file that has already been moved into a Cirrove-owned child
folder be renamed there through the FUSE mount, and can the confirmed identity
and bytes be reopened after a fresh process remounts? The previous fixture
guard required the file's original root parent, so the normal follow-up edit
was refused.

Prediction registered before the live run: walking the applied journal
upload and relocation receipts yields only the current exact file ID, parent
and ETag. A conditional `renameItems` within the owned child folder will keep
the same ID and full bytes; the old name will disappear from that folder.
The resulting journal receipt will survive a fresh remount. A stale original
version remains ineligible for another edit.

Arms and endpoints:

1. The synthetic journal test with an applied file move failed before the
   change with `Invalid` when resolving its current digest; the updated test
   must pass. A nested adapter guard must reject a foreign child folder and
   prohibit a reconciliation-only process from sending a second rename.
2. Reuse only the previously generated Cirrove UUID test folder
   `a01359b8-9644-4917-84f7-455228b2068b`. A separate Python process
   calls FUSE `os.rename` on its one small file inside `Mounted Folder`.
   Endpoint: applied exact-ID receipt, independent Apple listing with only
   `Nested Renamed.txt`, complete bytes and journal SHA-256.
3. A new process restores the private journal, remounts and opens the renamed
   nested file through FUSE. It repeats the independent identity and byte
   checks without issuing another mutation.

Record command, binary SHA-256, PID, expected duration and private btrfs-backed
`TMPDIR`/`SQLITE_TMPDIR` in a manifest before each arm; do not compile during
the arms. This is a functional check, not a performance measurement. No user
files or normal service state are touched. Ordinary iCloud connections remain
read-only; one fixture does not establish real-timeout, collision or
concurrent-client reliability.

## Results

The first mounted rename arm failed: the local FUSE rename returned, but the
journal's sixth mutation became `Failed` without a prepared item ID or remote
receipt. A read-only inspection showed the saved request, current journal
chain, adapter construction and Apple-side source preflight were valid. The
fixture dispatched a nested same-parent rename to its cross-parent move
adapter because it compared the destination only with the mount root. The
failed arm and its private manifest are retained.

Recovery follow-up registered after that failure, before retry: dispatch now
compares the source and destination parent IDs. The unsent failed operation
will enter read-only reconciliation; the exact original file still at its
source should yield `Uncommitted`, allowing the same journal operation to
prepare its ID and send one conditional rename. A changed or uncertain source
must not be resent. The endpoint remains an applied receipt, independent
listing and fresh-process FUSE read; a failed retry stays recorded.

The initial mounted arm (PID 3779636, binary SHA-256
`2867a0b5354b224e9bb32576893b407e2c2dbe0ca8a056f01f6db11a42532128`)
exited 1. Its sixth mutation remained `Failed`, with no prepared item ID or
remote receipt. The read-only diagnostic found that the fixture guard,
adapter construction and Apple-side preflight all passed when invoked as a
same-parent rename. The dispatch correction routed it to that adapter.

The recovery arm (PID 3787631, binary SHA-256
`bc332033e5ee1ddabb05b2c02d74066d63bf6ceb56cc547fa713a0c367edb9c5`)
first confirmed the old file version at the owned source, then asked the
shared journal to retry the same unsent mutation. Read-only reconciliation
returned `Uncommitted`; the worker prepared the exact file ID and reached
`Applied` with an upsert receipt. The journal retained the initial failed
attempt count of 1. Independent iCloud listing found the same ID and full
bytes only under `Nested Renamed.txt`. The fresh-process remount (PID
3788811, same binary) exited 0, opened that nested file through FUSE and
repeated the identity and byte checks without a new mutation. The private
manifests record btrfs-backed temporary storage; all failed and successful
logs remain with them.

This is one recovery from a known unsent preparation failure on a generated
file. It does not demonstrate recovery from a real uncertain network
response, collision, concurrent edit or arbitrary nested tree. Normal iCloud
connections remain read-only.
