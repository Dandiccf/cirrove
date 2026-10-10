# iCloud owned move through the mutation worker, 2026-09-28

## Registered before live arm

Question: Can the shared mutation journal prepare an exact iCloud file identity,
survive a deliberately discarded `moveItems` response, and reconcile that
operation in a new process without sending a second move?

The one arm creates two new UUID-named Cirrove validation folders in the
isolated account. One contains a new small file; the destination is empty. A
feature-gated `MutationProvider` accepts only this exact account, collection,
source file ID, ETag, full SHA-256 and destination folder ID/name. `prepare`
verifies both folders and complete source bytes, then the shared journal saves
the prepared ID before the one conditional `moveItems` request. The runner
deliberately discards the response after the request and requires the journal
to enter `VerifyRequired`. A separate process constructs a reconciliation-only
adapter from the journal and the synced private folder-identity record. It
lists both folders, verifies the full moved bytes and exact ID, and may mark
`Applied` only when the requested name and destination match. It cannot send
a move. No existing user item is addressed.

Endpoint: one mutating request and one applied exact-ID receipt after restart,
or an explicit uncertain/conflict state. A destination auto-rename must not be
acknowledged as the requested POSIX move. A synthetic guard test confirms that
the restarted adapter rejects the mutation path before network access.

Prediction: with an empty destination, Apple will move the same file ID under
the requested name; the second process will recover the receipt. This is one
functional arm, not a real network timeout or a reliability/timing claim. The
ordinary iCloud connection remains read-only regardless of the result.

The private run manifest records command, binary SHA-256, PID, expected
duration, start/completion time and btrfs-backed `TMPDIR`/`SQLITE_TMPDIR`.

## Results

The one mutating arm ran from 17:02:22 to 17:04:16 UTC and exited 0. The
shared journal saved run `8863adac-6df4-4cdc-b319-b7aa7e9a2337` before the
request, including the prepared exact file ID. After the provider request, the
runner deliberately discarded the response and confirmed `VerifyRequired`.

A separate new process used `--worker-reconcile-move` with that run ID. It
reopened the same journal and synced folder-identity record, constructed a
reconciliation-only adapter, listed both exact folders and checked the full
source-file SHA-256 under the requested name in the destination. The shared
mutation worker marked operation
`22fcc6c3-1227-4d11-a112-f615bee6c5d0` `Applied` with an
`Upsert` receipt accepted by the original `Relocate` request. The second
process exited 0 and had no call path that can send a move. The synthetic
reconciliation-only guard test also passed.

This validates one lost-receipt *simulation* with an empty destination. It
does not cover a real transport timeout, collision race, folder move,
concurrent external edit or long-session repeatability. There is no basis yet
to enable the ordinary iCloud mount for writes. The elapsed time is recorded
for context, not used as a timing estimate.
