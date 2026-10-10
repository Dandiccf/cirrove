# General iCloud empty-folder Trash adapter — 2026-09-29

Question: can a normal-build `ICloudFolderTrash` adapter remove one empty,
journal-confirmed Cirrove test folder through the shared FUSE mutation worker,
then verify that exact Apple folder ID in recoverable Trash? The earlier
mounted trial used a feature-gated fixture adapter.

Prediction registered before the live arm: `rmdir` on `Mounted Folder` in
owned fixture `de07f8c0-ae9f-485e-a969-4159dbda2016` will prepare its
exact folder ID and ETag, refuse any nonempty child listing, send one
conditional Trash request and record an applied removal only after a complete
Trash listing has that exact ID and a restore path. The neighbouring test
file's full bytes will remain unchanged. A fresh process will remount without
another removal.

Arms and endpoints:

1. Full local `scripts/check.sh` with the separate worktree target. Endpoint:
   format, Clippy, workspace and script tests, ledger and docs pass.
2. One live mounted `rmdir` of the existing, empty, journal-confirmed child
   folder. Endpoint: applied exact-ID receipt, independent Apple parent and
   Trash listings, neighbouring file unchanged.
3. Fresh-process `--resume-after-remove` of the same fixture. Endpoint:
   removed folder absent, exact ID recoverable, file readable and no new
   deletion.

Before each live arm, record exact command, binary SHA-256, PID, expected
duration and private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in its private
manifest. No compilation occurs during an arm. These are functional checks,
not performance evidence. No normal iCloud account is made writable.

## Results

The full local `scripts/check.sh` passed. A pre-existing rustdoc link warning
did not fail the docs check.

The mounted `rmdir` arm (PID 49212, binary SHA-256
`dd279e6201ff11de675a83118954e869c23f53ee787dcb0f058669f0a89740a6`)
exited 0. The isolated validator found the exact folder ID absent from its
parent and recoverable in complete Trash with a restore path, confirmed the
applied journal receipt and verified the neighbouring file's full bytes. Its
private manifest and log remain under
`.local-state/icloud-general-folder-trash-b1ddae0b-8766-40ea-ad82-8875bc641fcc/`.

The fresh-process `--resume-after-remove` arm (PID 52979, same binary)
exited 0. It remounted the same fixture, found that folder still absent and
recoverable, and reopened the unchanged file without another deletion. Its
manifest and log remain under
`.local-state/icloud-general-folder-trash-resume-92cc00ee-a82e-4a43-8fb6-ce315880ab44/`.
Both arms used private btrfs-backed temporary directories. No normal
Cirrove service was restarted.

This verifies one clean-response removal of an empty owned folder. A
deliberately lost response for the new adapter, concurrent changes, ambiguous
Trash listings and ordinary writable iCloud accounts remain unverified.
