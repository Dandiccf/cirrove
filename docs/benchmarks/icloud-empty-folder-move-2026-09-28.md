# iCloud empty nested folder move, 2026-09-28

## Registered before live arm

Question: Does Apple's conditional `moveItems` preserve the exact identity of
an **empty folder** moved between two newly created Cirrove-owned parent
folders? File moves are covered separately; folder moves are still untested.

The one arm creates two UUID-named Cirrove validation folders at the account
root. It creates one empty `Cirrove Nested Move-*` child under the source,
checks its exact ID, name, ETag and emptiness, then synchronously saves source,
destination and nested IDs/names/ETag in private local state before sending
**one** `moveItems` request with the child's current ETag. It does not retry.
A separate process loads that record and lists both parents and the nested
folder by exact ID without mutation. No existing user item is addressed.

Endpoint: either the same empty folder ID appears only under the destination
with its name intact, or a rejected request leaves that same ID at the source.
Any duplicate, disappearance, changed name or nonempty child is indeterminate.
The second process must agree with the first; a response status alone is not
proof. This protocol arm does not use the shared mutation journal and does
not enable mounted folder moves.

Prediction: Apple will move the empty folder under its unchanged ID. One arm
is functional evidence only. A lost response, collision, populated folder and
external edit remain open. The normal iCloud connection remains read-only.

The private manifest records command, binary SHA-256, PID, expected duration,
start/completion time and btrfs-backed `TMPDIR`/`SQLITE_TMPDIR`.

## Results

The one mutating arm ran from 17:47:49 to 17:49:36 UTC and exited 0. It
synced private fixture record `1e57785a-fca9-46e8-ab40-cc70d02d7bff`
before its one conditional `moveItems` request. The immediate readback found
the same empty nested-folder ID only under the destination and the original
name unchanged.

A separate new process invoked `--inspect-empty-folder-move` with that run ID.
It restored the saved Apple session, matched both exact UUID-named parent
identities at root, and listed the source, destination and moved folder. It
also found the exact nested ID only in the destination, with the same name and
no children, then exited 0 without mutation.

This is one successful empty-folder protocol arm. The move was not yet routed
through the shared mutation journal; response loss, collision, stale ETag,
populated folders and recursive namespace preservation remain open. No normal
iCloud write capability is enabled. The elapsed time is context only, not a
timing estimate.
