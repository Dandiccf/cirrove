# iCloud mounted replacement with the general Create stage — 2026-09-29

Registered before the live run. The scope is one new UUID-named,
Cirrove-owned folder in the isolated `iCloudGuiValidation` account. No
existing user item, ordinary mount, account setting or installed daemon is
changed. Normal iCloud connections remain read-only.

Question: can the feature-gated mounted replacement use normal-build
`ICloudFileCreate` for its staged bytes while retaining the shared worker's
two-ID recovery behavior? The previous replacement used
`ICloudOwnedFixtureUpload` for staging. The handoff itself remains an
isolated feature-gated adapter.

Arm A creates and fsyncs one small file through the isolated FUSE mount and
records its exact ID and full-byte digest. Arm B reopens that UUID fixture,
replaces only that file, and requires a durable `HandoffComplete` receipt:
the old exact ID must be in recoverable Trash, the new exact ID must be
visible under the original name, and both complete byte sequences must
match. Arm C starts a fresh process and read-only remount to confirm the
same IDs, bytes and journal receipts without issuing another cloud mutation.
Prediction: all three arms pass, with one original Create and one Replace
upload operation; no extra visible file or duplicate staged name remains.
Any uncertain outcome retains the journal and local bytes for inspection.

Endpoint is the independent remote list/read, complete Trash inventory,
mounted read and reopened journal. This is one functional trial, not proof
of atomic POSIX replacement, race safety or general account reliability.

Each exclusive arm records command, binary SHA-256, PID, expected duration
and disk-backed `TMPDIR`/`SQLITE_TMPDIR` in a private manifest before it
runs. No other measurement or compilation runs concurrently. Tokens,
signed URLs, cursor URLs, raw provider bodies and file bytes are not logged.

## Observation

All three arms exited zero on fixture
`bf81a29b-7292-4b6d-95cf-70e0f10c7ae9`. Arm A mounted, created and
fsynced the small file, then independently verified the exact remote
identity, bytes and journal receipt. Arm B used `ICloudFileCreate` to stage
the replacement through the shared worker; the validator confirmed the
two-ID handoff, current file and recoverable old item through independent
iCloud listing, complete byte reads and journal receipts. Arm C started a
fresh process, restored the fixture, read it through FUSE and independently
rechecked iCloud and journal state without another mutation. The test mount
was absent after each arm.

The private manifests are
`.local-state/icloud-general-stage-mounted-create-ff5aca91-bb33-41a8-aa72-87dd9a675893/manifest.json`,
`.local-state/icloud-general-stage-mounted-replace-ad62f26e-ab3d-4ddc-b7aa-daf8c9aaa439/manifest.json`
and
`.local-state/icloud-general-stage-mounted-resume-b6205260-85bd-45bd-a518-6b31a2b431ff/manifest.json`.
They record btrfs temporary storage, process IDs, zero exit codes and the
same binary SHA-256,
`4f8961bc026508a6b65808f3c7006ee36db85ce0be95b4fef61deacaed90adca`.
`scripts/check.sh` passed before the live arms.

This closes the staged-upload implementation difference for this owned
replacement fixture. The handoff remains feature-gated and non-atomic;
neither a concurrent edit nor a lost handoff response was exercised in this
run. Normal iCloud accounts remain read-only.
