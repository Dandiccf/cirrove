# Native Trash metadata publication

Prepared implementation and synthetic tests; no test or live execution by this
patch's author. This is additive to the unreleased schema16 native Trash slice.

An Applied native Trash receipt creates a durable metadata-publication row in the
same SQLite transaction through a trigger. Writable reopen backfills older Applied
native rows; completed publication stays completed. Ordinary removals are excluded.
The original mutation request/receipt is never rewritten by publication.

A separate bounded worker queries one indexed due row and calls the existing
`Engine::refresh_node` for the exact account/collection/item. It does not request
file content, mutate the provider, traverse generated children, or modify held
reader streams and local content files. Network awaits hold no journal lock.
Only `NotFound` returned after Engine's ordered absence publication marks metadata
removed. A newer observation superseding that absence retains its visible node.

Pending and Present are not removal success. Present stores the latest exact-ID
active observation and remains due with exponential cooldown capped at 60 seconds;
this handles delayed visibility without hiding a later restored document. Provider
failure retains visible metadata and retries. Absent is an acknowledged observation,
not a permanent assertion that another actor can never restore the item. A resumed
public observer needing current visibility must refresh rather than reinterpret an
old acknowledgement as a new provider observation.

`WriteControl::native_trash_publication(id)` exposes the existing bounded
`PackagePublicationStatus` value. Admission's status uses `metadata_removed=true`
only for Absent together with an exact confirmed native removal receipt. Receipt
alone is not metadata success. No user-facing command is enabled here.

## Prepared validation

Five tests cover receipt-before-publication visibility; no content/list calls;
failed/active observations with persisted cooldown and restart; delayed absence
superseded by a restored node; wrong callback identity/receipt refusal; and migration
backfill without receipt rewrite or duplicate completed work. Existing fake provider
asserts its journal mutex is available during every exact-ID network call.

Negative controls: treat any refresh result as Absent to break failure/newer-node
arms; skip Engine refresh and publish receipt-only absence to break the initial
visibility/read-count assertions; remove triggers/backfill to break due/restart
assertions; accept a changed expected request to break the stale callback test.
Actual open-FD/native archive survival remains part of the full service/kernel
acceptance, not a claim proved by these metadata-only fixtures.

## Parent validation

All nine targeted native-trash service tests passed, including five publication
tests (`native-trash-publication-tests`). Treating an observed active node as
Absent made the restart/visibility test fail (`native-trash-publication-negative`);
the exact original implementation was restored. These are synthetic service
tests; held-reader kernel acceptance remains a separate pending check.
