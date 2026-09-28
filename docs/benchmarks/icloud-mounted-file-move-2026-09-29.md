# Mounted iCloud file move into an owned folder — 2026-09-29

Question: can a separate application move a journal-confirmed Cirrove test
file from the isolated FUSE root into a journal-confirmed child folder,
keeping its Apple identity and complete bytes visible after a fresh process
remounts? The prior fixture restored only same-parent renames and resolved
only direct children.

Prediction registered before the live arm: a conditional `moveItems` bound to
the current exact file ID and ETag will place that same ID under the owned
child folder. The source root will no longer contain it. The shared journal
will accept one `Relocate` receipt, and independent Apple listing plus a
fresh-process FUSE read will find the same bytes only at the destination.

Arms and endpoints:

1. Synthetic baseline: a private journal with an owned upload, an applied
   child-folder create and a pending file move failed restoration before the
   change with `non-fixture rename in isolated journal`. The updated test
   must pass and a separate adapter guard must refuse an unprepared move.
2. Live move: reuse only the previously generated Cirrove UUID folder
   `a01359b8-9644-4917-84f7-455228b2068b`, whose one small test file has
   already had two journal-confirmed renames. A separate application calls
   FUSE `os.rename` into its own `Mounted Folder`. Endpoint: applied exact-ID
   receipt, complete independent source/destination listings, full-byte hash.
3. Remount: a new process restores the private journal, opens the nested
   file through FUSE and repeats independent identity and byte checks.

Before each arm, record command, binary SHA-256, PID, expected duration and
private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in a private manifest. Do not
compile during an arm. These are functional checks, not performance evidence.
No user files or normal daemon state are touched. One controlled nested move
does not establish arbitrary trees, collisions, concurrent edits or real
network-timeout recovery; ordinary iCloud mounts remain read-only.

## Results

The synthetic restoration regression failed before the change as predicted:
one executed test returned `non-fixture rename in isolated journal`. It passed
after the ownership rule was extended. The adapter's exact-prepared-ID guard
also passed its targeted test.

The live move arm (PID 3726185, binary SHA-256
`e902bc6960587cd8a32aef4eb51aeec7df925352746450bd558dfc1e57543078`)
exited 0. The separate application moved the file through FUSE, and an
independent iCloud session found the same file ID and complete bytes in the
owned child folder, absent from the source. The shared journal contained an
applied fourth mutation receipt for that identity.

The first fresh-process remount (PID 3727273, same binary) **failed** while
reading the nested file. Further read-only diagnostic remounts (PIDs 3731748,
3733187 and 3734939) reproduced `EACCES`: the isolated provider received a
local folder identity as the file's parent, which was not one of its confirmed
remote folder IDs. The shared writeback read path replaced the local file ID
with the remote ID but left `parent_id` local. This is a provider-neutral
read-path defect exposed by iCloud's parent-scoped download protocol.

After the shared read path also supplied the confirmed remote parent ID,
the fresh-process remount (PID 3739709, binary SHA-256
`5731231ecfd3c4a0eb1a475e2424303a6d7b79400eca94eee3a05d10e0f8e057`)
exited 0. It opened the nested file through FUSE and independently verified
the exact cloud identity, source absence and complete original bytes. All
arms used the private btrfs-backed temporary directory recorded in their
manifests; the logs and failed results remain alongside them. No regular
Cirrove service was restarted.

This proves one generated, small file move and remount after a concrete
parent-identity correction. It does not prove arbitrary folder depth, real
network timeout recovery, concurrent edits, collisions or normal writable
iCloud connections.
