# iCloud owned-file Trash and restore cycle — 2026-09-27

Registered before the live run. A bounded read-only scan showed a complete
special Trash listing with a restore path. This trial uses a new Cirrove-owned
folder and a new small file. The normal service stays read-only; no existing
user file or other client's state is touched. The trial does not permanently
delete anything.

Question: After moving the exact test ID to Trash with its current ETag,
does the complete Trash listing contain that same ID with a recovery path,
and can a single `putBackItemsFromTrash` request return the original bytes
to the original folder? Does the Drive item ID survive restore?

One arm: create the folder/file and verify exact ID, document ID, name,
ETag and bytes. Send one current-ETag `moveItemsToTrash` request, verify the
exact ID is absent from its parent, read the bounded complete Trash list,
and require exactly one matching item ID with an ETag and `restorePath`.
Send one restore request for that ID/ETag. Re-list the original parent,
require one file with the original document ID/name and full byte sequence,
and classify whether the Drive ID stayed the same or changed. If any stage
is uncertain, stop without replay. No comparative arm.

Prediction: the same ID and bytes will reappear in the original folder.
This is not a crash/restart trial. It does not prove recovery from a lost
request response, cross-account behavior, shared-folder behavior or general
Trash consistency.

The private manifest under `.local-state/icloud-trash-restore-validation/`
records command, binary hash, PID, expected duration and disk-backed temp
filesystem before the process starts. It records no credentials, URLs,
raw provider bodies, file contents or item IDs.

## First run and read-only follow-up

The process exited with an indeterminate classification after 130.8
seconds. It had created and byte-checked its own fixture, but the current
probe combines several later uncertainty points. No request was replayed.
Before changing any remote state, a separate bounded read-only Trash count
will determine whether the item population increased. This cannot by itself
identify the exact failed stage or prove the owned item is there.

The read-only follow-up completed in 93.8 seconds and again found one
complete Trash entry with a restore path, the same count as before the
cycle. It did not establish the owned file's identity. The next probe
classifies the stopping phase without printing raw metadata.

The [separate identity follow-up](icloud-trash-restored-identity-2026-09-27.md)
later verified that a restored owned file retained its document ID and
bytes but received a different displayed name. This is a plausible
explanation for this first run's indeterminate result, not direct proof of
its unrecorded item state.
