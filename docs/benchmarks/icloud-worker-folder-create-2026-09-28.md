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
