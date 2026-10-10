# iCloud owned-file mutation adapter trial — 2026-09-27

Registered before the live run. This is a feature-gated adapter trial with
the isolated `iCloudGuiValidation` account, not an enabled writable mount.

Question: can Cirrove's `MutationProvider` contract move one freshly created,
small Cirrove-owned file to iCloud Trash using its exact remote ID and observed
ETag, then independently reconcile that same ID in a complete Trash listing?

Arm A: create one fresh `Cirrove Write Validation-<UUID>` folder and one
`created-by-cirrove.txt` file; verify complete bytes. Build `RemoveFile` with
that exact account, collection, parent, ID and ETag. `prepare_mutation` checks
the original ID, metadata and full SHA-256 without changing remote state.
`mutate_prepared` sends one `moveItemsToTrash` request, then requires absence
from the exact parent and the same ID with a restore path in a complete Trash
listing. `reconcile_prepared_mutation` repeats the read-only check. There is
no comparative arm. The test item remains recoverable in Trash.

Prediction: the mutation returns an exact `Removed` receipt and reconciliation
returns `Applied(Removed)`; any unclear response remains uncertain and does
not authorize replay. This one arm gives no within-arm spread or latency
claim. It does not test response loss, concurrent remote edits, non-owned
files, large files, folder deletion, permanent deletion or restore.

Endpoint: exact adapter receipt and independent Trash reconciliation. The
private manifest records command, binary SHA-256, PID, expected duration and
disk-backed temporary storage before execution. No credentials, signed URLs,
raw provider bodies or file bytes are recorded in this artifact.

## Observed Arm A

The native `MutationProvider` returned the exact `Removed` receipt, and a
separate read-only `reconcile_prepared_mutation` returned `Applied(Removed)`
for the same file ID. The adapter required the test folder still at the exact
root ID, the original ETag and full content SHA-256 before the request; after
the request it required absence from that parent and the same ID with a
restore path in a complete Trash listing. The file remains in recoverable
Trash. The private manifest records binary SHA-256
`88d158dbe4f1bc9412ac5dfa1ac12197f592ec4b1d818376ee924a9e0261a8a2`
and btrfs temporary storage. The process remained active for more than three
minutes, much longer than an interactive file-manager deletion budget; this
single run does not isolate whether root listing, Apple mutation or Trash
listing dominated. It is one account and one arm without a within-arm spread.
