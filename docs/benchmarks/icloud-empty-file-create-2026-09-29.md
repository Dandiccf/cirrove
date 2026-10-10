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

## September 30 diagnostic arm A3

Registered before invocation: `cea5902c-b02f-4e67-b18d-4469b87bc365`. Question: does a known-length zero-byte
content POST receive an HTTP refusal, an unexpected JSON shape, or a structured
receipt rejected by Cirrove's integrity requirements? Earlier attempts discarded
that distinction. Prediction: bounded response-shape observations identify the
failed stage; success is not assumed. The public rclone transport source uses
the same allocation/content-POST/registration sequence and does not establish
a separate successful empty-file protocol. No runtime dependency is introduced.

Use `--account-empty` with a new copied account/session and newly created owned
root. The normal create adapter permits size zero only through a developer-only
method bound to the exact validated fixture folder. The standard adapter remains
unchanged for user connections. The real TransferWorker persists its slot and
receipt through the account's sealed checkpoint vault before advancing.
One allocation and one content attempt; no retry of A, A2 or this arm. If Uploaded,
independently list the exact receipt ID and zero size; fresh-process read validation
is a separate step. On failure retain journal and checkpoints.

Diagnostics contain HTTP status and fixed boolean fields for the response shape
(singleFile object, zero size, nonempty receipt/checksum/reference/key), never
actual values, raw bodies or signed URLs. The body is explicitly known-length
empty. Allow 600 seconds; record binary hash, PID, disk-backed temporary directory
and command. No concurrent compile or cloud measurement.

A3 finished with exit 1 after 67.822 seconds. The content POST returned HTTP
200, a singleFile object, zero size, nonempty checksum, reference checksum and
wrapping key, but no nonempty receipt. Its durable journal remains VerifyRequired.
No registration was attempted by that arm after rejecting the content response.
This distinguishes the response-contract failure from an HTTP refusal; it does
not yet prove a valid registered zero-byte file.

### Fresh registration arm A4

Registered before invocation: `5be54f7f-b005-4d2e-b181-54e33e3acb96`. Parse missing/null/empty receipt as
absent, and permit absence only for size zero with nonempty checksum, reference
checksum and wrapping key. Nonzero content still requires a nonempty receipt.
When absent, omit receipt from registration data rather than inserting an empty
value. Persist the content response in the sealed checkpoint before registration
through the same worker. A3 is never replayed.

The regression test failed on the old missing-receipt parser (one test executed,
exit 101). The corrected test also rejects missing integrity fields and a missing
receipt for nonzero bytes. Prediction: A4 reaches Uploaded and exact zero-byte
metadata, or stops with retained state and a bounded registration HTTP status.
Use a fresh account/folder, no concurrent compile/measurement, disk-backed TMPDIR
and a 600-second manifest. Ordinary empty uploads remain disabled.

A4 (`5be54f7f-b005-4d2e-b181-54e33e3acb96`) finished with exit 0 in
74.134 seconds. Content and registration both returned HTTP 200. The same
zero-byte integrity shape was observed, the real worker reached Uploaded after
revision-bound verification, and an independent listing confirmed the exact
receipt ID with size zero. The earlier uncertain arms remain untouched.

### Fresh-process mounted read arm B

Registered before invocation: `--account-empty-read` loads only A4's recorded
account, owned root and confirmed receipt. It requires exactly one Uploaded row
matching that receipt, checks the revision-bound empty digest using a fresh read
session, then mounts only the owned subtree. A separate Python process checks
size zero and reads EOF. The mount is shut down even after application failure.
No application writes or retry of a failed upload. Prediction: all checks pass;
this validates a fresh-process read, not yet regular-router empty creation, empty
replacement or an installed-daemon release. Allow 300 seconds with the same
exclusive, disk-backed temporary-directory discipline.

Arm B finished with exit 0 in 2.008 seconds: fresh-process journal, independent
revision/digest checks and kernel-mounted size-zero/EOF read passed. Exact
findmnt lookup after shutdown returned no mount.

### Regular-router arms C/D

Registered before invocation: `09c98a4c-4506-42cd-90d6-1c23d118f456`. The normal create adapter now accepts
zero bytes; the temporary fixture-only allowance is removed. Its preflight test
was shown to fail before this change and passes afterwards. General iCloud
read-write account settings remain disabled.

The same `--account-empty` command now instantiates ICloudWriteProvider from the
Engine-owned WriteContext, observes the exact owned parent, and uses its normal
operation-bound envelope and sealed upload checkpoints. Arm C creates one new
empty file. Only if it succeeds, arm D invokes `--account-empty-read` in another
process on those recorded IDs. Prediction: regular routing retains the same
receipt and mounted read behavior. Record both child PIDs/results and one binary
hash, allow 600 seconds total, disk-backed TMPDIR, no concurrent compile/live run.
Previous arms are not replayed.

Arms C/D finished with exit 0 in 75.544 seconds combined. The regular account
router produced the exact Uploaded zero-byte receipt; the second process checked
the reopened journal, independent revision-bound empty digest and mounted EOF.
This closes the normal-adapter empty-create protocol gap for the recorded case.
It does not validate empty replacement, application truncate-to-zero, ordinary
installed-daemon operation or general write reliability. The full repository
gate must run on these changes before committing them.

The complete `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh`
gate then passed (exit 0): format, clippy, workspace/feature tests, kernel mounts,
scripts, ledger and documentation. The existing rustdoc link warning remains;
display-dependent window scenarios are not part of this command.
