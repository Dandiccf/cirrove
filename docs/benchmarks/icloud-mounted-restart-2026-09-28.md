# Isolated iCloud mounted restart, 2026-09-28

## Registered before the live arm

Question: After the first writable FUSE process has stopped, can Cirrove reopen
the exact same isolated account, test folder, metadata store and upload journal,
restore only its confirmed remote IDs, and read the file and folder through a
new FUSE mount without sending another cloud mutation?

Arm: `cirrove-icloud-mounted-write-probe --resume
37fc6800-2a7f-4f43-9a6d-fd1d4f2c5938`. That UUID identifies the second
fresh Cirrove-owned fixture from the mounted Create artifact. The validator
verifies the UUID-named root against an independent live root listing. It reads
successful journal receipts and admits only IDs whose account, collection,
parent, kind and original Create name match. A separate Python process then
opens the existing mounted file and checks its bytes and the existing folder.
The validator independently lists iCloud again and compares exact IDs, parent,
size and full-byte SHA-256 to the saved receipts. It is not asked to write.

Endpoint: successful FUSE read and folder lookup after remount; exactly one
uploaded file and one applied folder in the reopened journal; independent exact
ID and byte match; clean unmount; no new upload or mutation record. Any foreign
or malformed journal receipt stops before the mount. Prediction: the prior
process's confirmed receipts reconstruct both allowlisted IDs, and the shared
metadata store retains their routes. A failure would identify a restart gap that
the two fresh-mount runs could not expose.

This is one functional arm, not a reliability or timing measurement. Record the
binary hash, process ID, disk-backed temporary filesystem and result below.

## Result

The live remount passed. The independent Python process opened the previously
created file through FUSE, compared its complete 43 bytes, and found the
previously created folder. A separate restored iCloud session then matched both
exact item IDs and parent IDs to the reopened journal receipts and verified the
file hash. Read-only SQLite inspection after shutdown still found exactly one
`uploaded` row and one `applied` row, so this run did not add a write intent.
The FUSE mount was absent after exit.

The private manifest is
`.local-state/icloud-mounted-restart-control-d1b12bf3-526d-46f3-9cde-494d82050935/manifest.json`:
PID 2750761, binary SHA-256
`b80418ed15881bf8aec030b70ace0b1cbc678b2d61df8f32f5c8dddc84bc48ee`,
start `2026-09-28T10:31:52Z`, expected window 360 seconds, with both temporary
directories on btrfs. The endpoint had no stored completion timestamp, so no
timing claim follows. This single successful restart covers confirmed direct
children of one Cirrove-owned folder. It does not validate resumed uncertain
mutations, nested writes or arbitrary application editing.
