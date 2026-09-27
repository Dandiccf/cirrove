# iCloud stale-ETag move-to-Trash experiment — 2026-09-27

Registered before the live run. Same-ID update and rename requests accepted
stale ETags on the validation account. Deletion through the Drive web
transport has not yet been exercised by Cirrove. This trial uses only a new
Cirrove-owned folder and a small file created within it. It does not delete
any pre-existing user file or empty Trash; the normal service stays read-only.

Question: Does `moveItemsToTrash` reject a stale ETag after the exact test
file's content and ETag have changed, or can it move a newer revision out of
the parent? The request is sent once and is not retried.

One arm: create and read back a small test file; save its initial item and
document IDs and ETag in process memory. Change its content using a
same-ID update and verify the newer bytes and ETag. Submit the **original**
ETag with the exact item ID to `moveItemsToTrash`, then re-list the exact
parent. If the response reports success and the ID is absent, classify stale
acceptance. If it reports refusal and the current ID, ETag and bytes are
intact, classify stale rejection. Anything else is indeterminate. No
comparative arm.

Prediction: based on the two other stale-ETag experiments, Apple may accept
the stale trash request. This is a prediction, not a claim. Absence from the
parent alone does not prove that the item is recoverable in Trash, and this
trial does not test an in-flight timeout, another account or concurrent
client requests.

The private manifest under `.local-state/icloud-stale-trash-validation/`
records command, binary hash, PID, expected duration and disk-backed temp
filesystem before the run. No credentials, signed URLs, raw response bodies
or file contents are recorded.

## Observed first run

The process completed in 65.9 seconds. After a confirmed same-ID content
update, `moveItemsToTrash` refused the original ETag. The exact file ID,
newer ETag and newer bytes remained in the test parent. This is one run,
with no within-arm spread. Because the request did not succeed, it does not
yet show that the endpoint can move a file when given the current ETag.
