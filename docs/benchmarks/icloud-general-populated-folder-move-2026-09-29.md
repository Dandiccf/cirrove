# General iCloud populated-folder move — 2026-09-29

Registered before the live run. The scope is two new UUID-named
Cirrove-owned parent folders and one nested folder containing one generated
small file in the isolated `iCloudGuiValidation` account. Existing user
items, ordinary mounts, account settings and the installed daemon are not
changed. Normal iCloud connections remain read-only.

Question: can the normal-build `ICloudFolderMove` adapter use the shared
mutation worker to conditionally relocate one exact folder ID between two
parents, persist a receipt, and preserve its child's file ID and complete
bytes after a fresh process independently lists both parents and the child?

One mutation arm prepares the saved source ID and ETag, checks both parents
and the destination name, sends one `moveItems` request, and requires an
`Applied` receipt with the same folder ID and the new parent. A separate
read-only arm must find that folder only at destination and confirm the
child's document ID, size and full SHA-256. Prediction: both arms pass. A
missing or ambiguous response leaves the journal for exact-ID
reconciliation, never a blind second move.

Endpoints are the worker journal receipt, independent remote listings and
complete child-file bytes. This one-child trial does not prove all subtree
sizes, concurrent edits, a lost move response, FUSE ancestry relocation or
normal-account reliability.

Each exclusive arm records its command, binary SHA-256, PID, expected
duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR` in a private manifest
before it runs. No other measurement or compilation runs concurrently. No
tokens, cursor URLs, signed URLs, raw provider bodies or file contents are
logged.

## Observation

The mutation arm reached `Applied` for fixture
`d37a0592-05a5-4924-b0f2-cb55fda59165`. The worker's SQLite mutation
receipt named the original folder ID and the exact destination parent. A
separate read-only process then found the source parent empty, the same
folder ID only at destination, and the original child file/document IDs
with matching size and complete SHA-256. Both processes exited zero. The
private manifests are
`.local-state/icloud-general-folder-move-run-522e0494-2758-456c-a48e-06627f48ffbf/manifest.json`
and
`.local-state/icloud-general-folder-move-inspect-0a5f2162-9a0d-45f6-a177-8bafe7c3c031/manifest.json`.
They record btrfs temporary storage and the same binary SHA-256,
`6dae567aac5560620379bca2ac3ecf11e267c602f2818454cf4810621b5461d9`.
`scripts/check.sh` passed before the live run.

This is one bounded populated-folder move through the normal-build adapter.
It does not validate arbitrary subtree depth, a lost response, concurrent
edits, a FUSE cross-parent folder move or normal-account reliability. The
ordinary iCloud mount remains read-only.
