# iCloud ordinary Unicode filename through the shared create worker — 2026-09-28

Registered before the live run. Only one new UUID-named Cirrove folder and
one new small Cirrove-owned file named `Résumé 2026 final.txt` inside it may
be created in isolated `iCloudGuiValidation`. No existing user item, normal
mount, other client state or permanent deletion is touched.

Question: after the Create worker stopped requiring UUID-pattern filenames,
can the existing reserved-document-ID flow register, read back and journal
a normal name with a space, Unicode and an extension? Does the exact
returned name remain intact in the durable receipt?

One arm: create a new validation folder, enqueue the small file, persist an
Apple-allocated document ID in the credential checkpoint, upload and
register once, verify the exact ID/full bytes, then reopen the journal to
check `Uploaded` and the returned name. If Apple rejects or the response is
uncertain, stop without blind registration replay. Prediction: one
`Uploaded` receipt retaining `Résumé 2026 final.txt`. There is no
comparative arm or repeatability/latency claim; one run has no within-arm
spread. This does not prove every filename, arbitrary existing parents,
large files, collisions, other account classes or mounted writes.

Endpoint: verified exact-ID/full-byte receipt and reopened SQLite record.
The private manifest is written before starting with command, binary
SHA-256, PID, expected duration and disk-backed TMPDIR/SQLITE_TMPDIR. No
credential, cursor, signed URL, raw provider body or file content is logged.

## Observed

One owned-fixture run reached `Uploaded` through the shared worker with
`Résumé 2026 final.txt`. The provider checked the saved document ID, exact
parent, returned name and full bytes before its receipt. An independent
read-only SQLite reopening found the matching operation `uploaded`, a
remote receipt with the exact Unicode/spaced name, and its retained local
payload. The private manifest records binary SHA-256
`d67fdd11c6538699b3a7fc406631053ecb12716f867ee47be700af5075e11cf2`
and btrfs temporary storage. This one result has no within-arm spread or
latency/reliability claim; it does not establish arbitrary-name coverage,
existing-parent safety, collisions, large files or mounted writes.
