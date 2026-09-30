# Export a retained local save

`cirrove export-save` copies one immutable saved generation from an active
account's local journal. It does not retry, acknowledge, discard or resolve the
cloud operation. This is useful before deciding how to handle a conflict or an
uncertain upload. It works through the account-neutral journal, including the
experimental iCloud writer.

Find the operation ID in `cirrove recent --label NAME --limit 200` (the `[save …]`
value), then choose a new filename outside every Cirrove mount:

```sh
cirrove export-save --label NAME --operation SAVE_UUID \
  --destination "$HOME/Documents/Recovered copy.txt"
```

The destination's parent must already exist. Existing files, directories,
symlinks, symlinked ancestors, `..` traversal, the source journal and FUSE targets
are refused. The service also rejects known Cirrove mount paths and the active
account's state directory. An export never overwrites an existing name, even if
it appears while copying. Destination names are passed as structured paths,
not shell command strings.

The service immediately returns a job; the CLI waits for its explicit completion
receipt with operation ID, byte length and SHA-256. `cirrove jobs NAME` reports
progress. `cirrove stop NAME --id JOB_ID` requests cancellation. Only one recovery
export runs at a time. The completed receipt remains in the bounded job history
for up to ten minutes (or until dismissed/evicted); if the service or result is
lost, the CLI reports uncertainty and tells the user to inspect the destination.
It never equates a missing job with success.

The selected descriptor pins the original immutable generation while copying
outside the journal lock. Copying uses a 128 KiB buffer, checks size and SHA-256,
fsyncs the temporary file, publishes without replacement, then fsyncs the parent.
Cancellation or integrity failure leaves no normal destination filename. An
abrupt process exit can leave a hidden `.cirrove-export-*` temporary file, or a
complete destination if publication already happened; the original journal is
unchanged. No automatic cleanup walks the destination directory.

## Current limits

- The account must have an active writable journal. Export from an unmounted or
  retired account is not wired to this command yet.
- It selects an unresolved **sealed generation**, not unsaved editor buffers or
  unsealed working bytes. `Preparing`, acknowledged, discarded and resolved
  generations are refused. Selecting an older ID exports that older generation.
- The CLI and status protocol expose this flow; a dedicated desktop export picker
  and progress view are still pending. Export jobs are excluded from the offline
  pinning section rather than mislabeled as downloads.
- A directory renamed during copying is detected before publication. As with any
  file operation, another process can rename the completed file afterward.
- These controls do not establish ordinary iCloud write readiness; see
  [the iCloud acceptance gates](icloud-write-integration.md).
