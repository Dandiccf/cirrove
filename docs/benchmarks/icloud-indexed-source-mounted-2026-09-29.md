# Indexed source identity for isolated iCloud replacement, 2026-09-29

## Registered before live execution

Question: can the mounted replacement obtain its exact original file and
parent from Cirrove's visible metadata index rather than a prior upload
receipt, while the existing mutation-free iCloud preflight still validates
the remote revision? This is necessary for editing a pre-existing iCloud
file in a normal account. The isolated validator still confines mutation
to its owned test tree and uses its journal to authorize that ownership and
locate the pending replacement operation.

The new Store query reads the item and its ancestor chain to the selected
root in one SQLite snapshot, scoped to account and collection. Synthetic
checks reject another account, missing ancestry and a parent cycle. The
live arms use a new UUID-owned fixture: A creates and verifies a small file
through FUSE; B edits that same file through FUSE and uses the index-derived
source node and parent to construct the replacement adapter, then requires
a durable two-ID receipt, exact old ID in recoverable Trash, complete new
bytes and no visible staged/recovery duplicate; C starts a fresh process
and verifies the mount, journal, IDs and bytes without another mutation.
The old file bytes are verified before replacement but not downloaded from
Trash afterward.

Prediction: all arms pass. A failure or uncertain result stops the sequence
without retry. Each arm has a private manifest recording command, binary
SHA-256, PID, expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`;
B's bound is 900 seconds, A and C use 360 seconds. No concurrent local
measurement or compilation runs. The installed service, normal iCloud
connections and other mounts remain unchanged and read-only. One successful
fixture will establish feasibility of indexed resolution on an owned file,
not general conflict or account-wide write reliability.

## Observation

All three arms passed for fresh owned fixture
`4e39b01a-048e-4102-93b9-71aff859a2ac`. Arm A independently verified
the source receipt and bytes. Arm B reopened the private metadata database,
resolved the exact source node and parent chain within one account-scoped
SQLite snapshot, used the existing remote original-hash preflight, and
completed the mounted two-ID replacement. Independent iCloud listing,
full-byte read and journal receipt confirmed the new ID and contents,
the old exact ID in recoverable Trash, and no visible stage/recovery
duplicate. Arm C used a new process and passed the mounted read and
independent remote/journal verification without another mutation. All
test mounts shut down.

Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are in
`.local-state/icloud-indexed-source-9860e7d3-dc49-40e9-9046-0ca0f9c9512c/`.
Each records binary SHA-256
`b7e22503d7f40f41bb9594afc79066744d9480903d70d433d5c981b8a8b5ba5e`,
PID, expected window and btrfs-backed private temporary directories.
No compilation or second local measurement overlapped the arms. The
original bytes were verified before replacement but not read from Trash
afterward.

This passes indexed source resolution on one owned fixture. Ownership and
pending operation selection still come from the fixture journal; there is
no normal account-wide iCloud write provider or writable account setting.
This run does not establish pre-existing-file editing, concurrent conflicts
or general provider reliability.
