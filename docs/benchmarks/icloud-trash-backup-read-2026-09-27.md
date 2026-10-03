# iCloud Trash backup readability — 2026-09-27

Registered before the live run. Only one newly created small Cirrove-owned
file in a new UUID-named validation folder of `iCloudGuiValidation` may be
moved to recoverable Trash. The ordinary mount remains read-only. The file is
not permanently deleted or restored by this trial.

Question: after an ETag-bound `moveItemsToTrash` succeeds, can Cirrove still
download that exact file ID and verify its complete original bytes while a
complete Trash listing retains its ETag, size and restore metadata? This is
a prerequisite for using Trash as the old-version recovery location in a
safer replacement protocol.

One arm: create and read back a bounded ordinary file; send one Trash
request with its current ETag; require its exact ID to leave the parent and
appear in a complete Trash listing with restore metadata. Request a signed
download for that ID without logging the URL, bound the response to 4 KiB,
compare all bytes to the original and re-list Trash to require the same ETag
and size. No comparative arm and no request replay.

Prediction: Trash may preserve the exact ID and restore metadata but its
download-by-ID endpoint may refuse content; the outcome is unknown. One run
has no within-arm spread and cannot prove cross-account behavior, an in-flight
request's outcome or concurrency safety. A successful read alone would not
enable writes or settle how the shared journal represents a Trash backup.

Endpoint: exact-byte equality and stable Trash metadata, or the sanitized
failure class. A private manifest records command, binary SHA-256, PID,
expected duration and disk-backed temporary storage before execution. No
credentials, signed URLs, raw provider bodies or file bytes enter this
artifact.

## Observed

The one owned file was accepted into recoverable Trash. Its exact Drive ID
appeared in a complete Trash listing with restore metadata, size and ETag.
`download/by_id` provided its complete original bytes; a second complete
Trash listing retained the same ETag, size and restore metadata. The private
manifest records binary SHA-256
`8334d86cc52e48f3931647c6cdcffa61abf985fd22629bbccd2c1ef2af0b2b0a`
and btrfs temporary storage. This is one account and one run, with no
within-arm spread. It establishes that a small old version can remain
byte-readable under its exact ID in Trash for this fixture. It does not
establish retention duration, broader file sizes, concurrent-edit behavior,
or the shared journal's ability to publish a Trash-backed recovery object.
