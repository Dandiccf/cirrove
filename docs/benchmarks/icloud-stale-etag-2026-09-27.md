# iCloud stale content revision experiment — 2026-09-27

Registered before the live run. One isolated `iCloudGuiValidation` invocation
creates a new root fixture folder and small file; it does not use any earlier
fixture or pre-existing user item. The normal iCloud mount stays read-only.

Question: Does `update/documents` reject a same-ID content save that supplies
the **old** ETag after a prior accepted update? The prior unchanged-base trial
proved same-ID replacement once, but did not exercise conflict behavior.

Arm: create and verify an owned file, perform one unchanged-base same-ID update,
verify its exact bytes and a new ETag, then send a second same-ID update carrying
the original stale ETag. The final candidate has the same byte length as the
current content. Independently list and read after the request. One request per
stage; no retry after an ambiguous response. No comparative arm.

Prediction: the web endpoint may ignore a top-level `etag` field and accept
the stale save. If so, Cirrove cannot use this request shape for conflict-safe
replacement. If rejected and the prior revision remains byte-for-byte intact,
that is positive evidence for this account, but a second-device race and lost
response still need their own trials.

Endpoint: accepted stale save with same ID and exact candidate bytes, rejected
save with unchanged newer ID/ETag/bytes, or indeterminate. The test fixture
remains in iCloud Drive. The run uses disk-backed private temp directories;
its ignored `.local-state/icloud-stale-etag-validation/manifest.json` records
the command, binary hash, PID, filesystem, expected duration and timing. No
tokens, signed URLs, raw responses or contents are logged.

## Observed first run

The pre-registered run completed in 78.7 seconds. The fixture was created and
read back; the first same-ID update succeeded with exact revised bytes. The
probe observed a new ETag and then sent a second same-ID update carrying the
original, now-stale ETag. Apple accepted it. The file kept the same
Drive/document ID, and an independent read returned the exact second candidate
bytes. Both payloads had the same length. This directly falsifies the proposed
conflict protection **for this request shape** on this account. It does not
prove every possible Apple request lacks a precondition. No existing user item
was touched. There is no within-arm spread from this single run.

Direct same-ID `update/documents` must not be wired to Cirrove's write journal.
The next candidate is a staged new-file upload followed by exact-ID,
conditional namespace operations that preserve the previous file. That route
changes the remote item ID and therefore also requires a durable journal
identity handoff and recovery proof before any writable mount.
