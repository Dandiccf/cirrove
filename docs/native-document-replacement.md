# Explicit native iWork archive replacement

This experimental workflow replaces one selected iCloud Pages, Numbers or Keynote PACKAGE using a
validated local ZIP archive. An active writable iCloud connection is required.
This command is an explicit replacement surface. The separate experimental
mounted-save path accepts edits to the exact canonical native archive; it does
not enable arbitrary edits to generated package children. A `.pages` filename alone is not evidence of
PACKAGE representation. For a wrapped archive, the source root and selected destination must
identify the same application format (`.pages`, `.numbers` or `.key`). Extension
classification ignores ASCII case; exact archive roots and remote names do not.
Explicit flat Numbers exports have a separate source layout. Numbers has bounded
live replacement, recovery, remount and Apple application evidence; Keynote
replacement and broader editing/format acceptance remain open.

Replacement creates a **new provider item ID** and retains a verified receipt
for the original in Trash. It does not preserve the original ID, promise revision
history or sharing-link continuity, or provide atomic replacement against other
cloud clients. Check the recorded acceptance limits before using this workflow.

## Submit one exact selection

A service advertising `replace-native-package: 1` accepts:

```sh
cirrove replace-native-package --label '<connection>' \
  --account-id '<account-uuid>' --path '<folder/original.pages>' \
  --item-id '<FILE::com.apple.CloudDocs::original-id>' --etag '<selected-etag>' \
  --archive '/absolute/path/replacement.zip' --source-root 'Replacement.pages' \
  --socket '<control-socket>'
```

Use the exact configured account UUID, mount-relative original path, provider
item ID and revision from the same selection. `--source-root` is the exact single
enclosing folder inside the ZIP, not the archive's filename. The source must be
an independent local archive outside cloud mounts and protected service state.
Current source admission is bounded to 64 MiB archive and expanded content with
10,000 entries; malformed, unsafe, duplicate or unsupported ZIP entries are
refused. A different source basename is allowed; it does not rename the selected
remote document. The optional `--socket` selects an isolated service explicitly.

An actual Numbers export with no enclosing document folder uses
`--source-layout flat-numbers` instead of `--source-root`:

```sh
cirrove replace-native-package --label '<connection>' \
  --account-id '<account-uuid>' --path '<folder/original.numbers>' \
  --item-id '<FILE::com.apple.CloudDocs::original-id>' --etag '<selected-etag>' \
  --archive '/absolute/path/edited-export.numbers' --source-layout flat-numbers \
  --socket '<control-socket>'
```

This selects the explicit flat Numbers contract; it does not infer a source
layout from the filename. The selected destination must already be a verified
Numbers PACKAGE. The raw source stays independently retained while Cirrove
derives and verifies a bounded transport ZIP that preserves the complete tree.

The command starts a tracked admission job, then follows it. Admission captures
and verifies the archive, independently verifies the original native content and
rechecks account, mount, path and revision before durable enqueue. Once queued,
progress carries its operation UUID. Success requires a typed two-identity
receipt and confirmed publication of the new document in Files. A resumed watch
also checks the replacement's current metadata against that receipt.

Stopping the watch, closing the terminal or losing its connection does **not**
roll back a queued replacement. Ctrl+C requests observation stop; durable work
remains retained and may complete. A stopped or failed observer is not permission
to submit the same replacement again.

## Lost replies and retained operations

If no reliable submission result arrives, discover saved operations first:

```sh
cirrove list-native-replacements --label '<connection>' \
  --account-id '<account-uuid>' --limit 100 --json --socket '<control-socket>'
```

The separately advertised `list-native-replacements: 1` interface returns one
bounded page. Limits are 1–100. Follow `next` using `--after <sequence>`, even if a
page is empty. Match the selected original item ID and ETag as well as the account,
not just a filename. Each entry includes operation UUID, saved state and original,
current and recovery identities when recorded. Listing is local-only: it does not
capture an archive, submit work, retry a mutation or contact the provider. An
enabled read-only account can expose retained journal records without a writer.

Attach to the chosen operation using `watch-native-replacement: 1`:

```sh
cirrove watch-native-replacement --label '<connection>' \
  --account-id '<account-uuid>' --operation '<retained-operation-uuid>' \
  --socket '<control-socket>'
```

Watching currently requires the active writable connection. It takes no archive
or retry option and never submits another replacement. If the state is uncertain,
conflicted or failed, retain the operation and its local recovery data for
inspection; watching is not an automatic repair or replay command.

`handoff_receipt_recorded` and the recovery identity describe historical evidence.
The original may since have been restored or deleted externally. Neither a list
nor a successful replacement watch proves that the old document is still in
Trash. No restore or permanent-delete action is added by these commands.

## Validation status

The [synthetic validation record](benchmarks/icloud-native-package-replacement-2026-10-01.md)
records actual worker/coordinator fault tests, encrypted checkpoints, publication
and retained discovery. Synthetic reconstruction is not an actual daemon crash
or Apple application acceptance. The [owned live arm](benchmarks/icloud-native-replacement-live-2026-10-01.md)
records the earlier attempt. The subsequent [owned v2 replacement](benchmarks/icloud-native-owned-v2-live-2026-10-01.md) completed with a typed receipt and mounted publication. Independent readback verified the edited current content and original recoverable Trash content, preserving the old proof and abandoned Stage without mutation replay. This is one controlled Pages fixture. The subsequent [Apple reopen check](benchmarks/icloud-native-replacement-apple-open-2026-10-02.md) opened the exact replacement ID in Pages and rendered its version-B marker; broader application acceptance remains open.
This guide describes implemented interfaces, not installed/full iCloud readiness.

The [fresh Numbers recovery arm](benchmarks/icloud-native-final-remount-before-apple-corrected-2026-10-06.json)
also verified genuine edited B and original A in Trash after final local
acknowledgement loss, recovering the same operation by inspection without replay.
Its [normal read-only remount](benchmarks/icloud-native-package-readonly-remount-before-apple-2026-10-06.json)
returned all 42 files and three previews before Apple opening. A
[separate Numbers open/reload](benchmarks/icloud-native-final-apple-after-remount-2026-10-06.json)
retained 17/3/20 and `SUM(A2:B2)` without edits or repair. This does not establish
native Linux editor saves, provider-reply loss or concurrent cloud replacement.
