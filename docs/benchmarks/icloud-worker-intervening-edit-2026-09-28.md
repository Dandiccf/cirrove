# iCloud worker concurrent edit before conditional Trash — 2026-09-28

Registered before the live run. Only one new UUID-named Cirrove folder and
two new small owned files in isolated `iCloudGuiValidation` may be changed.
The original is deliberately updated once through the validation session;
the worker then attempts to Trash that exact ID with its saved, now-stale
ETag. No existing user file, normal mount, other client state, or permanent
delete is touched.

Question: after the shared worker's final prepared observation, does an
intervening same-ID edit make Apple's conditional Trash refuse the stale
ETag, preserving the revised original and staged replacement? Does the
worker retain the local operation as a conflict rather than publishing the
replacement?

One arm: prepare two exact-ID/full-byte fixtures, reserve the original's
identity, enter the worker's commit phase, inject a same-ID edit after its
prepared observation, then send one stale-ETag Trash request. Read both
exact IDs/full bytes and the complete Trash listing; reopen the journal to
inspect the checkpoint, reserved owner, absence of a current receipt, and
conflict state. There is no comparative performance arm.

Prediction: Trash refuses the stale request. Both IDs remain in the folder
with the revised original and unchanged staged bytes. The worker reports
`Conflict`, keeps its local checkpoint and reservation, and does not publish
`Uploaded`. A timeout or uncertain Apple response requires read-only
inspection; the request will not be repeated with a fresh ETag.

Endpoint: verified remote exact IDs/full bytes and persisted local journal
state. A private manifest records command, binary SHA-256, PID, expected
duration and disk-backed TMPDIR/SQLITE_TMPDIR before the run. No token,
signed URL, raw provider body or file content is recorded. One run has no
within-arm spread or reliability/latency claim. Even success does not prove
protection against edits after a provider has accepted the request, other
clients, collisions, folder operations, Trash retention, or ordinary mounted
writes.

## Observed

One owned-fixture live run reached the injected same-ID edit after the
worker's prepared observation. Apple's stale-ETag Trash request was refused.
The validation session then confirmed that both exact IDs remained in the
folder: the old ID had the revised full bytes and a new ETag; the staged ID
retained its full bytes and saved ETag. A complete Trash listing did not
contain the old ID. The worker returned `Conflict` and did not send the staged
rename.

An independent read-only SQLite reopening found the matching operation in
`conflict` with no remote receipt, a saved checkpoint and its local payload.
The worker also checked that the old-ID recovery reservation remained. The
private manifest records binary SHA-256
`3a498a1ae1b2641ffbacfe2c9f333e03281d62b7748983236b42879930cea4a3`
and btrfs temporary storage. This is one run with no within-arm spread or
latency claim; it does not establish protection against concurrent changes
after Apple accepts a mutation, nor ordinary mounted-write reliability.
