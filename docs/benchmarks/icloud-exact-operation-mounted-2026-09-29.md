# Exact journal operation for isolated iCloud replacement, 2026-09-29

## Registered before live execution

Question: after the shared transfer worker passes its durable upload operation ID
through every provider phase, can the isolated mounted iCloud replacement reopen
the exact journal row and complete without relying on a scan for a matching
request? Two identical pending requests are ambiguous under that scan. A
synthetic worker test verifies distinct IDs for identical requests. The live
arm checks the new routing on an owned file, not duplicate real writes.

Use fresh owned fixture `6db8bbab-2dcd-4a5a-b6ac-6d963dbae41a`. Arm A
creates and independently verifies a file through FUSE. Arm B replaces that
file through FUSE, checks exact new and old IDs, full new bytes, recoverable
Trash handoff, and the absence of a visible staged duplicate. Arm C starts a
new process and remounts to verify the resulting bytes, IDs and journal
without another mutation. The old file bytes are verified before replacement
but not downloaded from Trash afterward.

Prediction: all three arms pass. A failure or uncertain result stops the
sequence without retry. Each arm has a private manifest with command, binary
SHA-256, PID, expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`.
A and C have 360-second bounds, B has 900 seconds. No concurrent compilation
or other local measurement will run. The installed service, ordinary iCloud
connections and other mounts stay unchanged and read-only. One successful
fixture establishes feasibility of exact-operation routing, not account-wide
write reliability.

## Observation

All three arms passed for owned fixture
`0877d9aa-dce6-455c-b56a-7cdf727625f0`. Arm A created and verified the
file by independent iCloud listing and journal receipt. Arm B replaced it
through the mounted path using the exact journal operation ID and verified
the two-ID receipt, new contents, recoverable old identity and absence of a
visible staged duplicate. Arm C started a new process, remounted, and
verified the new bytes, IDs and journal without another mutation. The test
mounts shut down. The ordinary installed service was not restarted.

Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are in
`.local-state/icloud-exact-operation-6db8bbab-2dcd-4a5a-b6ac-6d963dbae41a/`.
They record binary SHA-256
`e3ed8ba65694528965fc4b84907d91d7ee447ec544fbfed78b71499343e14a55`,
PID, expected window and btrfs-backed private temporary directories. No
compilation or second measurement overlapped the arms.

Arm B took roughly eight minutes despite its small payload. That is a
material latency concern, not a performance pass. Each arm ran only once,
so there is no within-arm spread to quote and no claim of reliability or
normal-account write readiness. The result establishes that exact-operation
routing works for this owned mounted replacement. It does not exercise two
simultaneously pending identical live saves or an interrupted handoff.
