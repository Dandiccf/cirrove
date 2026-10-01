# Owned native Pages restore-path observation

## Preregistration

Question: does the exact successfully trashed Pages fixture from run
`97be33d2-b216-49bf-9e49-465b1ca85d1d` expose a bounded string
restorePath matching the owned parent and renamed document?

One read-only arm calls `--owned-package-restore-shape` with that run.
It requires the sealed v2 Recovered checkpoint and exact bound refusal evidence,
checks Trash identity/name/revision, downloads into private staging for semantic
verification, and checks unchanged metadata and source-only destination folder
before/after. No restore request exists in this arm.

Prediction: Apple returns a bounded slash-separated string as its public client
expects; exact name-form matching may fail and does not authorize restoration.
Only type/count/boolean observations are retained, never the raw restorePath.

The endpoint is successful bounded observation or a static refusal. This one
observation cannot establish destination race safety, reliability, or native
editing. The installed daemon is unchanged; no other compilation or measurement
runs concurrently. The launcher records binary hash and private disk-backed
TMPDIR/SQLITE_TMPDIR before starting.

## Local validation

17 package_trash_probe tests passed, including restore-shape bounded/private
output and synthetic HTTPS source/Trash observations. Feature probe build passed.

## Live result

The single `owned-restore-shape-live` arm passed at
2026-10-01T13:31:08.035350Z (exit 0). Apple returned a bounded string with two
segments, no leading/trailing slash, matching the exact owned parent and renamed
document. Metadata remained stable, package semantics matched the saved source,
and the original parent retained only the source fixture. No restore was sent.

Safe result retained in owned run `97be33d2-b216-49bf-9e49-465b1ca85d1d`,
`verify-61d47858-53b7-46a7-a7e4-a0c8ed0bbc61/restore-shape.json`; launcher evidence
in `.local-state/icloud-access-owned-restore-shape-live-2026-10-01/`.
This establishes the intended target for this fixture, not atomic vacancy
protection or successful restoration.

## Negative control

Removing the length/segment bound made the exact bounded-output test fail at
its excessive-input assertion (`restore-shape-bound-negative`, exit 101).
The original implementation was restored before the complete check.
