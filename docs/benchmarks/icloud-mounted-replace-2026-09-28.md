# Mounted iCloud replacement, 2026-09-28

## Registered before live arms

Question: Can a separate application overwrite a file through the isolated
Cirrove FUSE mount, have the shared upload worker first stage the sealed new
bytes under a reserved document ID, then conditionally move the exact old ID
to recoverable Apple Trash and install the staged ID under the original name?
Can a fresh process reconstruct the new ID and content from the journal without
making another cloud mutation?

Arm A creates a new UUID-named `Cirrove Write Validation-*` folder and uses
FUSE to create `Mounted Create.txt` and `Mounted Folder`, with independent
exact-ID and full-byte verification. Arm B reopens that fixture and a separate
Python process truncates, writes and fsyncs only `Mounted Create.txt`. The new
bytes are `Cirrove isolated mounted replacement` followed by a newline. The
old bytes are `Cirrove isolated mounted iCloud validation` followed by a
newline. Arm C opens the same fixture again and only reads the replacement
through FUSE and an independent iCloud session. These are Cirrove-owned test
items; no existing user file, normal account configuration or installed daemon
is changed.

Endpoint for B: two uploaded journal records with a Replace intent bound to
the exact first receipt ID and ETag; the second receipt has a different ID,
the original name and parent, and the new full-byte SHA-256; an independent
complete parent listing and content read agree. The old exact ID must be
uniquely visible in a complete Trash listing. The handoff provider itself
checks both exact IDs, ETags and full hashes before acknowledging. On failure
or uncertainty, retain the local snapshot, checkpoint, journal and fixture;
do not infer success from a name or retry an uncertain old-ID Trash request.

Endpoint for C: the same two upload receipts and one folder-create mutation
remain; FUSE and independent reads return the new bytes and exact second ID;
no new upload intent appears. Every arm must leave its mount absent at exit.

Prediction: A passes as in earlier mounted Create trials. B reaches an exact
two-ID `HandoffComplete` receipt through the shared worker; C restores that
receipt and reads the replacement without replay. A failure before remote
commit or an uncertain Apple response is a failed/inconclusive arm, not a
reason to claim general write reliability. Even three successful arms would
not prove behavior under concurrent clients, lost responses or large files.

## Results

Arm A passed. The separate FUSE application created and fsynced the file and
created the folder. The worker recorded one uploaded file and one applied
folder creation; the validator independently listed both exact receipt IDs
and read all file bytes. The mount was absent after exit. The fixture UUID is
`c02f3682-392b-49a3-a9ca-a146c78ad56d`. The private manifest is
`.local-state/icloud-mounted-replace-create-control-80b2ad17-a28c-4a51-9e16-f7dbbf6e4a39/manifest.json`:
PID 2887977, binary SHA-256
`c583e126836fa4914ed8a2f3307b79e796fbf08190193f359393a3e5824c6ec9`,
start `2026-09-28T12:19:22Z`, expected window 360 seconds, btrfs-backed
`TMPDIR` and `SQLITE_TMPDIR`. No completion timestamp was recorded, so this
supports no duration claim.

Arm B's FUSE application returned, and the worker journal recorded two
`uploaded` files and the one existing applied folder creation. The validator
then exited nonzero at its independent Trash check, so the arm is **not**
counted as a full pass. Its new check incorrectly required an explicit
`restorePath: null` field. At that point this was wrongly interpreted as an
optional field; the later diagnostic below corrected that interpretation.
The mount was
absent after exit. The private manifest is
`.local-state/icloud-mounted-replace-control-9931511c-5bc6-4776-a1ef-3840f4fbdaf7/manifest.json`:
PID 2890669, binary SHA-256
`c583e126836fa4914ed8a2f3307b79e796fbf08190193f359393a3e5824c6ec9`,
start `2026-09-28T12:22:14Z`, expected window 900 seconds, btrfs-backed
temporary directories. No completion timestamp was recorded, so this supports
no duration claim. The replacement is **not retried**; the journal's completed
receipt and live iCloud state will be examined only through Arm C.

First attempted correction before Arm C: the independent Trash predicate was
changed to accept either an omitted `restorePath` or an explicit null, still
the wrong condition. It required a complete listing and exactly one matching
old ID. The read-only remount remains the
registered Arm C; it will verify both exact receipts, bytes, Trash presence and
absence of a new upload intent. A failure there leaves the result unresolved.

The first Arm C read-only remount again stopped at the independent Trash check.
It had already observed both `uploaded` records and identified the current file
in the independent listing, but
the old exact ID was not accepted by the independent predicate. No new upload
or mutation appeared and the mount was absent at exit. Its manifest is
`.local-state/icloud-mounted-post-replace-control-85bab20c-46cd-4c35-9386-e635ccc4f2f5/manifest.json`.
The predicate is now made diagnostic: it reports only the number of exact-ID
matches or whether restore metadata is present, never any provider body or
signed URL. A further **read-only** remount will determine which condition
failed. No replacement or Trash request is retried.

That diagnostic remount found exactly one old ID with a **present**
`restorePath`; the predicate rejected it because its condition was inverted a
second time. The private manifest is
`.local-state/icloud-mounted-post-replace-diagnostic-c61b2a19-6d92-448f-8c2c-0ff11bf4e573/manifest.json`.
The existing handoff verifier and the earlier Trash restore experiment require
a non-null `restorePath` as evidence of recoverability. The independent
predicate is corrected to the same requirement. This is a validator error,
not evidence that the old file disappeared from Trash. A final read-only
remount will check the registered Arm C endpoint without repeating any cloud
mutation.

The final read-only Arm C passed. A fresh process restored the journal's
account-bound ownership, read the replacement through FUSE, and independently
listed the current exact ID and read all 37 new bytes. It found exactly one old
ID in a complete Trash listing with a non-null `restorePath`. The journal
remained at two `uploaded` rows and one `applied` folder creation; no new
upload or mutation was added. The mount was absent after exit. Its private
manifest is
`.local-state/icloud-mounted-post-replace-final-d81cce6b-9848-4865-b9da-6bfd8551bd27/manifest.json`:
PID 2910479, binary SHA-256
`fde1735f151f1e19ec1a19d78e309e73684eba73bbeb97347063891d6aedd87f`,
start `2026-09-28T12:41:57Z`, expected window 360 seconds, btrfs-backed
temporary directories. No completion timestamp was recorded, so this supports
no duration claim.

Taken together, the journal, the handoff adapter's exact-ID and full-byte
checks, the independent read and Trash inspection, and the read-only remount
support one functional mounted replacement of a Cirrove-owned 43-byte file by
a 37-byte file. The first write arm itself ended nonzero because of the
separate validator error; the result is therefore recorded with that limit,
not relabeled as an uninterrupted passing arm. Network-timeout reconciliation,
multi-client edits, larger files and normal account writes remain open.
