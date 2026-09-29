# Isolated iCloud mounted rename using the sealed session, 2026-09-29

## Registered before live execution

Question: does an ordinary FUSE file rename still complete when the isolated
validator constructs its general iCloud mutation adapter from the same sealed,
keyring-backed session path as an account, instead of an in-memory snapshot?
The fixture is the already created and independently verified
`d506e959-a92f-4d86-b6bd-d081db1225b1` owned test tree. Its file and folder
have final journal receipts. No other cloud item, normal mount or installed
daemon is targeted; ordinary iCloud accounts remain read-only.

Arm A runs `--rename-file` for that fixture. A separate process renames the
existing file through FUSE. Require one applied conditional mutation receipt,
the same exact file ID and bytes under the new name in an independent iCloud
listing, and absence of the old name. Arm B, only after A succeeds, runs
`--resume-after-file-rename` in a fresh process and checks the same state
without submitting another mutation. An uncertain outcome stops the sequence;
neither arm retries an uncertain rename.

Prediction: both arms pass because the general sealed-session rename adapter
has direct coverage, but this mounted wiring has not been exercised. Exact
receipt IDs, full bytes, journal state, process exit and mount shutdown are the
endpoints. A single sequence is functional evidence, not proof of reliability
under concurrent edits or timeouts. Each arm records its command, binary hash,
PID, expected duration and disk-backed private `TMPDIR`/`SQLITE_TMPDIR` before
running. No compilation or other measurement runs alongside either arm.

## Rename result and registered move extension

Both rename arms exited zero. Arm A applied the conditional file rename and
matched the exact file ID, bytes and new name to an independent iCloud listing
and journal receipts. Arm B remounted in a fresh process, read the renamed
file, and verified the same state without another write. The validator shut
down the test mount after each arm. The private manifests are
`.local-state/icloud-sealed-rename-control-dd4c43fd-30ed-4192-98ed-2dc1f705f2ef/arm-a.json`
and `arm-b.json`, with binary SHA-256
`1f21804d98286be71cc109c1deb76c881846ce86527cc1b7a2193ccc9a2d3d4d`,
360-second expected durations and btrfs-backed private temporary directories.

Before further execution, register an extension on the same owned fixture.
The validator's file-move scenario expects the file to have undergone its
second rename first. Arm C runs `--rename-file-again`; only after it passes,
arm D runs `--move-file` into the already confirmed child folder. Arm E runs
`--resume-after-file-move` in a fresh process without another write. The
endpoint for D and E is the same exact file ID and full bytes in the child
folder, absence at the old parent, applied receipts and mount shutdown.
Prediction: all three arms pass through the general sealed-session rename and
move adapters. A failed or uncertain arm ends the sequence with the fixture
and journal retained. Separate private manifests record command, binary SHA,
PID, expected 360-second window and disk-backed temp directories before each
exclusive arm. No other account item is targeted.

## Move extension result

Arms C, D and E all exited zero. C completed the second conditional rename
and matched the same ID, bytes and new name in an independent iCloud listing.
D moved that exact file ID into the confirmed child folder; the validator
matched the applied journal receipt, full bytes in the destination and absence
from the source. E remounted in a fresh process, reopened the file and
independently verified the same state without another mutation. The test mount
was detached after each arm. The additional manifests are `arm-c.json`,
`arm-d.json` and `arm-e.json` beside the first two, with the same executable
SHA-256 and btrfs-backed private temporary directories.

This covers file rename and file move through the sealed-session general
adapters on one owned FUSE fixture. The code also selects sealed-session
constructors for file Trash and folder rename, move and Trash, but these
paths have not been rerun live after this wiring change. It does not establish
normal-account reliability, concurrent edit safety, timeout recovery or
support for empty files.
