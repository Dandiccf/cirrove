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

## Desktop

Expand an active account that offers local recovery and choose **Save a local copy…** under
**Recover a saved version**. The picker lists eligible versions among the latest
200 saves, identified by filename, generation number and byte length. Choose a
new local destination; existing destinations are still refused even if the
platform chooser offers replacement. The progress dialog can request **Stop**.
Only a matching completed service receipt produces a success message. Missing
jobs, disconnected services and incomplete receipts report an unconfirmed result
and ask you to inspect the destination. These outcomes do not discard the save.

For a configured **disabled and unmounted** account, expand its row and choose
**Save a local copy…** under **Recover local changes**. This works without the
daemon or an Apple sign-in. The picker distinguishes saved versions from working
files and offers **Next page** when more local records remain. Pages scan up to
200 saves and 200 working files; clean or completed records can leave a page
without an exportable entry. Select a version and a new local destination.

The offline copy runs in a blocking worker with a cancellable progress dialog.
The account identity and exclusive owner locks are checked again after selection.
Working files get a separate completion message: a process may have stopped in
the middle of writing them. Neither success nor cancellation seals, uploads or
discards these bytes. A changed account, changed generation or mismatching receipt
cannot produce a confirmed-success message.

## Recovery after switching to read-only

A current daemon also offers saved and working recovery for an enabled read-only
account. It opens the retained journal read-only, without starting write workers,
replaying operations or sealing bytes. A missing journal produces an empty list;
a busy or damaged journal is reported as unavailable, not silently empty.

Use the same active `export-save` and `export-working --active` commands. The
desktop displays **Save a local copy…** when the service advertises this capability.
Retry, discard and keep-both actions remain unavailable while read-only. Local
recovery never grants cloud write access. Names come from retained local metadata;
an item identity is shown only if no local name remains.

## Recovery while the account is unmounted

For an existing **disabled** account, the CLI can recover sealed versions without
starting the daemon, mounting the account or opening its credentials:

```sh
cirrove recovery-saves --label NAME
cirrove export-save --offline --label NAME --operation SAVE_UUID \
  --destination "$HOME/Documents/Recovered copy.txt"
```

`recovery-saves` returns bounded JSON metadata (operation ID, sequence, name,
state and size). Use `--after LAST_SEQUENCE` for the next page; pages include
terminal history so pagination does not skip past it. A page contains at most
200 records. The export still accepts only unresolved sealed generations.
`--state PATH` selects an isolated state directory for both offline commands.
`Ctrl+C` requests cancellation during a copy.

Offline recovery holds the account operation, account owner and journal owner
locks, refuses enabled or still-stopping accounts, and opens only the existing
journal schema in SQLite read-only mode. It does not migrate the journal, rewrite
interrupted upload states, seal working files or start reconciliation. It refuses
symlinked journal/database/object locations and destinations inside the state or
configured mount paths. Older/newer journal schemas are refused explicitly.
The regular active-account command continues to use the daemon.

## Recover unsealed working files

The experimental offline CLI also recovers bytes written into Cirrove's working
files before a save was sealed, including retained writes to unlinked files:

```sh
cirrove recovery-working --label NAME
cirrove export-working --label NAME --file FILE_UUID --generation GENERATION \
  --destination "$HOME/Documents/Recovered working copy.txt"
```

Both commands require the account to be disabled and its journal unused. Both
accept `--state PATH`. The list returns JSON `files` and `next`; pass a non-null
`next` as `--after UUID` and repeat until `next` is null. Up to 200 records are
scanned per page. A page can have no files but a non-null cursor because clean
working records are skipped. Metadata includes actual byte size, last recorded
size, generation, and whether the pathname was removed. No file contents are
read to populate this list.

Copy the file ID and generation from that listing. Export refuses a changed
generation, keeps the exclusive owner lease through publication, and never seals
or queues an upload. The JSON receipt identifies the recovered working version
and its SHA-256. This digest describes the bytes recovered from disk; unlike
`export-save`, it is **not** checked against a previously sealed save. Following
an interrupted write the bytes can be partial and their size can differ from the
journal metadata. Recovery preserves those actual bytes instead of silently
truncating, padding, or uploading them. Editor buffers never written to Cirrove
cannot be recovered this way.

The same private, cancellable, no-overwrite local destination rules apply.
Unexpected source size/timestamp changes during copying abort publication;
`Ctrl+C` requests cancellation. The source and journal remain unchanged.

## Working bytes from a running account

Use the explicit `--active` mode to recover working bytes without disabling the
connection or closing the application's file:

```sh
cirrove recovery-working --active --label MyDrive --limit 200
cirrove export-working --active --label MyDrive --file FILE_UUID --generation GENERATION --destination /absolute/local/copy
```

Use the UUID and generation from the listing. `--after UUID` follows its `next`
cursor; a page may contain no eligible files and still provide a next cursor
because clean working records are skipped. Listing reads local metadata only.
`--socket PATH` selects an isolated daemon and requires `--active`. Omitting
`--active` keeps the existing disabled-account offline workflow; `--state` cannot
be combined with active mode.

The daemon stages a private copy outside the journal lock, then checks that the
selected generation did not change. A concurrent edit before that check refuses
the export without publishing a destination. Later edits may coexist with the
already verified copy; its receipt names the selected generation, not the latest
version. The CLI waits for an exact UUID/generation/size/destination receipt and
requests cancellation on Ctrl+C. A missing job or missing receipt is not success.
The mount and application file remain usable. No export seals bytes, retries a
cloud operation or queues a new upload.

The [core tests](benchmarks/icloud-active-working-export-core-2026-10-01.md) and
[service/CLI fixture](benchmarks/icloud-active-working-export-service-2026-10-01.md)
cover the separate boundaries. The [desktop picker](benchmarks/icloud-active-working-export-desktop-2026-10-01.md)
also offers these active working versions alongside saved versions. It labels
unfinished edits and validates the same exact receipt before reporting success.

## Current limits

- Desktop recovery supports active writable journals and configured disabled
  accounts, including active unsealed working bytes. Removed/retired accounts
  are not wired into the desktop yet.
- `export-save` selects an unresolved **sealed generation**, not unsealed working
  bytes; `export-working` handles retained working files in offline or explicit
  active mode. `Preparing`, acknowledged, discarded and resolved
  generations are refused. Selecting an older ID exports that older generation.
- Active-account desktop selection is bounded to the latest 200 sealed saves;
  active working-file metadata and the offline picker support further pages. The CLI can select an
  older known operation ID. The export dialog monitors the current operation;
  after closing/restarting the app, use CLI jobs/status to inspect retained jobs.
  Export jobs are not shown as offline pinning downloads.
- A directory renamed during copying is detected before publication. As with any
  file operation, another process can rename the completed file afterward.
- These controls do not establish ordinary iCloud write readiness; see
  [the iCloud acceptance gates](icloud-write-integration.md).
