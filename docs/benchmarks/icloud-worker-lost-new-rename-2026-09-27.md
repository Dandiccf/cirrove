# iCloud shared-worker new-rename response loss — 2026-09-27

Registered before the live run. Both processes use only a new UUID-named
Cirrove folder and two small owned files in the isolated
`iCloudGuiValidation` account. The ordinary mount remains read-only.

Question: if the second rename installs the staged ID at the original name
but its received result is discarded, can a fresh process verify the two
exact IDs and full hashes, then commit the current and recovery bindings
without issuing either rename again?

Arm A: seal one private replacement, reserve the old recovery identity,
complete and verify the old rename, save the second-phase checkpoint, send
the new rename and discard its received result. Require `VerifyRequired`
with its saved checkpoint. Arm B: reopen the journal and vault in a fresh
process, reconstruct a read-only reconciliation adapter from that checkpoint,
and run the shared worker. The Arm B adapter refuses every commit call, so
it cannot send either rename. No comparative arm.

Prediction: Arm A remains uncertain; Arm B observes the exact staged ID at
the target and old ID at recovery, verifies both complete hashes, and commits
`Uploaded` with two distinct durable bindings. Missing, incomplete or
divergent evidence must not become success. One run per arm has no within-arm
spread and does not establish safety for in-flight requests, concurrent
edits or ordinary files. Apple's stale-ETag rename behavior still prevents
mounted writes.

Endpoint: process exit, durable state and exact current/recovery identities.
Separate private manifests record command, binary SHA-256, PID, expected
duration and disk-backed temporary storage before each process starts. No
credentials, signed URLs, raw provider bodies or file bytes are recorded
here. Both versions remain in iCloud Drive.

## Observed

Arm A discarded an already received second-rename result for operation
`2e3d1164-3016-4ecb-8467-2fa1c1d62ea3` and exited with `VerifyRequired`
and its saved second-phase checkpoint. Arm B loaded that checkpoint in a new
process. Its adapter refused both rename calls; it read and hashed the exact
staged ID at the original name and old ID at recovery, then committed
`Uploaded` with the two distinct journal bindings. No rename was replayed.
The private manifests record binary SHA-256
`c613eb4e2feaa86194c8d00b93a16b7a91722b4308b0871341042c1fe8e2a11e`
and btrfs temporary storage. This is one owned fixture and one run per arm;
it does not cover a request still in flight or concurrent remote changes.
