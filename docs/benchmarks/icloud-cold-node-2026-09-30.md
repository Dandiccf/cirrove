# Cold iCloud file resolution — 2026-09-30

Registered before the live arm. Question: can a fresh normal read provider resolve
an owned file by its exact ID without any cached folder/index, while refusing
its two recoverable Trash predecessors?

Use the existing passed fixture `21d05f56-70c4-47a8-a5e3-577ab68994ac` and the
read-only `--account-cold-node` validator. It checks the saved owned root and
receipts, restores the isolated session, constructs a fresh normal ICloudDrive,
and compares the resolved file against its saved receipt. No writer, mount,
cloud mutation or regular service change. Do not print identities or contents.

Prediction: exact metadata supplies a fully qualified parent ID; a complete
parent listing confirms identity, name, size and revision. Both Trash IDs must
return NotFound. An unqualified or absent parent is an unresolved contract, not
permission to infer an arbitrary path or report success. One fixture is not
broad provider reliability evidence. Folder and generated-artifact cold lookups
remain separate contracts.

The private arm manifest records binary hash, command, PID, disk temporary
paths and terminal result. No compilation or other measurement during execution.

Negative control: the new active-file regression ran one test against the old
node implementation and failed with Unavailable. Four focused tests now pass,
including changed revision, invalid identity/metadata, Trash, scope and cancellation.
An initial incorrectly qualified filter ran zero tests and was discarded; the
negative log contains the corrected fully qualified test invocation.

## Live result

Passed in 3.662 seconds, using btrfs temporary storage. The fresh normal provider
resolved the active file without an index and matched the owned receipt's ID,
parent, name, size and revision. Both exact predecessor IDs returned NotFound,
so recoverable Trash entries cannot reappear through this path. No cloud write
or installed-daemon change occurred. This is one account/fixture, not a latency
benchmark or a proof of all formats, permissions or concurrent operations.

[Public result](icloud-cold-node-live-2026-09-30.json). Private manifest and log:
`.local-state/icloud-cold-node-arm-6373d59c-8d1e-4db8-b34f-70f8b6a60cb7/`.

Normal cold FILE lookup now uses exact metadata followed by the complete parent
listing and the same document-package projection as ordinary directory reads.
Missing/unqualified parent identity, inconsistent revisions and incomplete
metadata remain errors. Folder IDs and generated export IDs still require their
existing parent/projection context; this change does not guess their ancestry.

Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed on
this implementation: formatting, clippy, workspace and feature tests, kernel
mount scenarios, script tests, ledger and documentation. The existing rustdoc
warning about `Writeback::retry_stuck` remains. Display-dependent window tests
were not run. Private log: `.local-state/icloud-cold-node-full-check.log`.
