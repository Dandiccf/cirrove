# iCloud owned move collision after preflight, 2026-09-28

## Registered before live arm

Question: If a new file occupies an iCloud move destination after the shared
worker's final empty-folder check but before `moveItems`, does Cirrove refrain
from acknowledging the requested `Relocate` when Apple renames the incoming
file?

The one arm creates a fresh Cirrove-owned UUID source folder with one small
file and a fresh empty UUID destination in the isolated validation account.
The mutation journal saves the exact source identity and ETag. A test-only
gate pauses the feature-gated worker **after** its final preflight and
**before** the one move request. A separate session creates a different
Cirrove-owned small file under the destination name, confirms its exact ID
and full bytes, then releases the worker. A fresh read-only session checks
both exact IDs and complete bytes. No existing user item is touched.

Endpoints: if Apple rejects the move, the source file stays byte-identical
and the occupant remains in the destination; if Apple accepts and auto-renames,
both exact IDs and full bytes must remain, but the moved file has a different
name. In either case the original requested `Relocate` must have **no**
accepted receipt or `Applied` journal state. Any disappearance, overwrite or
accepted wrong-name receipt is a failure. A nonzero or indeterminate run stays
recorded as such and is not automatically retried.

Prediction: Apple will repeat its observed automatic suffix rename. The
worker should mark Conflict because the remote name differs from the request,
while preserving both files. One arm is functional evidence only; it does not
prove all collision races or general provider reliability. The normal iCloud
mount remains read-only.

The private manifest records command, binary SHA-256, PID, expected duration,
start/completion time and btrfs-backed private `TMPDIR`/`SQLITE_TMPDIR`.

## Results

The one arm ran from 17:25:16 to 17:27:49 UTC and exited 0. Journal run
`ddadda96-eb2e-4fed-a433-866b910faf36` prepared the exact source ID before
the worker's final preflight. The gate paused the worker after that preflight;
a separate session then registered and byte-verified the new destination
occupant. After the gate was released, exactly one `moveItems` request ran.

An independent read-only session found the original occupant under the
requested `created-by-cirrove.txt` name and the moving exact ID under an
automatically changed name. Both complete byte streams matched their own
pre-request values. The shared journal finished in `Conflict` with its
prepared source identity still present and **no** accepted `Upsert` receipt.
It did not mark the requested `Relocate` `Applied`.

This is one controlled race at the deliberately exposed pre-request point.
It confirms that this feature-gated adapter detects Apple's collision rename
and keeps both files, but does not prove every concurrent-client scenario or
repair the remote rename automatically. The ordinary iCloud mount remains
read-only. The elapsed time is context only, not a timing estimate.
