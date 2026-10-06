# Explicit native-document Trash

This experimental service workflow removes an original Pages, Numbers or Keynote PACKAGE document into
provider recovery. It is separate from file-manager `unlink` and `rmdir`:
generated package children and ordinary folders do not acquire this capability.
It does not add native editing, replacement, permanent deletion or a restore UI.
Standalone Numbers/Keynote removal is still under development validation;
Numbers replacement evidence does not establish this separate removal path.
A matching suffix alone does not establish
PACKAGE representation or authorize removal.

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
The development implementation can observe retained operations after reconnecting
read-only. It uses local journal evidence, without constructing a mutation worker.
An unconfirmed removal stays pending or requires review; observing it does not
send or repeat the removal. This remains separate from installed acceptance.

## Recorded evidence, not current cloud state

The public fields `removal_receipt_recorded` and `metadata_absence_recorded`
mean that the exact recoverable removal and local metadata absence were recorded
for this operation. Successful observation requires both records. They are
historical evidence: a later restore or external change can make the document
visible again. Listing and watching do not claim to refresh its present cloud
state. A successful old operation must never be replayed to make current state
match that history.

A [local actual-process trial](benchmarks/icloud-native-trash-receipt-loss-guard-2026-10-06.json)
now covers receipt loss before local acknowledgement and inspection-only recovery
of the same operation against a TLS fixture. Live standalone Numbers acceptance
still requires its own controlled removal, independent Trash-content verification
and public read-only observation.

The [first separate real-account attempt](benchmarks/icloud-native-trash-numbers-receipt-loss-2026-10-06.json)
stopped at the test controller's startup status guard before folder creation,
import or Trash admission. No cloud-file mutation was dispatched; provider reads
may have occurred and were not counted. All original child handles were reaped,
the residual owned mount was closed in a separately recorded unmount, and the
final source and installed baselines matched. This attempt exercised none of
the receipt-loss recovery or Trash-content endpoints and will not be replayed.

A [fresh successor](benchmarks/icloud-native-trash-numbers-receipt-loss-attempt2-2026-10-06.json)
passed startup and created one owned test folder, then stopped at the import-job
guard. Its journal contains the completed folder creation, no upload and no
native removal. The daemon exited normally; all original child handles were
reaped and the mount/socket closed without a separate manual unmount. Source,
executable and installed baselines matched. The failing guard operands were not
retained, so the precise import failure is unproved. Neither failed attempt
establishes live standalone removal acceptance or is automatically repeated.

This document describes the explicit API contract. Synthetic tests and a
successful build do not establish installed-provider reliability; controlled
normal-service acceptance and release gates remain separately recorded.
