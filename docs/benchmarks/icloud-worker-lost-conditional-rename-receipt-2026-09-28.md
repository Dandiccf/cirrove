# Lost staged-rename receipt after conditional iCloud Trash — 2026-09-28

Registered before the live run. Only one new UUID-named Cirrove folder and
two newly uploaded small owned files in the isolated `iCloudGuiValidation`
account may be changed. No existing user file or normal mount is touched.
The former test ID may remain recoverable in Trash; no permanent delete or
restore is attempted.

Question: if Apple commits the staged rename after the conditional old-ID
Trash step but its rename response is deliberately discarded, can a fresh
process verify both exact IDs and full bytes and publish the existing
handoff without sending either mutation again?

Two sequential arms on the same fixture. The first process reserves both
identities, performs the old Trash step, sends at most one staged rename and
discards its response. It must stop `VerifyRequired` with a saved checkpoint;
the fault is considered reached only when the adapter reports that it
actually discarded the rename response. Earlier uncertain phases are not
mislabelled as that fault. The second process loads the same journal and
checkpoint in reconciliation-only mode. It may read exact IDs and bytes but
cannot call either `moveItemsToTrash` or `renameItems`; success requires
`Uploaded` with the old ID in Trash and the staged ID current. If the
remote handoff is not complete, it remains uncertain. These arms test
recovery, not comparative performance.

Prediction: the first process retains `VerifyRequired`; the second observes
the completed remote handoff and publishes exactly one current and one
hidden recovery identity. One run has no within-arm spread or latency claim
and does not establish concurrent-edit safety, collisions, Trash retention,
folder operations or ordinary mounted writes.

Endpoint: exact-ID/full-byte remote receipt and reopened SQLite bindings.
Private manifests record each command, binary SHA-256, PID, expected
duration and disk-backed TMPDIR/SQLITE_TMPDIR. No token, signed URL, raw
provider body or file content is recorded.

## Observed

The first process reached the deliberately discarded staged-rename
response and stopped `VerifyRequired` with its checkpoint and old-ID
reservation. A separate journal read confirmed that it had not fabricated
a current receipt. The second process loaded that same journal in
reconciliation-only mode. It verified both exact IDs/full bytes and reached
`Uploaded` without issuing another Trash or rename request. Reopening
SQLite independently found exactly one current staged-ID binding and one
hidden, owned former-ID binding under the opaque iCloud Trash parent.

Both private manifests record binary SHA-256
`8635288aab0575f25cc5eaa19c0c368ff151580a1bca6f8b6661f33c5712eaea`
and btrfs temporary storage. This is one owned-fixture response-loss run;
it has no within-arm spread or latency claim. An in-flight timeout before
Apple's rename response, concurrent external edits, collisions, Trash
retention and ordinary mounted writes remain unverified.
