# Isolated iCloud mounted Create, 2026-09-28

## Registered before the live arm

Question: Can an ordinary process create one file and one folder through a real
Cirrove FUSE mount, with the shared upload and mutation workers committing both
to iCloud, while the normal iCloud account remains read-only?

Arm: one fresh UUID-named `Cirrove Write Validation-*` folder in the isolated
`iCloudGuiValidation` account. The feature-gated validator projects only that
folder as its mount root. A separate Python process creates a small text file,
fsyncs it and creates one empty subfolder through FUSE. The validator waits for
the upload and mutation journal receipts and uses an independent restored Apple
session to list both exact items and read all bytes of the file. The program
shuts down its mount; it leaves the new cloud fixture and private local journal
for diagnosis. No existing iCloud item is targeted.

Endpoint: both worker records reach `Uploaded`/`Applied`; independent listing
finds the exact receipt IDs under the test folder, and the exact file bytes,
size and SHA-256 match the sealed journal generation. Any timeout, uncertain
receipt or difference fails the arm. The ordinary daemon and account settings
must remain untouched.

Prediction: the FUSE file Create will reach the phased upload adapter. Folder
Create will reach the operation-ID mutation adapter. A failure may expose a
mount-to-journal mismatch that direct worker probes could not see. A single
passing arm is functional evidence, not reliability or memory evidence.

## Run result

One live arm passed. The executable reported a mounted file and folder Create,
followed by independent folder listing and full-byte file read. Reopening the
SQLite journal read-only found exactly one `uploaded` upload and one `applied`
mutation, each with an exact item ID under the same run-owned parent. The mount
was no longer present after the validator exited. The control manifest is in
the private, gitignored
`.local-state/icloud-mounted-write-control-e147e0c0-21ea-4b5f-a141-abd931a47d2f/manifest.json`:
PID 2722458, executable SHA-256
`397b7dc51c79663e018303b7505868060fdcb784d826138970cee4dffe53fdd3`,
expected window 360 seconds, `TMPDIR` and `SQLITE_TMPDIR` on btrfs. The fixture
and journal are retained in the private `icloud-mounted-write-6efdc284-*`
directory. Wall-clock duration was not recorded, so this arm supports no timing
claim.

Correction for the next arm: the first executable checked independent names,
bytes and both journal receipts, but did not compare the independent item IDs
to the two journal receipt IDs in code. Manual read-only reopening confirmed the
receipt IDs and common parent. The validator now compares both exact IDs and
parents itself; its changed version needs a separate live arm. This single arm
does not establish restart behavior, arbitrary nested writes or reliability.

## Second arm registered before execution

Question: Does the same mounted Create flow produce exact file and folder IDs
that an independent iCloud listing confirms, rather than merely matching names?
Use another new UUID-named test folder and the same separate FUSE-writing process.
The endpoint requires both journal receipt IDs and parents to match the
independent listing, in addition to full-byte and SHA-256 verification. Prediction:
both will match because the first arm's reopened journal showed distinct exact
IDs in one parent. This is a repeat functional arm, not a controlled timing or
failure-rate measurement.

The second live arm passed. The updated validator required the independently
listed file and folder IDs, plus each parent ID, to equal the uploaded and
applied journal receipts. It then read the complete 43-byte file through a
separate restored iCloud session and matched its SHA-256 to the sealed upload.
Independent read-only journal reopening found exactly one `uploaded` and one
`applied` row under the same owned parent; the isolated FUSE mount was gone at
exit. The private control manifest is
`.local-state/icloud-mounted-write-control-9c1dcc04-3c21-4eab-b1dd-6700b3e4f92e/manifest.json`:
PID 2726620, executable SHA-256
`3f0fd7a42b5064071858b97641092ff72fafa51d7f4b14a3bb4c14c3a9dc14d9`,
started `2026-09-28T10:12:52Z`, expected window 360 seconds, with both temp
directories on btrfs. The local fixture is retained in the private
`icloud-mounted-write-37fc6800-*` directory. No end timestamp or controlled
repeats were recorded, so this is still only functional evidence.

The mount adapter currently admits only file Create and folder Create directly
inside the fresh test root. Its in-memory allowlist is not reconstructed from
the durable journal after a process restart. Ordinary user iCloud mounts remain
read-only; replacement, deletion, nested writes, restart reconciliation and
external concurrent edits need mounted acceptance before enabling them.
