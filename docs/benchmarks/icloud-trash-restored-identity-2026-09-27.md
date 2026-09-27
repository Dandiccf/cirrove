# iCloud restored item identity — 2026-09-27

Registered before the live run. A prior owned-file restore request was
accepted and one entry returned to the original folder, but a combined
metadata check failed. This run creates another fresh Cirrove-owned folder
and file and classifies kind, Drive ID, document ID, name and full bytes
separately after a single Trash/restore cycle. No existing user item or
other client's state is touched.

Question: Which identity or presentation field changes when Apple's
`putBackItemsFromTrash` returns the owned file, and are the bytes intact?

One arm: repeat the exact-ID and complete-Trash-list safeguards; send one
current-ETag Trash request and one restore request. Require one item in the
original folder. If it is an ordinary file, read it by its returned exact
Drive ID and compare all bytes before classifying differences in document
ID and name. No retry, no comparative arm. Only fixed result classes are
printed, never names, IDs, raw bodies or content.

Prediction: the restored bytes will match but Apple will have changed the
document ID or name. A single run cannot establish stable restore semantics
or crash/restart behavior.

The private manifest under `.local-state/icloud-trash-restored-identity/`
records command, binary SHA-256, PID, expected duration and disk-backed
temporary filesystem before the process starts.

## Observed first run

The process completed in 156.3 seconds. The exact test ID left its parent,
then appeared in a complete Trash listing with an ETag and restore path.
Apple accepted one restore request. Exactly one ordinary file returned to
the original folder; its document ID and complete byte sequence matched
the original, but its displayed name differed. The probe did not print or
persist the returned name or Drive ID, so it cannot yet say whether the
Drive ID changed or why the name changed. This is one run without a
within-arm spread. A restore must use the provider's actual resulting name
instead of promising the previous pathname.
