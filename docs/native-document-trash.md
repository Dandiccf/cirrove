# Explicit native-document Trash

This experimental service workflow removes an original Pages document into
provider recovery. It is separate from file-manager `unlink` and `rmdir`:
generated package children and ordinary folders do not acquire this capability.
It does not add native editing, replacement, permanent deletion or a restore UI.

A daemon advertising `trash-native-document: 1` accepts an exact account,
mount-relative original document path, provider item ID and original ETag:

```sh
cirrove trash-native-document --label '<connection>' --account-id '<account-uuid>' \
  --path '<original.pages>' --item-id '<FILE::com.apple.CloudDocs::item>' \
  --etag '<original-etag>' --socket '<control-socket>'
```

The command returns an admission job promptly. Once queued, its progress carries
the durable operation UUID. Closing the command or stopping the job stops
observation; a queued removal remains retained and may complete. Uncertain
operations are inspected rather than automatically submitted again.

## Lost replies and restarts

If the initial reply is lost, do not submit another removal merely to discover
whether the first was queued. A daemon advertising `list-native-trash: 1` can
read retained operations for the exact account:

```sh
cirrove list-native-trash --label '<connection>' --account-id '<account-uuid>' \
  --limit 100 --json --socket '<control-socket>'
```

Each result includes the operation UUID, original item ID, document name,
original ETag and saved state. Match the original identity and revision, not
just a similar filename. Listing reads one bounded page. If `next` is present,
pass that value as `--after` for the following page, even when the current page
contains no operations. Older read-only journals may advance across a bounded
page of unrelated history without returning native operations. Listing never
enqueues a removal, wakes a mutation worker or retries an operation. Limits are
1–100.

Attach an observer to a selected retained operation using the separately
advertised `watch-native-trash: 1` capability:

```sh
cirrove watch-native-trash --label '<connection>' --account-id '<account-uuid>' \
  --operation '<retained-operation-uuid>' --socket '<control-socket>'
```

Watch requests have no path, revision, archive, retry or permanent-delete
arguments. They cannot enqueue a new operation.

## Recorded evidence, not current cloud state

The public fields `removal_receipt_recorded` and `metadata_absence_recorded`
mean that the exact recoverable removal and local metadata absence were recorded
for this operation. Successful observation requires both records. They are
historical evidence: a later restore or external change can make the document
visible again. Listing and watching do not claim to refresh its present cloud
state. A successful old operation must never be replayed to make current state
match that history.

This document describes the explicit API contract. Synthetic tests and a
successful build do not establish installed-provider reliability; controlled
normal-service acceptance and release gates remain separately recorded.
