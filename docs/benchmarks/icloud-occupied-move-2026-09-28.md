# iCloud occupied-name move, 2026-09-28

## Registered before live arm

Question: What does Apple's `moveItems` do when a different exact file ID
already occupies the moving file's name in the destination folder? The
[stale-revision arms](icloud-stale-move-2026-09-28.md) found a conditional
ETag check, but did not test destination collisions.

The one arm creates two new UUID-named `Cirrove Write Validation-*` folders
in the isolated validation account. Each gets a separate small file named
`created-by-cirrove.txt`, with different complete bytes and distinct Apple
document/Drive IDs. The probe independently confirms both listings, IDs,
ETags and bytes, then sends **one** `moveItems` for the source's current ETag
and the occupied destination ID. It never retries or overwrites an existing
user item; every item here was created by this arm.

Possible endpoints: rejection leaves both exact IDs and full bytes intact in
their original folders; acceptance may produce two same-name destination
items with both IDs intact; displacement of the destination ID is recorded
separately; any other state is indeterminate. A move being rejected after
this preflight alone cannot prove race-safe collision handling. The mounted
Relocate path remains disabled regardless of one arm's outcome.

Prediction: `moveItems` may reject the occupied name, but iCloud has admitted
duplicate displayed names in other contexts. The probe must verify exact IDs
and full bytes rather than infer behavior from a response status. One run is
functional evidence only, with no reliability or timing claim.

The private run manifest records command, binary SHA-256, PID, expected
duration, start/completion times and btrfs-backed private `TMPDIR` and
`SQLITE_TMPDIR`. It contains no token, signed URL or provider body.

## Results

The one live arm ran from 15:27:27 to 15:30:20 UTC with exit 1. Its
immediate classifier returned `Indeterminate`: it expected the moved file to
retain its displayed name, and had not persisted the fixture IDs before the
request. This is a probe defect, not evidence that the provider lost a file.
No mutation was replayed.

A subsequent read-only inventory of the 55 Cirrove-owned validation folders
located the unique destination fixture by its distinct content prefix. Its
listing contains two exact file IDs. The original occupant remains at
`created-by-cirrove.txt`, size 71, SHA-256
`5967fccff7e45964359fc095221705b12deb99e479fc0d70fce4611c8f113b2f`.
The additional file is `created-by-cirrove 2.txt`, size 62, SHA-256
`16e273f475e303b241e3793256250c7cca10e8b21f2b204b32618ecbc54be32f`.
Both complete byte streams were fetched by exact ID on a separate read-only
process. Its content matches the source fixture's own validation prefix; the
destination had exactly one file before the move. Apple therefore renamed the
incoming file on this collision instead of replacing the occupant. The original
source folder ID was not durably recorded, so this arm cannot independently
prove that folder is now empty. The move request's per-item acceptance status
was likewise not recorded, so the finding is about observed remote state, not
the response's success signal.

The probe now recognizes a two-ID, byte-preserving renamed outcome. A mounted
POSIX move into an occupied name must not silently treat Apple's new name as
the requested result. The normal iCloud mount remains read-only. A later arm
must persist source/destination IDs and preflight hashes before sending a
request, then test reconciliation if the response is lost. This single arm is
functional evidence only, not a reliability or timing claim.
