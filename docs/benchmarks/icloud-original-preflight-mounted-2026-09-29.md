# Mounted iCloud replacement with remote original preflight, 2026-09-29

## Registered before live execution

Question: can the two-ID replacement adapter capture the exact original
revision's SHA-256 from iCloud itself, persist it in its sealed upload
checkpoint, finish a mounted replacement, and reconstruct that checkpoint
after a fresh process? Previously it needed the old digest supplied by the
owned test journal; that dependency cannot work for pre-existing files in
a normal account.

Arm A creates a new UUID-named Cirrove-owned root and small source file
through the isolated mounted validator. Require its normal exact-ID and
byte verification. Arm B uses the validator's new existing-file constructor,
which receives the original node but **not** the prior digest. It must list
the exact parent and item, hash the saved ETag's complete original bytes,
checkpoint that hash before any stage allocation, then complete the staged
upload and conditional Trash handoff. Require a new exact ID and complete
replacement bytes, old ID recoverable in Trash, no visible stage/recovery
duplicate, and a durable handoff journal receipt. Arm C starts a fresh
process and rechecks the mount, remote identities, bytes and journal without
another mutation. The old item's bytes inside Trash are not read by this
validator.

Prediction: all arms pass, showing this constructor no longer relies on the
owned journal's digest. A synthetic checkpoint test already checks that the
digest survives adapter reconstruction and a mismatched known digest is
rejected. It does not prove the remote hash path, so this live arm is needed.
Any conflict or uncertain result stops the sequence; never retry the
mutation blindly. Each arm gets a private manifest with command, binary
SHA-256, PID, expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`.
Arm B has a 900-second validator bound; A and C use 360 seconds. No
compilation or second local measurement runs during an arm. Only the new
owned fixture may change; the installed service and normal iCloud mount
remain untouched and read-only. One passing sequence is feasibility
evidence, not reliability under concurrent edits.

## Observation

All three arms passed for fresh owned fixture
`852008c5-4aaf-4796-8ee7-6dc6a987853b`. Arm A independently confirmed
the exact source receipt and bytes. Arm B used
`from_sealed_session_for_existing`, so the replacement adapter received no
source digest from the fixture journal. Its version-checked preflight hashed
the exact original in iCloud before the first stage checkpoint. The mounted
replacement reached a durable two-ID receipt; independent listing and full
byte verification confirmed the new ID and content, the old ID in Trash,
and no visible staged or recovery duplicate. Arm C started a new process,
read the replacement through FUSE and rechecked identities, bytes and
journal without another mutation. All three test mounts shut down.

Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are under
`.local-state/icloud-original-preflight-ab4eb500-0231-4fe8-99f1-70c40812d45a/`.
They record binary SHA-256
`a5c446b495da7aa6b4309da7af74ca634ccfd4d63248c9c652cc1163475b62c3`,
PIDs, expected windows and btrfs-backed temporary directories. No local
compilation or second measurement overlapped the arms. The old bytes were
verified before replacement but not downloaded from Trash afterward.

This removes the adapter's *supplied-digest* dependency for one mounted
fixture. The fixture still used its owned journal to identify the source
node and operation. An ordinary account-wide router for pre-existing
iCloud files, concurrent-edit tests and repeated reliability runs remain
open; normal iCloud connections are still read-only.
