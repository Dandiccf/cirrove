# Mounted iCloud file creation inside an owned folder — 2026-09-29

Question: can the shared FUSE upload worker create a new file inside a
journal-confirmed Cirrove-owned child folder, then restore the exact file and
parent identity after a fresh process remounts? Previously the isolated
upload adapter accepted only the fixture root.

Prediction registered before the live arm: a separate application's
`O_CREAT|O_EXCL`, write, `fsync` and close under `Mounted Folder` should create
one additional upload journal row. The worker must validate the child's
applied folder-create receipt and live parent identity before registering the
file. An independent Apple listing should show one `Nested Created.txt` with
the exact uploaded file ID, size and complete SHA-256 bytes. A new process
should restore both uploaded IDs and reopen both files through FUSE without a
new mutation. A foreign or unconfirmed parent must be refused.

Arms and endpoints:

1. Synthetic provider and journal tests check that the root adapter refuses a
   nested parent, the child adapter rejects a foreign ancestor, and a nested
   upload cannot restore before its parent has an applied exact-ID receipt.
   These guards are not evidence of Apple-side behavior.
2. Reuse only the existing generated UUID fixture
   `a01359b8-9644-4917-84f7-455228b2068b` and its confirmed `Mounted Folder`.
   A separate Python process creates one small text file through FUSE.
   Endpoint: second upload `Uploaded`, exact account/collection/parent bound
   receipt, independent Apple listing with exact file ID and full bytes, and
   the previously moved-and-renamed file unchanged.
3. A fresh process restores the private journal and metadata store, remounts
   the fixture and reads both nested files through FUSE. It independently
   repeats the ID and byte checks without another write intent.

Before each live arm, record command, binary SHA-256, PID, expected duration
and private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in its manifest. Run no
compilation concurrently. This is a functional check, not a performance
measurement. The installed daemon and user files are untouched. One generated
file does not establish general iCloud write reliability, real network-timeout
recovery, concurrent-client safety or arbitrary nested trees. Ordinary iCloud
connections stay read-only.

## Results

The synthetic provider and journal tests passed. The first live arm (PID
3816805, binary SHA-256
`8d6df999dbc90aaadcfc9194ab1636920447ba9c95a2bc9647bfbb9a795819bb`)
exited 0 after the separate application created and fsynced the generated
file. The upload worker marked the second upload `Uploaded`. An independent
iCloud session found its exact receipt ID, parent ID, ETag, full size and
SHA-256 bytes under `Mounted Folder`; the previously moved and renamed file
remained byte-identical. The fresh-process remount arm (PID 3817928, same
binary) exited 0. It restored the private journal, reopened both nested
files through FUSE and repeated the independent identity and byte checks
without adding an upload or mutation. Both private manifests record
btrfs-backed temporary storage and preserve the arm logs.

This is one small generated file in one confirmed child folder. It does not
prove arbitrary nested writes, conflict handling under another client,
recovery from a real uncertain registration response or permission to enable
ordinary iCloud writes.
