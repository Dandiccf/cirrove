# iCloud mounted file rename, 2026-09-28

## Registered before live arms

Question: Can an ordinary application rename one Cirrove-owned file through
an isolated writable FUSE mount, with the shared journal preserving its
exact iCloud ID and full bytes across a fresh-process remount?

The create arm makes a new UUID-named Cirrove validation root, mounts only
that root, writes `Mounted Create.txt` with generated test bytes and creates
one empty `Mounted Folder`. It waits for confirmed upload and folder-create
receipts and independently verifies cloud identities and file bytes before
unmounting. The rename arm reopens that private journal and mount; a separate
Python process calls `os.rename` to `Mounted Renamed.txt`. The feature-gated
adapter accepts only the confirmed uploaded file ID, current ETag, root
parent, original and target names, bounded size and the journal's full
SHA-256. It persists the exact prepared ID, sends one conditional Apple
rename request, then verifies the exact ID/name and full bytes before an
accepted `Relocate` receipt. The independent reader checks that the old
name is absent and folder/file data are intact. A third process remounts
the same run and repeats FUSE and journal verification. No existing user
item or installed service is addressed.

Endpoint: one exact-ID file rename visible through FUSE, Apple listing,
journal receipt and fresh-process remount, or an explicit unresolved/error
state. Prediction: the direct conditional rename behavior will pass through
the existing provider-neutral file relocation path. This is one small file
and one application workflow; it does not prove arbitrary writes,
concurrency, genuine timeout recovery or release reliability. Normal iCloud
connections remain read-only.

Each exclusive arm records command, binary SHA-256, PID, expected duration,
start/completion time and btrfs-backed private `TMPDIR` and
`SQLITE_TMPDIR` in its private manifest. Arms do not overlap each other or
compilation.

## Results

One new private fixture `a01359b8-9644-4917-84f7-455228b2068b` passed
all three exclusive arms with binary SHA-256
`b5c8d4f34244b97136f610a2044d141cfc59454ca79193b9f210f97440e8e830`.
The create arm (PID 3634728) confirmed the exact uploaded file and empty
folder in iCloud. The rename arm (PID 3638761) had a separate Python
application call `os.rename` through FUSE. The shared worker prepared the
exact file ID, received an accepted conditional rename and recorded an
`Upsert` receipt for that same ID at `Mounted Renamed.txt`. An independent
iCloud session found no old name and verified the file's complete bytes and
journal SHA-256. The empty folder retained its original identity. Both arms
exited 0.

A third process (PID 3640243, exit 0) reopened the private journal and
mount. It read the renamed file and unchanged folder through FUSE, checked
the original bytes and rechecked both cloud identities and journal receipts.
The private manifests record commands, matching binary SHA-256, btrfs-backed
temporary path, timestamps and exits. The elapsed times are context only,
not a performance result.

This is one small owned file and one mounted application workflow. It does
not establish arbitrary filenames, larger files, case collisions,
simultaneous clients or real transport-timeout recovery. Ordinary iCloud
connections remain read-only.
