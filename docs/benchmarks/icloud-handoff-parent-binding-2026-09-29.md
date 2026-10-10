# iCloud replacement parent binding — 2026-09-29

Registered before the live run. This check changes only a new UUID-named
Cirrove-owned folder in the isolated `iCloudGuiValidation` account. It does not
alter the installed daemon, ordinary mount, credentials, or existing user files.

Question: after requiring the replacement handoff to observe its exact parent,
folder ID and name, does an isolated mounted create/replacement still finish
and recover in a fresh process? The older version-2 journal format must remain
readable as a root-owned fixture; version 3 rejects a missing or retargeted
parent. The latter boundary has a synthetic test, not live nested replacement
evidence yet.

Arms: create one small file via an isolated FUSE mount, replace it via an
application save in a second process, then remount and read the replacement
in a third process. Each arm must exit successfully and independently verify
the exact cloud IDs and bytes. Prediction: all three pass, with the original
recoverable by exact ID in Trash and the new ID visible at the original name.

Endpoints are exit status, applied journal rows, exact remote IDs and full
bytes. One fixture does not establish ordinary-account safety, arbitrary
folder replacement, conflict reliability or long-run behavior. The normal
iCloud connection remains read-only.

Each exclusive arm records a manifest with command, binary SHA-256, PID,
expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR` before execution.
No build or other measurement overlaps an arm. No token, cursor URL, signed
URL or raw provider response is logged.

## Observation

All three sequential arms exited 0 for owned run
`b22d481a-5f73-4dd4-a5a9-cb12d70537cb`. The initial mount created the
file and verified it through an independent iCloud listing and journal
receipts. The second process completed a two-ID replacement, independently
verified the new visible ID and original Trash ID, and retained the journal.
The third process remounted and verified the replacement name and bytes.

The private manifests and disk-backed btrfs temporary directories are under
`.local-state/icloud-parent-measure-20260929/{initial,replace,fresh-read}/`.
The owned fixture and journal remain under
`.local-state/icloud-mounted-write-b22d481a-5f73-4dd4-a5a9-cb12d70537cb/`
for inspection. No normal iCloud connection was changed.

This supports compatibility of the stricter parent observation with the
existing root-owned replacement. Nested version-3 plans have only synthetic
validation; they have not performed a live nested replacement. One run does
not close conflict, restart-during-handoff, broader folder or reliability
gates.
