# Bounded iCloud Trash metadata inspection — 2026-09-27

Registered before the live run. The prior owned-fixture test proved that a
current-ETag request removed a file's exact ID from its parent, but did not
establish Trash membership or recovery. This read-only request inspects the
special Trash root through Cirrove's own saved validation session. It never
uses another client's mount, configuration or credentials.

Question: Does the special Trash root return a bounded, complete item array
with exact item IDs and `restorePath` metadata that could support a later
owned-fixture recovery test?

One arm: request `FOLDER::com.apple.CloudDocs::TRASH_ROOT` once with
`partialData: false`, cap the response at Cirrove's 8 MiB JSON limit and
90-second listing timeout, check the returned root ID and every item ID,
and compare `numberOfItems` with the parsed item count. Record only the
count, completeness boolean and number with `restorePath`; no names, IDs,
raw bodies or URLs enter the artifact. No comparative arm.

Prediction: Apple will return a complete special folder with at least one
recoverable item, but a missing count or oversized response must be
reported as a limitation rather than presumed complete. One snapshot does
not establish consistency under concurrent Trash changes.

The private manifest under `.local-state/icloud-trash-listing-validation/`
records command, binary hash, PID, expected duration and disk-backed temp
filesystem before the process starts. No credentials or raw provider data
are recorded.

## First attempt and correction

The first read-only request completed in 92.1 seconds but the probe rejected
the returned special-root `drivewsid`: its check accepted only the prefixed
request ID. Other iCloud clients treat the returned `TRASH_ROOT` short ID as
the special root. The parser now accepts only those two explicit forms and
still rejects every other root identity. No item content or raw response was
logged. The first attempt established neither count nor completeness; the
corrected probe requires a separate run.

## Corrected read-only run

The corrected process completed in 31.4 seconds. The special root returned
one item, `numberOfItems` matched the parsed count, and that item had a
non-null `restorePath`. No item identity, name or content was recorded. This
is one complete bounded snapshot, not a consistency or pagination test.
