# iCloud two-ID handoff through the shared transfer worker — 2026-09-27

Registered before the live run. Only a fresh UUID-named Cirrove test folder
with two newly created small files in the isolated `iCloudGuiValidation`
account may be changed. The normal mount remains read-only.

Question: can the shared transfer worker reserve the old file's recovery
identity, save separate checkpoints before each iCloud rename, independently
verify both exact IDs and full bytes, then publish the new current binding and
old recovery binding as one durable journal receipt?

Arm A: create and read back an original test file, upload a distinct staged
file in the same new folder, create a local working replacement with the
staged bytes, and seal it in a private journal. The worker reserves the
recovery name, moves the old ID to it, saves a new checkpoint, then installs
the staged ID at the original name. No comparative arm. A timeout, missing
response or divergent observation must retain `VerifyRequired` or conflict
without blindly resending an uncertain rename.

Prediction: the journal ends `Uploaded`, with the staged ID as the current
remote binding and the old ID owned by a hidden recovery object; both remote
files retain their complete expected bytes. One run cannot establish
concurrent-edit safety, response-loss recovery, interactive latency or support
for ordinary files. Apple's rename endpoint has accepted a stale ETag in an
earlier fixture trial, so this cannot enable mounted replacement.

Endpoint: final journal state and exact current/recovery identities, plus the
adapter's full-byte verification. A private manifest records command, binary
SHA-256, PID, expected duration and disk-backed temporary storage before
execution. No credentials, signed URLs, raw provider bodies or file bytes are
recorded here. Both test versions remain in iCloud Drive.

## Observed Arm A

Operation `e0b141f4-9a79-429a-8109-ebe61c9ac7c9` reached `Uploaded`.
The shared journal recorded the staged ID as the current remote binding and
the former ID as a remote-owned, hidden recovery object. Before returning its
two-ID receipt, the native adapter re-listed both exact IDs and read and
hashed both complete files. The private run manifest records binary SHA-256
`ab14ffbd5d4749a41f60a144ecfcd17ef439d52cb99402a10156cde19bcf139d`
and btrfs temporary storage. This is one owned fixture and one run; there
is no within-arm spread. It does not show that a concurrent edit between the
final old-file check and Apple's rename would be rejected, nor that a crash
at either pending request can always be resolved.
