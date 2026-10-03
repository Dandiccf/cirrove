# iCloud occupied-name staging experiment — 2026-09-27

Registered before the live run. The known iCloud web requests do not enforce
the stale content or rename ETags tested so far. A staged replacement would
first create a second item under a unique name and preserve both IDs. This
experiment asks what `renameItems` does when the destination name is already
occupied. It touches only two new Cirrove-owned files in a fresh isolated
validation folder; the normal service and all earlier fixtures remain alone.

Arm: create and independently read the original file; create a second file
under a unique staging name and independently read it. Submit one rename of
the staging item to the original item's name, with the staging item's current
ETag. Do not rename, trash or edit the original item. Do not retry after an
ambiguous response. No comparative arm.

Prediction: Apple will reject the collision or generate a distinct name.
An automatic replacement or deletion of the original would disqualify this
request from any non-destructive fallback. Endpoint: rejected with both exact
IDs and byte strings intact; accepted with two same-name IDs; original ID no
longer listed; or indeterminate. Leave the fixture for inspection.

The ignored `.local-state/icloud-occupied-name-validation/manifest.json`
records the binary hash, command, PID, disk-backed private temp filesystem,
expected duration and timing. No tokens, signed URLs, raw bodies or content
are logged.

## Observed first run and correction

The registered live run ended after 109.0 seconds with exit code 1 because the
probe's classification did not include Apple's actual result. It had created
and read both files separately. After the rename attempt, it still found both
exact IDs and read the original and staged bytes correctly, but neither the
unchanged staging name nor a duplicate original name matched its expected
branches. It correctly stopped without retrying.

A subsequent **read-only** listing of Cirrove validation folders with two
items showed the original `created-by-cirrove.txt` alongside the staged item's
new name `created-by-cirrove 2.txt`. Apple had assigned a conflict suffix.
The classifier was expanded to report this observed pair, but no second
mutation run was made. This is one account/session sample, without a
within-arm spread. It supports preservation of both items for this collision
case, not a conditional or atomic replacement protocol: the original file
still occupies the original name, and the earlier stale-ETag tests show that
the known rename request cannot serve as the required compare-and-swap.
