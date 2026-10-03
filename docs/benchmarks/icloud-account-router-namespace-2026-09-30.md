# iCloud account-router namespace acceptance

Preregistered before running this protocol on 2026-09-30.

Question: do the normal account-wide folder and file mutation adapters work with
Engine ownership, sealed per-operation plans and `MutationWorker`, beyond the
older fixture-specific adapter path?

The developer binary's `--account-namespace RUN_UUID` mode creates a fresh
isolated local account and a new Cirrove-owned remote fixture root. Existing
state directories cannot be reused. It does not mount a filesystem or alter the
installed service. Every mutation is guarded by an exact run-owned receipt and
an owned destination; even the fixture root itself cannot be removed.

## Sequence and prediction

- Create Folder A and Folder B through the real router.
- Create known file bytes through its transfer worker.
- Rename the file, then move it into Folder A without renaming.
- Independently hash the moved revision; put that file in recoverable Trash and
  independently check its exact ID there.
- Refresh Folder A metadata (its children changed), rename it, move it into B,
  and remove the now-empty A. Refresh and remove the now-empty B.
- Reopen Engine, context, router and journal for each operation. Compare every
  resulting receipt with an independent parent listing. Never infer a receipt
  from a matching name alone. Retain the fixture root and all private evidence.

Prediction: nine namespace operations and one file create will pass; changed
folder revisions or durable plan routing may expose an integration gap. No
combined move+rename, arbitrary user data, recursive removal, permanent delete,
or application save behavior is exercised by this arm. Those remain separate
requirements. An error ends the run without automatic replay.

Permit 20 minutes. Run one arm at a time, without compiling. Before launch,
record command, binary hash, revision/dirty state, PID, run ID, expected duration,
and disk-backed TMPDIR/SQLITE_TMPDIR plus `findmnt` result in a private manifest.
Use the same manifest discipline as the account-router upload arms. This is
functional acceptance, not a performance comparison. If a first arm passes,
repeat on new fixture IDs before making any repeatability claim.

## Results

Arm A `9dec4957-94db-41a6-bea8-2881562e7bdc` exited 0 after **515.146 seconds**.
Private evidence: `.local-state/icloud-account-namespace-arm-9dec4957-94db-41a6-bea8-2881562e7bdc/`.
The manifest records its PID, binary hash and btrfs temporary filesystem.
All nine operations passed through the regular account router and MutationWorker:
create A/B, rename file, move file, recoverable file removal, rename A, move A
into B, remove empty A, remove empty B. The file create and independent digest
after moving also passed. Each receipt matched an independent listing; moved IDs
were absent from their old parent. The exact removed file ID was found in Trash.
The remote fixture root was empty at the end, and a freshly opened journal
retained all nine Applied rows with receipts. The fixture root and local evidence
remain retained. The process has exited.

This is one functional acceptance arm. There is no within-arm spread yet and no
repeatability or mounted-filesystem claim. Combined move+rename, populated-folder
removal and the other exclusions above remain untested by this protocol.

Synthetic source/destination/root protection tests passed. Removing the explicit
root-removal guard made the root-removal rejection test fail (exit 101); the guard
was restored before building the live binary.
