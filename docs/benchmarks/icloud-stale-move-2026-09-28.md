# iCloud stale-ETag move, 2026-09-28

## Registered before live arm

Question: Does Apple's `moveItems` reject a Cirrove-owned file whose ETag
became stale after a confirmed same-ID content update? The tested
`renameItems` shape accepted stale content and metadata revisions, so move
cannot inherit rename's presumed conditional behavior.

The probe creates two new UUID-named `Cirrove Write Validation-*` folders in
the isolated validation account. It creates a small exact-ID file in the
source, confirms a newer same-ID content revision, then submits **one**
`moveItems` request with the original ETag and the empty destination folder
ID. No user file, normal mount or installed service is changed. The request
shape follows rclone's [iCloud Drive web transport](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/api/drive.go#L2753-L2819),
but Cirrove never invokes rclone.

Endpoint if accepted: the exact file ID and document ID are absent from the
source and present exactly once in the destination with the newer bytes.
Endpoint if rejected: the newer exact ID, ETag, name and full bytes remain in
the source and the destination remains empty. Any other state is
indeterminate; no move retry or fresh-ETag retry follows it. A separate
control fixture would be needed if the stale request is rejected.

Prediction: `moveItems` may reject the stale revision because rclone handles
an `ETAG_CONFLICT` response, but its source also offers a forced retry with the
ETag returned by Apple. `renameItems` accepted stale revisions in this same
account. Neither behavior proves this endpoint's semantics; the one live arm
decides only this request shape on this fixture. A conditional general Move
would still need lost-response, destination-collision and concurrency tests.

The private manifest records command, binary SHA-256, PID, expected window,
start/completion times and btrfs-backed private `TMPDIR`/`SQLITE_TMPDIR`.
Only outcome categories are printed, never provider bodies or URLs.

## Results

The stale-ETag arm passed its **rejection** endpoint. After confirming the
newer same-ID contents, `moveItems` refused the recorded old ETag. The exact
file ID, current ETag, name and full newer bytes remained in the source, and
the destination remained empty. No retry was sent. Private manifest:
`.local-state/icloud-stale-move-aa49c154-6e5c-4d79-8f56-bdf95a6cc07d/manifest.json`.
It records binary SHA-256
`bc74d70905868da10a443b093d00189077a2f255a3c9e2409c9ea6e052cd3229`,
PID 3087978, btrfs-backed temporary directories, start
`2026-09-28T14:45:17.516900+00:00`, completion
`2026-09-28T14:47:41.021847+00:00`, and exit 0. The 2m23s duration is one
observation, not a performance claim.

## Registered fresh-ETag control before the second arm

The control creates another independent pair of new UUID-named folders and a
small exact-ID file, then sends one `moveItems` with its current ETag. It
accepts success only when the exact file and document IDs and full bytes are
absent from the source and present once in the destination. Rejection is
accepted only when the same ID, ETag and bytes remain in the source and the
destination is empty. Any other state remains indeterminate; the request is
never retried. Prediction: the fresh request succeeds, separating ordinary
move support from the stale rejection above. The second arm still does not
cover stale metadata ETags, destination collisions, response loss, concurrent
clients or folders.

The fresh-ETag control passed. The one move request was accepted. The exact
file ID and document ID, original filename and complete bytes were absent
from the source and present once in the destination. Private manifest:
`.local-state/icloud-fresh-move-27cf7296-b4ee-426a-a78f-d9fd16bf6073/manifest.json`.
It records binary SHA-256
`617048ca9bbd676b74d8161bfc9c005b3d5312c7f37653d99765465484cd3030`,
PID 3093178, btrfs-backed temporary directories, start
`2026-09-28T14:49:49.603803+00:00`, completion
`2026-09-28T14:51:42.693076+00:00`, and exit 0.

Together the two single arms show this account's `moveItems` request shape
rejecting one content-stale ETag and accepting a current ETag for an ordinary
small file. They do not yet show metadata-stale rejection or safe mounted
reconciliation. No normal iCloud move is enabled.

## Registered metadata-stale arm before the third run

A third, independent pair of new validation folders gets a small exact-ID
file. A current-ETag `renameItems` changes only its name and establishes a
new ETag while preserving its ID and full bytes. The probe then sends exactly
one `moveItems` using the **pre-rename** ETag. Rejection passes only when the
renamed exact ID, newer ETag and bytes remain in the source and the destination
is empty. Acceptance passes only as a characterized unsafe result when that
exact ID and bytes appear once in the destination. Any other state is
indeterminate and no request is retried.

Prediction: this endpoint may reject metadata-stale ETags as it rejected the
content-stale ETag, but `renameItems` accepted both classes of stale ETag in
prior trials. Therefore this arm must be observed rather than inferred from
the first two. One arm cannot establish general concurrency safety.

The metadata-stale arm passed its rejection endpoint. After the fresh-ETag
rename had changed only the name and ETag, `moveItems` refused the original
ETag. The renamed exact file and document IDs, newer ETag and full bytes
remained in the source; the destination stayed empty. No retry was sent.
Private manifest:
`.local-state/icloud-metadata-stale-move-2dd84f29-9282-416f-8373-8a46648417ed/manifest.json`.
It records binary SHA-256
`fc6ff54e6102bf0ac6eb04e8cae19d1afb7d700545b5887fa3ae78be24cea3db`,
PID 3097933, btrfs-backed temporary directories, start
`2026-09-28T14:54:05.795503+00:00`, completion
`2026-09-28T14:56:29.443404+00:00`, and exit 0.

These three single arms support conditional `moveItems` behavior for one
ordinary file on one account: current ETag accepted; content-stale and
metadata-stale ETags rejected. They do not prove repeatability or safe
mounted Move. A destination name collision, uncertain response, process
death, folder move and simultaneous external edit remain open.
