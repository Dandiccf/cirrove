# General iCloud file-create adapter — 2026-09-29

Question: can the normal-build `ICloudFileCreate` adapter, rather than the
feature-gated owned uploader, create a small file through the shared FUSE
upload journal in a fresh Cirrove-owned iCloud folder, then verify its exact
Apple document identity and full bytes? A fresh process should be able to
recover the confirmed file from the private journal and open it again.

Prediction registered before the live arm: the isolated FUSE probe will
create one new UUID-named validation folder, upload `Mounted Create.txt`
through the general adapter's saved content slot and receipt, and report an
applied journal entry. Its independent Apple readback will match all bytes.
A separate `--resume` run should see the same file and bytes. A mismatch in
parent identity, registration or checkpoint handling should fail the arm.

Arms and endpoints:

1. Full local `scripts/check.sh` with the separate worktree target. Endpoint:
   format, clippy, workspace tests, script tests, ledger and docs pass. This
   is a code gate, not evidence of Apple behavior.
2. One fresh mounted live create in a newly generated Cirrove-owned test
   folder. Endpoint: successful FUSE save, applied upload receipt, independent
   exact-ID listing and full-byte readback.
3. One fresh-process `--resume` of that UUID. Endpoint: same exact remote ID
   and full-byte FUSE read with no second upload.

Before each live arm, record the exact command, binary SHA-256, PID, expected
duration and private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in its private
manifest. No compilation occurs during an arm. These are functional checks,
not performance evidence. Normal iCloud connections remain read-only;
this experiment does not prove arbitrary sizes, names, collisions, concurrent
edits or uncertain-response recovery for the general adapter.

## Results

The full local `scripts/check.sh` passed, including format, clippy, workspace
tests, script tests and the ledger. A pre-existing rustdoc link warning did
not fail the docs check.

The fresh live arm (PID 4099572, binary SHA-256
`16de365a3e75ca3ddd993e1969e5ca37d7238603405d4505417b93d89429e05d`)
exited 0. It created fixture
`de07f8c0-ae9f-485e-a969-4159dbda2016` and reported a confirmed mounted
create, independent Apple listing, complete byte readback and applied journal
receipt. The private run manifest and log are retained under
`.local-state/icloud-general-create-83fb25c7-133e-4d02-a52f-0f295ab3b062/`.

The fresh-process `--resume` arm (PID 4101760, same binary SHA-256) exited 0.
It remounted the same fixture and reported the confirmed remote file and
complete bytes without initiating another create. Its private manifest and
log are retained under
`.local-state/icloud-general-resume-c9ed3d7f-d641-4777-9c9e-f45fa75048aa/`.
Both arms used private btrfs-backed temporary directories. No normal
Cirrove service was restarted.

This verifies one small file through the new adapter and its remount path.
It does not validate the adapter's lost-registration recovery, arbitrary
file sizes or names, collision handling or ordinary writable accounts.
