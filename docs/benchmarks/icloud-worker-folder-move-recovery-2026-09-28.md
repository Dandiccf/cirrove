# iCloud empty-folder move through the mutation worker, 2026-09-28

## Registered before live arm

Question: Can Cirrove's shared mutation journal prepare the exact ID of a new
empty iCloud folder, tolerate a deliberately discarded `moveItems` response,
and reconcile the requested destination and name in a fresh process without
another move?

One arm creates two new UUID-named Cirrove validation parents and one empty
`Cirrove Nested Move-*` child under the source in the isolated account. The
feature-gated adapter accepts only that account, collection, parent pair,
nested exact ID/name/current ETag and a still-empty child. The runner syncs a
private folder-identity record, then the shared mutation journal persists the
prepared child ID before **one** conditional `moveItems` request. The adapter
deliberately discards the response and the first process must leave
`VerifyRequired`. A second process reopens the same journal, restores a
reconciliation-only adapter, checks root parents and complete source,
destination and child listings, and may mark `Applied` only if the same empty
folder ID appears exclusively at the requested destination under its original
name. It cannot send a second move. No existing user item is addressed.

Endpoint: one exact-ID `Upsert` receipt accepted by the original `Relocate`
request after restart, or an explicit unresolved/conflict state. A matching
name with a different ID and an automatic collision rename are not success.
The synthetic guards verify that an unprepared or restarted adapter cannot
reach the mutation path.

Prediction: the empty-folder protocol outcome from the preceding one-shot
trial will recur, and read-only reconciliation will produce `Applied`. This
simulates lost receipt after the provider call; it is not a real network
timeout, populated subtree or reliability proof. The ordinary iCloud mount
remains read-only.

The private manifest records command, binary SHA-256, PID, expected duration,
start/completion time and btrfs-backed private `TMPDIR`/`SQLITE_TMPDIR`.

## Results

The one mutating arm ran from 18:22:04 to 18:23:51 UTC and exited 0. Its
private journal/fixture run `5c9ae5f0-8bbf-463f-b2f2-c2e0ed03a90c` saved
the exact nested-folder ID before the one conditional move request. The
runner deliberately discarded the provider response; the shared mutation
worker left operation `5c8b62b0-c209-45e9-9e17-45aea92f6e3d` in
`VerifyRequired` with that prepared identity.

A separate new process reopened the saved Apple session, the synced folder
record and the same mutation journal. Its reconciliation-only adapter found
both exact parent identities at root, an empty source, the same nested folder
ID and name only at the destination, and no child items. It then marked the
operation `Applied` with an `Upsert` receipt accepted by the original
`Relocate` request. The second process exited 0; its adapter could not send
another move. The targeted synthetic guards and full local checks are reported
separately.

This is one controlled lost-receipt simulation for an empty folder. It does
not establish populated-subtree preservation, real transport-timeout recovery,
collision handling, simultaneous clients or repeatability across accounts.
The ordinary iCloud mount remains read-only. The elapsed time is context only.
