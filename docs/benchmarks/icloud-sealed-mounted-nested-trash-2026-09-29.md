# Isolated iCloud nested file Trash using the sealed session, 2026-09-29

## Registered before live execution

Question: after the isolated mounted mutation adapters switch from an
in-memory snapshot to the encrypted account session, can a file created in
an owned child folder be unlinked through FUSE, verified in recoverable iCloud
Trash, then remain absent after a fresh-process remount?

The fixture is `d506e959-a92f-4d86-b6bd-d081db1225b1`. Its one generated
file has already been moved into its generated child folder, with exact
journal receipts and independent remote verification. The validator requires
four sequential arms on that same owned tree:

1. `--rename-nested-file`: rename the existing moved file inside the child
   folder, retaining its exact ID and bytes.
2. `--create-nested-file`: create and fsync a second small file in that child
   folder with a final upload receipt and full-byte readback.
3. `--remove-nested-file`: unlink only that second generated file. Require an
   applied exact-ID Trash receipt, complete source listing without it, and
   the unchanged first file.
4. `--resume-after-nested-remove`: in a fresh process, read the remaining
   file, verify the removed file stays absent and check the journal without
   submitting another mutation.

Prediction: all four arms pass with the general sealed-session file rename,
create and Trash adapters. Any failure or uncertain result stops the sequence
without repeating a possibly accepted mutation. No normal mount, installed
daemon, account setting or other cloud item is changed. The endpoint is the
process exit, exact journal/remote IDs, complete bytes, recoverable Trash and
mount shutdown. One sequence is functional evidence only, not proof of
concurrent-edit or timeout reliability. Before each exclusive arm, a private
manifest records command, binary SHA-256, PID, expected duration and private
btrfs-backed `TMPDIR`/`SQLITE_TMPDIR`; nothing else compiles or measures
concurrently.

## Results

All four arms exited zero on the same fixture. The nested rename retained the
original exact file ID and bytes. The new nested file reached a final upload
receipt and independent full-byte verification. The mounted unlink reached an
applied recoverable Trash receipt for that new file's exact ID, while the
original remained readable. A fresh-process remount found the new file absent
without another mutation. Each arm shut down its test mount. The private
manifests are `arm-a.json` through `arm-d.json` in
`.local-state/icloud-sealed-nested-control-a2370eca-bb5c-406b-9300-6310558b6518`;
they record binary SHA-256
`1f21804d98286be71cc109c1deb76c881846ce86527cc1b7a2193ccc9a2d3d4d`,
the exact commands and PIDs, 360-second expected windows and btrfs-backed
temporary directories. This is one functional nested-file sequence, not a
reliability or timing claim. Normal iCloud mounts remain read-only.
