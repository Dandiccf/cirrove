# iCloud owned folder creation and lost-response recovery — 2026-09-28

Registered before the live run. This is one controlled functional trial in the
isolated `iCloudGuiValidation` account. It creates a new UUID-named Cirrove
validation folder at the root and one nested `Nested Cirrove folder` inside it.
No existing user item, ordinary iCloud mount, or other cloud client is changed.

Question: can the shared mutation worker publish Apple's allocated nested-folder
ID after a lost response without guessing identity from a matching name or
replaying `createFolders`? Apple's create response supplies the allocated ID;
the adapter saves a request-bound exact-ID receipt in the local desktop keyring
before returning success to the worker. If the response is lost before that
save, the operation must remain `NeedsReview`, not create again.

Arm A deliberately discards the adapter's response **after** the keyring saves
the allocated ID. Prediction: the first process leaves one journal mutation at
`VerifyRequired`. A fresh process restores the exact parent fixture and runs
only reconciliation. It must find the saved ID in a complete parent listing,
confirm name and parent, and change the journal to `Applied` without another
cloud mutation. The endpoint is an independently reopened SQLite mutation
receipt plus the exact-ID parent listing. A matching name alone is not proof.

This is one injection after an accepted provider response. It does not prove a
real network timeout before Apple responds, arbitrary process-crash safety,
cross-account reliability, folder deletion, or general mounted writes. The
ordinary iCloud service and GUI remain read-only. A private manifest records
each process's command, PID, binary SHA-256, expected duration and disk-backed
TMPDIR/SQLITE_TMPDIR before it starts. Compilation and this live run do not
overlap. No credential, signed URL, raw provider body or cursor is logged.

## Observed

Both isolated processes exited zero. The first saved the allocated folder ID
in the local keyring, deliberately discarded its success response, and left
operation `5cfd6d16-3b24-4485-8f44-967c4bc80e8f` at `VerifyRequired`.
The second process constructed a reconciliation-only adapter from the saved
account and parent fixture. It confirmed the exact child ID in a complete
parent listing and reached `Applied` without a second `createFolders` call.

An independent, read-only SQLite query after both processes exited found one
mutation with `upsert` receipt for
`FOLDER::com.apple.CloudDocs::F10A7DF2-4515-47E0-84C9-3EC3433E7B7B`, parent
`FOLDER::com.apple.CloudDocs::7C22768D-16C9-4C53-BF87-AE63E3A502D4` and
name `Nested Cirrove folder`. Its state was `applied`. Both private manifests
record binary SHA-256
`0da7e418dc8754c6bb3d31dfae235566f223da5285f81a13d9018460dc7c7802`
and btrfs-backed temporary storage. These are one injected lost-response arm,
not proof of all crash timings or mounted folder creation.

## Operation-aware worker arm, registered before the run

The shared mutation worker now supplies its durable operation ID to the
provider. The owned-folder adapter derives its keyring checkpoint from that
ID at call time, rather than requiring an adapter instance constructed for a
single operation. This permits one adapter to serve successive folder-create
requests in the same owned test parent without mixing their identities.

Question: does a newly started worker still create and publish one exact
folder identity through the new operation-aware call path? One fresh
UUID-named Cirrove-owned validation folder in `iCloudGuiValidation` may receive
one new `Nested Cirrove folder`. Prediction: the journal reaches `Applied`
with the same scope, parent and name, and an independent SQLite reopening
finds one exact remote ID. The binary SHA-256, process ID, command, expected
duration and btrfs-backed TMPDIR/SQLITE_TMPDIR are recorded privately before
the run. No compilation overlaps this single functional arm. It does not
establish mounted writes or repeatability across accounts.

Observed: the isolated process exited zero. Worker operation
`6433a231-5926-4979-b01d-cb330da553e6` reached `Applied`. An independent
read-only SQLite reopening found one `upsert` receipt for
`FOLDER::com.apple.CloudDocs::B00CE01C-AF3A-4D0E-B6FB-C1B1193183EB` under
parent `FOLDER::com.apple.CloudDocs::F8D857B6-8394-4BD3-9CE8-23BF69AAA0C3`
with name `Nested Cirrove folder`. The private manifest records binary
SHA-256 `e52698b11513700e3d862d0d3a4ca31a3578e1c565b021d5eca37101a4c98aa3`
and btrfs-backed temporary storage. A synthetic unit test separately confirms
that one adapter instance keeps two operation IDs and their distinct allocated
folder IDs in separate checkpoints. This single live operation does not prove
arbitrary concurrent folder creates or mounted writes.
