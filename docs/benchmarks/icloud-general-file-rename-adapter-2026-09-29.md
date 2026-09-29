# General iCloud file-rename adapter on an owned mount — 2026-09-29

Question registered before the live run: can an exact-ID, version-bound
`ICloudFileRename` that compiles without the validator feature replace the
fixture-only same-parent file rename in the shared mutation journal?

Prediction: a separate application creates and fsyncs one generated file and
an empty folder through an isolated FUSE mount. A second process renames only
that journal-confirmed file through FUSE. The adapter must check the exact
source ID, parent, ETag and complete SHA-256, reject a destination-name
collision, then send one conditional `renameItems`. An independent Apple
listing and full-byte read should show the same ID at the requested name,
with the old name absent. A third process remounts and reopens it without
another mutation. Transport classification must keep 429/5xx and incomplete
receipts uncertain rather than treating them as clean refusal.

Endpoints: one newly generated Cirrove-owned file, an Applied exact-ID
`Upsert` journal receipt, independent matching bytes and a successful fresh
remount. Only the isolated fixture may change; normal iCloud connections
remain read-only. Before each arm, a private manifest records command,
binary SHA-256, PID, expected duration and non-tmpfs filesystem for private
`TMPDIR`/`SQLITE_TMPDIR`. No compilation runs during the live arms. A passing
sequence does not establish real lost-response, concurrent-client or arbitrary
file-name reliability.

## Result

The controlled fixture `1ccd2655-f24d-4aaa-8b6b-0f899e758c14` passed all
three arms. The private manifests under
`.local-state/icloud-general-rename-run-eaada5f1-6007-4713-804d-e8f728af2f4f`
record the same binary SHA-256 and btrfs-backed temporary storage. Create
(PID 3990676, 107.0 seconds) produced one fsynced file and folder with
independently verified bytes and receipts. Rename through the general
adapter (PID 3992246, 13.7 seconds) recorded an Applied exact-ID Upsert;
independent Apple listing and complete content read found the same ID at
the requested name. A new process (PID 3992570, 34.8 seconds) remounted
and opened the renamed file without another mutation. The source name was
absent in both final checks.

The earlier full local check failed before this adapter existed because the
normal-build rename transport was unused. After the concrete adapter and
fixture routing were added, targeted iCloud tests and build passed. This
live result is one owned file and one clean response, not a production
reliability claim or permission to enable normal iCloud writes.
