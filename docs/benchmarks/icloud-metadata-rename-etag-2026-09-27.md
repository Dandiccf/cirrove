# iCloud metadata ETag rename experiment — 2026-09-27

Registered before the run. The previous conditional-rename trial used an
ETag made stale by a **content** update; Apple accepted it. This trial tests
whether a rename changes a separate metadata revision that `renameItems` can
enforce. It uses only a fresh Cirrove-created folder and file in the isolated
validation account. The normal mount remains read-only.

Arm: create and read a small owned file; rename it once using its current
ETag; verify the same ID, exact content and a new ETag. Then request a second
rename using the original stale ETag. If rejected with the first name and
bytes intact, request it once with the current ETag. No request is retried.
There is no comparative arm.

Prediction: the metadata-only stale ETag may produce `ETAG_CONFLICT` even
though the content-derived stale ETag did not. A stale rejection followed by
a fresh success would support a conditional namespace operation, but still
would not protect content replacement or make a staged path swap atomic.

Endpoint: stale accepted, stale rejected/fresh accepted, both rejected, or
indeterminate, all checked by independent listing and exact readback. The
fixture remains in iCloud. The ignored manifest at
`.local-state/icloud-metadata-rename-validation/manifest.json` records the
binary hash, command, PID, disk-backed temp filesystem, expected duration and
timing. No tokens, signed URLs, raw provider bodies or contents are logged.

## Observed first run

The pre-registered run completed in 76.5 seconds. The first rename kept the
exact item ID and bytes and exposed a new ETag. `renameItems` nevertheless
accepted a second rename with the ETag from before the first rename. The
second name was independently visible under the same ID; the exact content
bytes remained readable. This request shape is **not** an enforced metadata
compare-and-swap on this account. The fixture remains in iCloud Drive. A
single run has no within-arm spread and does not rule out other undocumented
Apple precondition mechanisms.
