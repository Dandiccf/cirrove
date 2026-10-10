# iCloud bounded binary Create through the shared worker — 2026-09-28

Registered before the live run. Only one new UUID-named Cirrove folder and
one new 1 MiB Cirrove-owned binary file inside it may be created in the
isolated `iCloudGuiValidation` account. The file is generated test bytes
named `Cirrove payload 1MiB.bin`. No existing user item, normal mount,
other client state or permanent deletion is touched.

Question: can the reserved-document-ID Create worker upload a 1 MiB binary
payload using the bounded 4 MiB path, register it as
`application/octet-stream`, read the full bytes back and publish the exact
ID, name, size and digest in the durable journal? The direct exploratory
probes remain limited to 4 KiB.

One arm: create the owned folder, seal the generated payload in the journal,
save Apple's allocated document ID in the credential checkpoint, send one
content upload and registration, then verify the exact ID and full bytes
before `Uploaded`. An uncertain result retains its payload for read-only
inspection; no request is blindly replayed. Prediction: an `Uploaded`
receipt with a 1 MiB binary file and the exact `.bin` name. One run has no
within-arm spread or latency/reliability claim. It does not prove 4 MiB at
the boundary, streaming of larger files, arbitrary MIME types, other
account classes or mounted writes.

Endpoint: exact-ID/full-byte receipt and independently reopened SQLite
record. A private manifest records command, binary SHA-256, PID, expected
duration and disk-backed TMPDIR/SQLITE_TMPDIR before the run. No token,
cursor, signed URL, raw provider body or file content is logged.

## Initial attempt and correction

The first process created its UUID-named test folder, then stopped before
enqueueing the binary payload: the probe still opened its isolated upload
journal with an 8 KiB quota. The error was a local pending-upload budget
refusal, before the Create worker or content request ran. No binary upload
or registration request was sent. Its manifest records binary SHA-256
`01a19cf1fd8d00d46a0284b4c115f1347aeabb281db1ea89dc481257a0d9ecd3`
and btrfs temporary storage. The empty test folder is left untouched.

Correction registered before the retry: keep the same question, prediction
and 1 MiB payload, but give this isolated journal an 8 MiB local quota.
The retry uses a new UUID-named owned folder and a new private manifest.
The failed first attempt is not counted as a successful upload arm.

## Observed retry

The corrected run reached `Uploaded` through the shared worker. The
feature-gated adapter checked Apple's allocated document ID, exact parent,
returned `.bin` name and complete 1 MiB byte stream before the receipt.
An independent read-only SQLite reopening found the matching operation
`uploaded`, a 1,048,576-byte remote receipt under the exact binary name,
and its retained local payload. The retry manifest records binary SHA-256
`14b35e33a449720bf3b22780eea1c950992bc64b3e5c96bd80deab60061f7221`
and btrfs temporary storage. This one success has no within-arm spread or
latency/reliability claim. Streaming and restartable writes beyond 4 MiB
remain open.
