# iCloud conditional rename experiment — 2026-09-27

Registered before the live run. One isolated account invocation creates its
own new folder and file, updates that file once under the same ID to obtain a
new ETag, then tests `renameItems` against **only** that item. Existing user
files, other clients and the normal Cirrove service are untouched.

Question: Does `renameItems` reject the previous ETag while accepting the
current ETag, preserving the same item ID and exact content bytes? A positive
result would support one conditional namespace primitive for a staged
replacement route. It would not make the path swap atomic or solve durable
journal identity handoff.

Arm: submit one rename with the original stale ETag. If and only if it is
rejected and the current name, ID, ETag and bytes remain, submit one rename
with the current ETag. There are no retries and no comparative arm.

Prediction: the stale rename will return `ETAG_CONFLICT` or another refusal;
the fresh rename will succeed. The endpoint is a classification of stale
accepted, stale rejected/fresh accepted, both rejected, or indeterminate,
after independent listing and exact readback. The fixture remains in iCloud.

Use a disk-backed private temp directory. The ignored
`.local-state/icloud-rename-validation/manifest.json` records command, binary
hash, PID, filesystem, expected duration and timing. Do not record tokens,
signed URLs, raw response bodies or file content.

## Observed first run

The pre-registered run completed in 79.2 seconds. After the first same-ID
content update changed the listed ETag, `renameItems` nevertheless accepted
the ETag from before that content update. Independent listing showed the new
name under the same item ID, and independent reading retained the exact bytes.
This request shape does **not** make a content revision a rename precondition
on this account. It remains possible that Apple uses a separate metadata
revision for conditional namespace operations; the next controlled trial must
make a first rename and then reuse the ETag from before that rename. This
single run has no within-arm spread.
