# General iCloud folder-rename adapter — 2026-09-29

Question: can the normal-build `ICloudFolderRename` adapter rename one
journal-confirmed Cirrove test folder through the shared FUSE mutation worker,
preserve Apple's exact folder ID and avoid repeating a mutation after a
fresh-process remount? The earlier mounted trial used a feature-gated adapter.

Prediction registered before the live arm: a separate application's FUSE
rename of `Mounted Folder` to `Mounted Renamed` in the owned fixture
`9dc46ad6-dd13-4509-b878-744c1c3913c9` will bind the original exact folder
ID and ETag in the prepared journal row. An independent Apple listing and a
fresh-process FUSE remount will find that ID only under the new name. The
neighbouring test file will retain its full bytes.

Arms and endpoints:

1. Full local `scripts/check.sh` with the separate worktree target. Endpoint:
   format, Clippy, workspace and script tests, ledger and docs pass.
2. One live mounted rename of the existing, journal-confirmed Cirrove child
   folder. Endpoint: applied exact-ID journal receipt, independent Apple
   listing, unchanged neighbouring file bytes.
3. Fresh-process `--resume-after-rename` of the same fixture. Endpoint:
   renamed folder and file readable without a new mutation.

Before each live arm, record exact command, binary SHA-256, PID, expected
duration and private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in its private
manifest. No compilation occurs during an arm. These are functional checks,
not performance evidence. This one empty folder does not establish behavior
for populated folders, collisions, simultaneous edits, real timeouts or normal
writable iCloud accounts.

## Results

The full local `scripts/check.sh` passed. A pre-existing rustdoc link warning
did not fail the docs check.

The mounted rename arm (PID 4184959, binary SHA-256
`fd07cde8e396f8cfcc0fc87def1271ca9bbff16bb9c63deb8265529bbb894838`)
exited 0. The isolated FUSE validator renamed the journal-confirmed child
folder and independently checked Apple's exact folder ID, the applied receipt
and the neighbouring file. Its private manifest and log remain under
`.local-state/icloud-general-folder-rename-7441f5fa-3cb7-4e0f-99df-38d8309eac71/`.

The fresh-process `--resume-after-rename` arm (PID 4185810, same binary)
exited 0. It remounted the fixture, found the same folder only at the new
name and verified the neighbouring file without issuing another rename. Its
manifest and log remain under
`.local-state/icloud-general-folder-rename-resume-20fad420-6edf-48e0-992f-92e42cdec336/`.
Both arms used private btrfs-backed temporary directories. No normal
Cirrove service was restarted.

This verifies one empty-folder rename and fresh-process remount. A populated
folder, real lost HTTP response, concurrent metadata edit, collision and
ordinary writable account remain unverified.
