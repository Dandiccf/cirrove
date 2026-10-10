# Isolated iCloud folder Trash using the sealed session, 2026-09-29

## Registered before live execution

Question: can the mounted general folder Trash adapter use the encrypted,
keyring-backed session to remove a Cirrove-created empty child folder, with
an exact applied receipt and fresh-process recovery?

Arm A creates a new UUID-named validation root in the isolated
`iCloudGuiValidation` account. A separate FUSE application creates and fsyncs
one small file plus one empty sibling folder; independent iCloud listing and
the upload/mutation journals must confirm both exact IDs and file bytes.
Arm B runs `--remove-folder` for that fixture, removing only the empty child;
the file must retain its exact ID and bytes, while the folder's exact ID is
recoverable in Apple Trash. Arm C runs `--remove-file` for the same fixture,
then arm D `--resume-after-file-remove` in a fresh process, confirming both
owned children remain absent with applied exact-ID receipts and no further
cloud mutation. Each arm runs only after its predecessor passes. An uncertain
or failed arm stops the sequence without blind retry.

Prediction: the folder and file Trash arms pass through the sealed-session
general adapters; the fresh process reconstructs the two removals from the
journal. The endpoints are process exit, exact remote/journal IDs, full file
bytes before deletion, recoverable Trash, absence after remount and mount
shutdown. One sequence is functional evidence, not timeout or concurrent-edit
reliability. Before each exclusive arm, a private manifest records command,
binary SHA-256, PID, expected duration and private btrfs-backed
`TMPDIR`/`SQLITE_TMPDIR`. No normal mount, account setting, installed daemon
or other cloud item is changed; no concurrent compilation or measurement runs.

## Results

All four arms exited zero on new owned fixture
`9ec42d89-27c2-4d30-bb94-0768cd6dc915`. Arm A created the file and empty
folder through FUSE; an independent iCloud listing and journal receipts
matched both exact IDs and the file's complete bytes. Arm B removed only the
empty folder, confirmed its exact ID recoverable in Trash and retained the
file. Arm C removed the file with its own applied exact-ID Trash receipt.
Arm D remounted in a fresh process and independently verified both children
absent without sending another mutation. The test mount shut down after each
arm.

The private manifests `arm-a.json` through `arm-d.json` are retained in
`.local-state/icloud-sealed-folder-control-8429e32c-ed30-4ad3-8da4-960ebbaceb01`.
They record binary SHA-256
`1f21804d98286be71cc109c1deb76c881846ce86527cc1b7a2193ccc9a2d3d4d`,
the respective commands and PIDs, 360-second expected windows and private
btrfs-backed temporary directories. This establishes one functional mounted
folder and file Trash sequence with sealed-session adapters. It does not
establish normal-account reliability, live timeout recovery, concurrent edits
or permanent deletion. Normal iCloud mounts remain read-only.
