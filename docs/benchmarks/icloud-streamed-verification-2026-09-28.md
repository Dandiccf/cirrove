# Streamed iCloud verification, 2026-09-28

## Registered before live read

Question: Does the new single-download, bounded SHA-256 verifier return the
same complete digest as independent 4 MiB range reads of the existing 6 MiB
Cirrove-owned mounted replacement, while keeping its exact ID, ETag and size
stable across the read?

Arm A is the previously recorded read-only remount in
[the large replacement artifact](icloud-mounted-large-replace-2026-09-28.md).
It checked all 6 MiB by ranges but did not call the new streamed verifier.
Arm B opens the same completed fixture in a fresh process, reads it through
FUSE, then verifies the complete digest by both independent ranges and the
new stream. It must leave the existing journal at two `Uploaded` files and
one `Applied` folder, make no cloud mutation and detach its mount.

Prediction: The streaming hash matches the range hash and the expected
deterministic bytes. A full-content request could fail on this content host
despite smaller reads working; that would be a failed Arm B, not a reason to
weaken revision or digest checks. One Arm B cannot establish a performance
improvement or general provider reliability. The command, binary SHA-256,
PID, btrfs-backed private temporary directory and expected 360-second window
are recorded before the run in its private manifest.

## Results

Arm B passed. The fresh process read all 6 MiB through FUSE, then independently
obtained the same complete SHA-256 through 4 MiB ranges and through the new
single-download stream. The current exact ID, ETag and size remained stable
across the stream. The journal still had two `Uploaded` file rows and one
`Applied` folder row, and the mount was detached at exit. Private manifest:
`.local-state/icloud-stream-verify-9c5868c1-d579-4926-87da-b01ad5f6dbb8/manifest.json`.
It records binary SHA-256
`7f7dc7bb11c190f47847df2b5ba09e0a3d5ca6470f1625b8e68130c5dea1b333`,
PID 3057595, btrfs-backed temporary directories, start
`2026-09-28T14:29:10.066349+00:00`, completion
`2026-09-28T14:29:50.857144+00:00`, and exit 0.

This verifies the streamed read against one real iCloud file. It does not
measure the replacement-phase speedup: the old 8m20s arm and this read-only
remount execute different work. A new controlled replacement would be needed
for even a directional comparison, and repeated arms for a result.
