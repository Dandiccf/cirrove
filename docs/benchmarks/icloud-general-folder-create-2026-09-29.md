# General iCloud folder-create adapter — 2026-09-29

Question: can the normal-build `ICloudFolderCreate` adapter, instead of the
feature-gated owned folder creator, make one child directory through Cirrove's
shared FUSE mutation journal in a fresh Cirrove-owned iCloud test folder?
Will an independent Apple listing and fresh-process remount preserve the exact
returned folder ID?

Prediction registered before the live arm: one separate application `mkdir`
will reach an applied mutation receipt bound to Apple's returned folder ID.
The independent session and fresh-process remount will find that same ID and
name. A lost response before the ID is persisted must remain indeterminate;
a name match alone is insufficient proof and must not trigger an automatic
repeat create.

Arms and endpoints:

1. Full local `scripts/check.sh` using this worktree's separate Cargo target.
   Endpoint: format, Clippy, workspace tests, script tests and ledger pass.
2. One fresh mounted live create in a newly generated Cirrove-owned UUID
   folder. Endpoint: successful FUSE `mkdir`, applied exact-ID journal receipt,
   independent Apple listing of the returned child ID. The same fixture also
   creates one small file through the already validated general Create adapter.
3. One fresh-process `--resume` of that UUID. Endpoint: same child ID and file
   bytes remain visible, without another create operation.

Before each live arm, record exact command, binary SHA-256, PID, expected
duration and private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in its private
manifest. No compilation occurs during an arm. These are functional checks,
not performance evidence. Ordinary iCloud connections remain read-only.

## Results

The full local `scripts/check.sh` passed. The pre-existing rustdoc link
warning did not fail the docs check.

The fresh mounted arm (PID 4143048, binary SHA-256
`a146684ea38c04be199a16e14eaf1a78e4205006b4de7a8e1cff8a4bc2415959`)
exited 0. In fixture `9dc46ad6-dd13-4509-b878-744c1c3913c9`, the separate
application created and fsynced a small file, then created `Mounted Folder`.
The validator independently confirmed both remote identities and journal
receipts. Its private manifest and log are retained under
`.local-state/icloud-general-folder-create-e2c62d4d-7fea-4d89-95c5-0c0b48bd5e78/`.

The fresh-process `--resume` arm (PID 4145526, same binary SHA-256) exited 0.
It remounted the same fixture and independently verified the child folder ID
and complete file bytes without another create. Its manifest and log are
retained under
`.local-state/icloud-general-folder-resume-5ae19c70-3acf-4918-b4b2-2e76c5842c02/`.
Both arms used private btrfs-backed temporary directories. No normal
Cirrove service was restarted.

This verifies one clean-response child-folder creation and remount. It does
not test a lost folder-create response, which remains indeterminate without
Apple's allocated ID, nor general folder depth, collisions, concurrency or
ordinary writable accounts.
