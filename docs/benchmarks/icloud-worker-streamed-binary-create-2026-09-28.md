# iCloud streamed binary Create through the shared worker — 2026-09-28

Registered before the live run. Scope is one fresh UUID-named Cirrove-owned
folder and one generated 20 MiB `.bin` file in the isolated
`iCloudGuiValidation` account. This does not touch an existing user item,
ordinary mount, other cloud client or permanent deletion.

Question: can the shared worker give its sealed payload descriptor to the
feature-gated iCloud Create adapter, stream content to Apple's allocated
slot in 64 KiB chunks, register the exact document ID and verify the
visible file's complete SHA-256 in 4 MiB version-bound read ranges before
issuing a durable `Uploaded` receipt? The previous 1 MiB arm used an
in-memory `Vec` for the content request and full readback.

One functional arm: create a new owned folder, generate and seal random-looking
bytes in a private journal, save the allocated ID and signed slot in the
credential vault, stream one content request, register once, read back exact
identity and all ranges, then independently reopen the journal. Prediction:
one `Uploaded` receipt for `Cirrove payload 20MiB.bin` with matching remote
size and content digest. A missing or uncertain result must leave the payload
for reconciliation; the request is never blindly replayed. Endpoint is the
worker state and independently reopened exact-ID/size receipt. One live run
cannot establish reliability, latency spread or process memory savings.

Before the run, a private manifest records command, binary SHA-256, PID,
expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR`; no parallel
compilation. No token, cursor, signed URL, raw provider body or file content
is logged. Normal iCloud GUI/FUSE stays read-only.

## Observation

The single live arm reached `Uploaded` for operation
`b381f56b-bb86-4d80-813f-79c8f8f35769`. The owned remote file has ID
`FILE::com.apple.CloudDocs::985886BF-D774-444B-95C4-6C82EE8131EC`, name
`Cirrove payload 20MiB.bin` and size 20,971,520 bytes. Before acknowledging
it, the adapter checked the allocated document identity and computed the
complete remote SHA-256 through five version-bound 4 MiB ranges. An
independent read-only reopening of SQLite found the one matching uploaded
receipt; its retained sealed payload has SHA-256
`beec86587066e35ac7d26874b98e2ac502e4dd136cdfcad06b1fef247b84ea8c`,
matching the journal record. The private btrfs run manifest records binary
SHA-256 `41072d90c1b4a3e506bf1df4276905bdceb4cadd63a547f03abb6c1e991c93ce`
and exit code zero. No memory maximum or repeatability was measured.

This establishes one functional streamed Create in an owned folder. It does
not validate a resumable content upload after transport loss, larger or
different iCloud item types, replacement, deletion, or normal mounted writes.
