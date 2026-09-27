# iCloud current-ETag Trash control — 2026-09-27

Registered before the live run. The prior one-shot test rejected a stale
ETag while preserving the newer test file. This control uses another fresh
Cirrove-owned folder/file and will send a current-ETag request only if it
first observes the same stale rejection and verifies the exact newer bytes.
The normal service stays read-only; no existing user file is touched and
Trash is not emptied.

Question: Does `moveItemsToTrash` accept the current ETag for a file whose
stale ETag it just rejected, moving that exact ID out of its parent?

One arm: create and byte-check a new file, perform a same-ID update, verify
the newer ETag/bytes, submit the original ETag once and verify refusal with
the newer version intact. Then submit the observed current ETag once. A
successful response plus absence of the exact ID from the parent counts as
current-ETag acceptance. A refusal with the same ID/ETag/bytes intact counts
as current-ETag rejection. All other results are indeterminate. No retry or
comparative arm.

Prediction: the current ETag will be accepted and the exact file will leave
the parent. This would establish one conditional Trash request shape on one
account, not Trash recovery, concurrency safety, timed-out request handling,
or release readiness.

The private manifest under `.local-state/icloud-fresh-trash-validation/`
records command, binary hash, PID, expected duration and disk-backed temp
filesystem before the run. No credentials, signed URLs, raw response bodies
or file contents are recorded.

## Observed first run

The process completed in 118.0 seconds. It verified a same-ID content
update, submitted the original ETag and observed refusal with the newer
ETag and bytes intact. It then submitted that current ETag once; the
response reported success and the exact file ID was absent from the parent.
These are one run per arm without a within-arm spread. The result supports
an ETag-conditional move-to-Trash request shape for this account. It does
not independently prove that the item is present or recoverable in Trash,
nor that an in-flight response loss can be reconciled safely.
