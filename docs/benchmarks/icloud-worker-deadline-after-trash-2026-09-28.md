# iCloud worker deadline after accepted conditional Trash — 2026-09-28

Registered before the live run. Only one new UUID-named Cirrove folder and
two newly uploaded small owned files in isolated `iCloudGuiValidation` may
be changed. The old ID may remain recoverable in Trash. No existing user
item, normal mount, other client state or permanent delete is touched.

Question: after Apple accepts the conditional old-ID Trash request, can the
shared worker's 125-second provider deadline expire before the adapter
returns that receipt, then a fresh process reconcile the exact old ID and
finish the handoff without another Trash request?

Two sequential arms use the same fixture. The first holds the accepted
response inside the feature-gated adapter for 130 seconds, beyond the
worker deadline. Its `VerifyRequired` state must retain a checkpoint, local
payload and old-ID recovery reservation, with no fabricated success receipt.
The delay is reached only after Apple returned acceptance. The second arm
opens the same journal in a fresh process, inspects remote exact IDs and
full bytes, and may send the staged rename once if the old ID is verified
in Trash. It must not resend `moveItemsToTrash`. If the remote phase cannot
be established, it remains uncertain.

Prediction: the first worker pass times out at `VerifyRequired`; the second
process publishes exactly one current staged ID and one hidden, owned old
ID under Trash without Trash replay. This tests the worker's in-flight
deadline after Apple's response reached the adapter. It does **not**
simulate a network timeout before Apple responds or a server request still
in flight. One trial has no within-arm spread or latency claim and does not
establish repeatability, other-account behavior or mounted-write safety.

Endpoint: exact-ID/full-byte remote checks and reopened SQLite bindings.
Each arm records a private manifest with command, binary SHA-256, PID,
expected duration and disk-backed TMPDIR/SQLITE_TMPDIR before it starts.
No token, cursor, signed URL, raw provider body or file content is recorded.

## Observed

The first process reached the fault only after Apple's conditional Trash
request returned acceptance. Its worker deadline expired while the adapter
held the response, leaving `VerifyRequired`, a saved checkpoint and the
reserved old identity without a published success receipt. The fresh
process loaded that journal, verified the old exact ID and full bytes in
Trash, performed only the remaining staged rename, and published both IDs
without another Trash request.

An independent read-only SQLite opening found the matching operation
`uploaded` with a current receipt, one current remote-owned binding and one
hidden remote-owned old binding under the opaque Trash parent. Both private
manifests record binary SHA-256
`acda422224e2a41e4da0aebc419da81e5821b170961e96b161b6ab3093761e93`
and btrfs temporary storage. This is one trial with no within-arm spread or
latency claim. An actual network timeout before Apple responds remains
unverified; the test held an already accepted response inside the adapter.
