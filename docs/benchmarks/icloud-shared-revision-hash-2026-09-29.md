# Shared iCloud revision hash — 2026-09-29

Registered before the live run. This test changes only a new UUID-named
Cirrove-owned folder in the isolated `iCloudGuiValidation` account. Existing
user files, the normal iCloud mount, account settings and the installed daemon
remain untouched.

Question: after the normal-build iCloud adapters switch from per-range hashes
to one bounded complete download with metadata observations before and after,
can the mounted Create and exact-ID file rename still verify one generated file
and recover its name and bytes in a fresh process?

Arms: create one small file and one folder through the isolated FUSE mount;
rename the file in a second process; reopen the same run in a third process
without sending a mutation. Each arm must finish its journal work, list the
exact file independently through a separate Apple session and check full
bytes. Prediction: all three exit successfully with one file identity. A
failure is retained for diagnosis; no blind mutation retry is allowed.

Endpoints are application exit status, applied journal rows, exact remote ID
and complete bytes. This is one file and cannot prove general iCloud
reliability, large-file behavior, conflict handling or replacement. The
normal account remains read-only.

Each exclusive arm records its command, binary SHA-256, PID, expected duration
and disk-backed `TMPDIR`/`SQLITE_TMPDIR` in a private manifest before it runs.
No compilation or other measurement runs concurrently. No token, cursor URL,
signed URL or raw provider body is logged.

## Observation

One sequential live run completed all three arms with exit status 0. The
initial isolated mount created its owned fixture and verified it with an
independent iCloud listing and journal receipts. The second process verified
the conditional file rename the same way. The third process reopened the
fixture after remount and verified the renamed file and bytes independently.
The owned run identifier was `def195cd-1255-4465-9cc5-1b4f55d9bb15`.

The private manifests are in
`.local-state/icloud-shared-hash-measure-20260929/{initial,rename,fresh-read}/manifest.json`;
their temporary filesystems were `/home` on btrfs. The owned fixture and
journal remain under `.local-state/icloud-mounted-write-def195cd-1255-4465-9cc5-1b4f55d9bb15/`
for inspection. No ordinary iCloud connection was switched to write mode.

This single fixture supports the shared revision-hash change and the three
specific operations above. It does not close the replacement, conflict,
deletion or long-run reliability gates.
