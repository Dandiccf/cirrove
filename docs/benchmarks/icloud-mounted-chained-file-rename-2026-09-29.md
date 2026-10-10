# Mounted iCloud file identity across consecutive renames — 2026-09-29

Question: after the shared journal confirms a mounted file rename, can a new
process safely rename the same Cirrove-owned file again using its current
identity and ETag, then reopen it after a second restart? The previous
fixture guard required the original upload receipt's name and ETag, so it
would refuse this ordinary follow-up operation.

Prediction registered before the live run: a journal walk of the confirmed
upload and applied relocation receipts will authorize only the latest file
version. The second FUSE `os.rename` will keep the same Apple item ID and
complete bytes, and an independent listing and fresh-process remount will
show only `Mounted Renamed Again.txt`. A stale original version must be
rejected by the synthetic regression.

Arms and endpoints:

1. Baseline: the pre-change synthetic constructor test fails for a second
   rename. It failed with the expected assertion; the filtered command was
   rerun without `--exact` to confirm one test actually executed.
2. Updated synthetic checks: the constructor accepts the second rename and
   a private journal with a confirmed upload plus applied first rename gives
   the SHA-256 only for the latest exact-ID/ETag version.
3. Isolated live arm: reuse the previously created, Cirrove-owned UUID test
   folder `a01359b8-9644-4917-84f7-455228b2068b`. A separate process
   renames its confirmed test file once more through FUSE. The endpoint is an
   applied journal receipt, independent Apple listing with only the new name,
   unchanged exact file ID and full-byte SHA-256, followed by a fresh process
   that remounts and reads the same file. No user files are touched.

This is a functional compatibility gate, not a performance claim. Record
each process's PID, binary SHA-256, private btrfs-backed temporary directory,
exit and result in a private manifest before and after its run. Do not compile
during a live arm. One controlled run does not establish real timeout or
concurrent-client reliability. Ordinary iCloud mounts stay read-only.

## Results

The pre-change constructor test failed as predicted, executing one test. With
the change, that test and the private-journal chain test passed; the latter
also rejected the stale pre-rename version. The live second-rename arm used
binary SHA-256 `4c97fd0497af8d045b213a5c954a16d62b512033b71618b0714f864ae9a7a6cb`
and btrfs-backed private temporary storage. Its recorded PID 3685294 exited
0. The separate application renamed `Mounted Renamed.txt` to
`Mounted Renamed Again.txt` through FUSE. The journal reached an applied
second `Relocate` receipt for the same file ID. Independent iCloud listing
found only the new name, and complete bytes matched the original upload
SHA-256. The fresh-process remount arm, PID 3686360 with the same binary,
also exited 0 and reopened the file by its new name through FUSE. Both
manifests and logs are retained privately under the worktree's `.local-state`.

One controlled file does not prove arbitrary names, real network-timeout
recovery, concurrent edits or general writable iCloud mounts. The new guard
still requires a journal-confirmed upload and applied exact-ID relocations
under the same isolated test root.
