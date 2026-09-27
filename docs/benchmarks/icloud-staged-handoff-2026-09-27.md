# iCloud staged identity handoff experiment — 2026-09-27

Registered before the live run. This is a feasibility experiment, not an
atomic-save or concurrency-safety gate. The known Apple web requests ignore
the stale ETags tested so far. Only two new Cirrove-owned files in a fresh
isolated validation folder are eligible for the operations below. Nothing is
deleted, and the normal iCloud mount remains read-only.

Question: Can the old file be moved to a unique recovery name and a separately
uploaded, verified new file be moved to the vacated original name, while both
exact provider IDs and byte strings remain independently readable?

Arm: create/read the original, create/read a uniquely named staged file,
list both IDs, rename the original to a unique recovery name, verify both
IDs and bytes, then rename the staged item to the original name and verify
both IDs and bytes again. If the first step is not fully verified, do not send
the second. Do not retry an uncertain request. No comparative arm.

Prediction: both renames will work when the destination name is free, and
both item IDs and bytes will survive. The operation is visibly non-atomic:
the original name is absent between the two requests, and a concurrent edit
cannot be guarded by the known ETag parameter. A positive result is evidence
for a recoverable *sequence* only; it cannot enable ordinary writeback without
durable checkpoints, identity handoff, conflict handling, fault injection,
and an explicit product decision about the missing atomicity.

Endpoint: both versions preserved under target/recovery names; recovery moved
but staging remained; or indeterminate. The fixture remains in iCloud Drive.
The ignored `.local-state/icloud-staged-handoff-validation/manifest.json`
records the binary hash, command, PID, disk-backed private temp filesystem,
expected duration and timing. No tokens, signed URLs, raw bodies or contents
are recorded.

## Observed first run

The pre-registered run completed in 87.8 seconds. Both files were created
under distinct provider IDs and read back byte for byte. After moving the
old file to a unique recovery name, the probe re-listed both IDs and read
both expected byte strings. It then moved the staged file to the vacated
original name and repeated those identity and byte checks. The final state
contains the new item under the original name and the old item under its
recovery name. No item was deleted. This is a single positive protocol
sequence, without a within-arm spread or interruption injection. It does
not prove atomicity, conflict safety under a competing writer, durable
restart, or suitability for the normal Cirrove write journal.
