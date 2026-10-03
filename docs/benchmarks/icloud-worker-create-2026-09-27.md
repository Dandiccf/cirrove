# iCloud upload-worker owned-file trial — 2026-09-27

Registered before the live run. The normal iCloud mount remains read-only.

Question: Can Cirrove's regular durable upload worker create a small native
iCloud file in a fresh Cirrove-owned validation folder, save a bounded upload
checkpoint before the remote mutation, and publish a verified completion receipt?

Arm A: with the isolated `iCloudGuiValidation` account, create one fresh
`Cirrove Write Validation-<UUID>` folder, enqueue one small
`staged-by-cirrove-<UUID>.txt` file into a private upload journal, and run one
worker attempt. Inspect the final journal state and remote item identity.
There is no comparative arm. A second, response-loss/restart arm requires a
separate deliberate fault-injection run and is not inferred from Arm A.

Prediction: the worker will finish `Uploaded` and the provider will confirm
the exact folder, unique item ID, ETag, size and full file hash by readback.
An uncertain response will remain `VerifyRequired` without blind retry.
This run cannot establish general write reliability or enable the GUI mount.

Endpoint: one worker attempt and its journal receipt; measured elapsed time is
descriptive only. With one run, there is no within-arm spread and no latency
claim. The fixture remains in iCloud Drive. The command, binary SHA-256, PID,
private disk-backed `TMPDIR` and expected duration are recorded in a separate
private manifest before the process starts. No credentials, signed URLs,
raw provider bodies or file contents belong in the artifact.

## Observed Arm A

The worker reached `Uploaded` for operation
`dc2dd5ad-1880-49e5-9a57-37efe8ac3616` in a private journal under
`.local-state/icloud-worker-create-a3753e74-db2e-442c-b78e-b452946edae4/`.
The adapter checked the exact parent, unique remote ID, ETag, size, and full
SHA-256 readback before returning its receipt; the worker then committed that
receipt to the journal. The private run manifest records binary SHA-256
`f434e2894bdf754de2621fc12fd6e979f722e1f4bc58a7104cf03a9d10f84c6b`
and a btrfs-backed temporary directory. No file outside the new test folder
was changed. This is one successful run, without a within-arm spread. A lost
response and a restart were not exercised by this arm.
