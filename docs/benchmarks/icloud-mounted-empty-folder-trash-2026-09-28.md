# Mounted iCloud empty-folder Trash, 2026-09-28

## Registered before the live arm

Question: Can a separate application call `rmdir` through the real FUSE mount
on a previously confirmed, Cirrove-created empty iCloud folder, have the shared
mutation worker move that exact folder to recoverable Trash, and leave a
neighbouring file intact?

Arm: reopen the isolated
`Cirrove Write Validation-37fc6800-2a7f-4f43-9a6d-fd1d4f2c5938` fixture
with `cirrove-icloud-mounted-write-probe --remove-folder` and that UUID. The
only deletion target is its journal-confirmed `Mounted Folder` with item ID
`FOLDER::com.apple.CloudDocs::3F445BB1-2A17-425B-89AD-C1479EBEBEF3`.
The folder had no children in the preceding mounted run. The adapter still
re-lists its children immediately before Apple's conditional Trash request;
if it became nonempty or changed ETag, it must stop rather than delete. The
separate Python process calls `os.rmdir` and reads `Mounted Create.txt` as an
untouched control. The normal iCloud connection, account settings and daemon
remain untouched.

Endpoint: two applied mutation records, one Create upsert and one Remove receipt
for the *same exact folder ID*; independent iCloud parent listing lacks that
folder but retains the exact uploaded file; its bytes and SHA-256 match the
existing journal receipt. The removal adapter itself must observe the exact
folder ID in a complete Trash listing before returning success. A failure or
uncertain outcome retains the local journal and fixture; no retry follows from
a matching filename or a missing parent listing alone.

Prediction: the shared `rmdir` path emits a `RemoveFolder` intent whose remote
node carries the confirmed ID and ETag, and the adapter reaches `Applied`
after exact-ID Trash inspection. This single arm is functional evidence only;
it cannot prove atomicity against a concurrent child creation between the final
listing and Apple's Trash call, nor real network-timeout reliability.

## Result

The first attempt failed in the separate FUSE application before it could
complete `rmdir`. The validator exited nonzero and detached its mount. Read-only
reopening of the local SQLite journal found exactly the original one uploaded
file and one applied folder Create, with **no** RemoveFolder intent. The
application's stderr was not reported by the first validator version, so the
refusal's errno is unknown; no cloud-success claim follows. The private
manifest is
`.local-state/icloud-mounted-folder-trash-control-573be198-1e46-4740-81f0-2d0c80048ef3/manifest.json`:
PID 2776871, binary SHA-256
`6adaf112f96ef9bb713c7910b754b6a8d7f7d0e48ba88e19b3af72ce3f23e7b9`,
start `2026-09-28T10:50:37Z`, expected window 360 seconds, btrfs-backed
`TMPDIR` and `SQLITE_TMPDIR`.

Correction before a second arm: the isolated Python process now reports only
the numeric `rmdir` errno (no provider body or credentials) if the syscall
fails. The second arm reopens the same fixture and expects the same endpoint;
its result must distinguish a local projection refusal from a journaled
provider outcome. No automatic cloud retry is inferred from the first failure.

The second arm again stopped before journaling a removal and reported
`rmdir_errno=13` (`EACCES`). Inspection of the FUSE call path identified a
local adapter gap: `rmdir` asks for the target folder's child listing, while
the isolated read adapter permitted only listings of the fixture root. The
new adapter accepts a confirmed owned child folder's complete listing, and
refuses the listing if it contains any unowned child; it never turns an
unreadable or incomplete folder into an apparently empty one. The second
arm's private manifest is
`.local-state/icloud-mounted-folder-trash-control-436b8392-2115-4da6-99fd-bbde0fbeb64d/manifest.json`.

A third arm is registered with the same exact target and endpoint after this
read-side correction. The prediction changes narrowly: FUSE's emptiness check
should now succeed on the still-empty owned folder, allowing a durable
`RemoveFolder` intent to reach the provider worker. A provider refusal or an
uncertain outcome remains a failed arm and leaves evidence in the journal.

The third arm passed. The separate process completed `os.rmdir`; the shared
mutation worker reached `Applied`. The validator's independent iCloud parent
listing no longer contained `Mounted Folder`, retained `Mounted Create.txt`,
and read its complete 43 bytes with the same journal SHA-256. Read-only SQLite
reopening found one uploaded file, one applied folder Create and one applied
folder Remove. The Remove receipt carried exactly
`FOLDER::com.apple.CloudDocs::3F445BB1-2A17-425B-89AD-C1479EBEBEF3`.
The mount was absent at exit. The private manifest is
`.local-state/icloud-mounted-folder-trash-control-33b4c35b-232c-4acb-ad96-fa1d2d33a9cb/manifest.json`:
PID 2780823, binary SHA-256
`2f397dbb5e0e82d60e7ff379c1a6e69344a12d3bd6d02551516ffc0cd187de6d`,
start `2026-09-28T10:53:45Z`, expected window 360 seconds, with btrfs-backed
temporary directories. No completion timestamp was recorded, so this supports
no timing claim.

## Post-removal remount registered before execution

Question: Does a new process reconstruct the owned-ID set from both journal
receipts so the removed folder stays absent while the retained file remains
readable? The arm runs `--resume-after-remove` for the same UUID and only reads
through a new FUSE mount. Its endpoint requires the file's bytes and exact
remote identity to match the upload receipt, the removed folder to be absent
from FUSE and the independent parent listing, and the journal still to contain
only the existing three records. Prediction: replaying the applied Remove
receipt removes the folder ID from the allowlist without another cloud request.

The post-removal remount passed. The separate process read the retained file
through FUSE and found no `Mounted Folder`; an independent restored iCloud
session again matched the file ID and all bytes and saw the folder absent.
Read-only SQLite inspection afterward still found one `uploaded` and two
`applied` rows. The FUSE mount was absent after exit. The private manifest is
`.local-state/icloud-mounted-post-trash-control-f5359493-a307-4668-bd20-591f7fb09466/manifest.json`:
PID 2785363, binary SHA-256
`57282a8f4444203eb8d0365871172a443224d10aad3ead45cf0212426a4296df`,
start `2026-09-28T10:58:27Z`, expected window 360 seconds, btrfs-backed
temporary directories. This read-only arm added no journal intent; it does not
establish general iCloud deletion or repeatability under concurrent clients.
