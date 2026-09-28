# iCloud mounted empty-folder rename, 2026-09-28

## Registered before live arms

Question: Can a separate application rename a Cirrove-owned empty folder
through an isolated writable FUSE mount, with the shared mutation journal
preserving the exact remote ID and a fresh process seeing the same name?

The first arm creates a new UUID-named Cirrove validation folder, mounts only
that folder with the feature-gated provider, creates one small file and one
empty `Mounted Folder`, waits for confirmed upload and folder-create receipts,
then unmounts. The second arm reopens that exact private journal and mount
and has a separate Python process call `os.rename('Mounted Folder',
'Mounted Renamed')`. The adapter accepts only this owned folder, root parent,
current ETag and target name, persists the exact prepared folder ID, sends
one conditional `renameItems` request, and accepts only an exact-ID receipt.
An independent iCloud session verifies the renamed folder ID, the absence of
the old name and the untouched uploaded file's full bytes. A third fresh
process remounts the same run and verifies the new name and journal receipts.
No existing user item is addressed, and installed services are untouched.

Endpoint: exact folder ID retained with `Mounted Renamed` through FUSE,
provider listing, journal receipt and remount; otherwise explicit
conflict/unresolved/failure. Prediction: the direct conditional rename
behavior seen earlier will hold through the provider-neutral mounted worker.
This is one isolated empty folder and one application workflow, not a
general write-reliability claim. Normal iCloud mounts remain read-only.

Each exclusive arm records a private manifest with command, binary SHA-256,
PID, expected duration, start/completion time and btrfs-backed private
`TMPDIR` and `SQLITE_TMPDIR`. No measurement or compilation overlaps an arm.

After the first application attempt returned `EOPNOTSUPP` from the shared
FUSE layer, the implementation added same-parent folder rename in the
provider-neutral namespace. A follow-up arm with a newly created fixture
will exercise the final capability-gated source: only the isolated validation
provider opts in, so installed OneDrive/Google behavior remains unchanged.
Prediction: that arm will repeat the successful exact-ID rename and remount
result seen before the capability gate was added.

## Results

The initial create arm (PID 3568183) exited 0 and confirmed one uploaded
file and one empty `Mounted Folder` in a new private run
`c2eb45b4-941b-4a77-8d01-1dea7ed70477`. The first rename attempt failed
in the application with `EOPNOTSUPP` (errno 95); no rename mutation entered
the journal. This is the observed failing baseline for the shared-layer fix,
not an iCloud-provider rejection. Its retry with errno-only reporting is
recorded by PID 3572732, exit 1, and a distinct binary SHA-256.

After the shared same-parent folder path was added, PID 3584966 ran the
rename on that same isolated fixture and exited 0. The application observed
the new name through FUSE. The worker produced an accepted exact-ID
`Relocate` receipt, and an independent Apple session saw the same folder ID
under `Mounted Renamed`, no old name, and the file's original full bytes.
A fresh-process remount (PID 3586158) read the renamed folder and original
file through FUSE and confirmed the saved journal receipts, exiting 0.
Private manifests record commands, binary SHA-256, btrfs temp paths and
timestamps. These first successful arms predate the provider capability
gate; the final-source follow-up is recorded below.

The final capability-gated binary SHA-256
`994687c67ce47b4292a00d7d422c5ab5b1222200ec15c60beada1f3ac1ce73ee`
ran all three phases on a second new private fixture
`f396219e-7ac7-40ac-b939-20e5e67388af`. Create (PID 3592049),
mounted rename (PID 3593644) and fresh-process remount (PID 3595287)
each exited 0. The rename phase verified the exact folder ID in the shared
journal receipt and independent Apple listing, absent old name, and full
bytes of the unchanged file. The remount read the renamed folder and file
through FUSE and rechecked the receipts. Each private manifest records the
command, matching binary SHA-256, btrfs temp path and timestamps.

This closes one same-parent, empty-folder rename workflow in the isolated
validator. The normal iCloud account remains read-only; populated folders,
cross-parent moves, concurrent clients and real timeout recovery are not
established by this arm. One run per arm supports a functional direction,
not a reliability or performance result.

The same-parent journal regression test passed with one expected test
executed. The full local `scripts/check.sh` passed after the final code and
test changes. Window scenarios requiring a display are outside that command.
