# iCloud two-level folder move protocol, 2026-09-28

## Registered before live arm

Question: Does one conditional iCloud `moveItems` operation preserve the
exact identities and full bytes of a small two-level Cirrove-owned tree:
outer folder → inner folder → file?

One arm creates two new UUID-named root validation folders in the isolated
iCloud account. The source receives an outer `Cirrove Nested Move-*` folder,
an inner folder with the same UUID-name shape, and one generated
`created-by-cirrove.txt` file. The probe checks exact parent/child IDs,
document ID, current ETags and full file bytes. It syncs a private fixture
record with all IDs and the file's SHA-256 before sending one conditional
move of the outer folder to the empty destination. It then verifies both
folder IDs, file ID/document ID and full bytes. A separate read-only process
reloads the record and repeats the exact-ID and full-byte inspection. No
existing user item is addressed.

Endpoint: the whole tree appears only at the destination under unchanged
exact identities and full file bytes, or the provider rejects the move with
the whole tree intact at source; otherwise the outcome is indeterminate.
Prediction: the one-level move outcome will extend to this two-level tree.
One arm establishes protocol behavior only. It cannot establish arbitrary
tree depth, concurrent edits, reliable timeouts or mounted write support.
Normal iCloud accounts remain read-only.

The private run manifest must record command, binary SHA-256, PID, expected
duration, start/completion time and btrfs-backed private `TMPDIR` and
`SQLITE_TMPDIR`. No other measurement or compilation overlaps the live arm.

## Results

The one mutating arm ran 19:33:48–19:36:23 UTC and exited 0. Private run
`231c9dcd-b622-4ca0-b4bb-afab257a9145` records PID 3489453, the exact
binary SHA-256, command and btrfs-backed temporary directory. It synced
fixture record `8974c135-f1e3-4008-875d-6484fd2068e6` before sending
one conditional move. Immediate readback found both original folder IDs
and the original file/document IDs and full bytes at the destination.

A separate process (PID 3492069) reopened the saved Apple session and the
synced fixture record, then independently found the same exact two-level
tree and full file SHA-256 at the destination. Both folders had their
expected parent IDs. A stricter read-only binary with explicit outer-parent
checks repeated the inspection in a third process (PID 3505904) and also
exited 0. Its distinct SHA-256 and command are recorded in its private
manifest. No second move was sent.

This is one controlled protocol outcome for a two-level tree, not a
journal-backed two-level move. It leaves arbitrary depth/breadth, conflicts,
real transport timeouts, concurrent clients and mounted writes unverified.
The normal iCloud mount remains read-only. Elapsed time is context only.
