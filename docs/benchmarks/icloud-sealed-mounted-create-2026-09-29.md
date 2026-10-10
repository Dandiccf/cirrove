# Isolated iCloud mounted Create using the sealed session, 2026-09-29

## Registered before live execution

Question: can the isolated FUSE validator create an ordinary file and folder
through the same sealed, keyring-backed Apple session path used by the normal
account, then reopen them in a fresh process? Previously its write adapters
received an in-memory session snapshot. The normal account remains read-only.

Arm A: create a new UUID-named `Cirrove Write Validation-*` folder in the
isolated `iCloudGuiValidation` account. A separate process writes and fsyncs one
small file and creates one folder through FUSE. The validator waits for durable
upload and mutation receipts, then independently lists the exact remote IDs and
reads the file bytes. Keep the fixture and journal even on failure.

Arm B, only if A succeeds: run `--resume` with A's UUID in a fresh process. It
remounts the same fixture and reads the created file and folder; it does not
submit new writes. Confirm the exact item IDs and bytes again. A failure or an
uncertain operation stops the experiment; there is no blind retry.

Endpoint: each arm exits successfully, journal receipts are final and match the
independent remote IDs, file size and SHA-256 match, and the mount is shut down.
Prediction: both arms pass because sealed-session adapters already pass direct
probe coverage. A single live sequence is functional evidence only; it does
not prove reliability or authorize ordinary-account writes. Each arm uses a
private control manifest with the exact command, binary SHA-256, PID, expected
duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`. No other measurement or
compilation runs concurrently.

## Results

Both arms passed with one owned fixture, UUID
`d506e959-a92f-4d86-b6bd-d081db1225b1`. Arm A reported mounted file and
folder creation verified against independent iCloud listing and final journal
receipts. Arm B used a fresh process, reported the same independent verification
after remount, and exited successfully. The validator compares exact receipt
IDs and parents, plus the file's complete bytes and SHA-256. The private fixture
and journal remain at `.local-state/icloud-mounted-write-d506e959-a92f-4d86-b6bd-d081db1225b1`.

The control manifests are
`.local-state/icloud-sealed-mounted-control-2fc25d6c-e47e-4a5d-964b-522a2e96db65/arm-a.json`
and `arm-b.json`. They record binary SHA-256
`0511f5d5ded6b624f7e9b151c76dd4c3d75486af461fd02cab73d569023b942e`,
the respective commands and PIDs, a 360-second expected duration, and private
disk-backed temporary directories on btrfs. These are functional runs, not a
controlled timing or reliability measurement. The ordinary account remains
read-only; the live evidence covers only owned fixture creation and restart
readback through this sealed-session path.
