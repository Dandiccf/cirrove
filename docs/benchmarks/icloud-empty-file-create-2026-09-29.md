# Empty iCloud file creation in an owned folder — 2026-09-29

Registered before the live run. The experiment creates one new UUID-named
Cirrove validation folder and one empty text file in the isolated
`iCloudGuiValidation` account. It does not touch the normal iCloud mount or
another client's state.

Question: can the normal-build `ICloudFileCreate` adapter and shared transfer
worker upload a zero-byte file with the empty SHA-256 digest, keep an exact
parent/item/ETag receipt, and verify that file in a fresh process? The old
adapter refused size zero before an Apple request. A focused test was run
against that old guard and failed `Invalid`; it passed after removing only
the local size-zero refusal. This test alone says nothing about Apple's API;
the refusal was restored after the live arms below.

Arms: (A) create a new owned folder, enqueue an empty file, run the shared
worker once; (B) open the retained journal in a fresh process and compare
its completed receipt against an independent Apple folder listing and the
empty digest. Arm B only runs after A has a durable `Uploaded` receipt.
If A is uncertain, no blind mutation retry is permitted. Prediction: both
arms exit zero, with one exact new file ID and size zero; if Apple rejects
the zero-byte content upload, A must retain a non-uploaded journal state.

Endpoints: arm exit status, durable journal state, exact account, parent,
item ID, name, ETag, size and SHA-256, and independent listing after process
restart. The run uses a disk-backed private `TMPDIR` and `SQLITE_TMPDIR`, with
a manifest recording command, binary SHA-256, PID and expected duration before
each exclusive arm. No credential, signed URL, cursor or raw response is
logged. One fixture is functional evidence only, not a reliability estimate
or permission to enable normal-account writes.

## Observation

Arm A, run `3eebfadc-13a4-4e0a-9622-1a3ccde135bf`, exited 1. The
shared worker persisted `VerifyRequired`, one failed attempt and zero
transferred bytes. A read-only fresh-process inspection found an allocated
document ID in the private checkpoint but no content receipt and no item
under the exact name in the owned folder. This is a failed arm; the allocated
slot remains unregistered and the journal is retained. No retry was sent.

Correction registered before a new arm: the original upload used a streamed
HTTP body even for zero bytes, leaving its wire length unspecified. The
transport then sent a known-length empty body only for size zero. Arm A2
creates a **new** owned folder and a new journal, then sends one upload
request through the same normal-build adapter and worker. It does not resume
or reuse A's uncertain slot. Prediction: A2 reaches `Uploaded`, followed by
the original fresh-process verification arm B against A2's run ID. If A2
does not complete, the zero-byte path stays disabled.

Arm A2, run `3413b754-edf9-40be-a3b5-89c2e4b25fbd`, also exited 1 with
`VerifyRequired`. Its read-only fresh-process inspection likewise found the
allocated document ID in the private checkpoint, no content receipt, and no
item under the exact name in its new owned folder. The journal and unregistered
slot remain retained; A2 was not retried. The read-only arm B was not run,
because neither arm had an `Uploaded` receipt. The local allowance and
known-length-empty-body experiment were reverted, keeping zero-byte iCloud
uploads disabled in the normal adapter.

These observations narrow the failure to the content-upload/receipt phase,
before file registration. They do not establish whether Apple rejected the
zero-byte POST, returned an incomplete receipt, or failed transiently: the
adapter intentionally does not log raw provider bodies or signed URLs. The
[rclone iCloud Drive source](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/iclouddrive.go#L3577-L3589)
also special-cases empty-file reads, but does not prove Cirrove's upload result.
Normal iCloud writes remain disabled. A future zero-byte implementation needs
a separately validated protocol path and fresh-process receipt, not a retry of
these uncertain operations.

The exclusive-run manifests and btrfs temporary directories are retained at
`.local-state/icloud-empty-create-20260929/`; each UUID journal is retained at
`.local-state/icloud-empty-create-<run>/`. Neither is checked into Git.
