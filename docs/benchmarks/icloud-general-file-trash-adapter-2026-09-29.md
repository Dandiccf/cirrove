# General iCloud file-Trash adapter on an owned FUSE fixture — 2026-09-29

Question: can the conditional Trash transport and exact-ID mutation adapter
compile without the validator feature and still satisfy the shared journal's
mounted unlink/restart contract on a real iCloud account? Ordinary iCloud
connections remain read-only; the validator confines the adapter to its own
fresh UUID test file.

Prediction registered before the live runs: a separate application creates
and fsyncs one generated file and creates an empty folder through FUSE. A
second arm removes only that empty folder, then a third unlinks the generated
file through the normal `ICloudFileTrash` adapter. The adapter must verify
the journal-confirmed full digest and exact parent/file/ETag before a
prepared-ID conditional Trash request. The mutation should reach `Applied`
with the exact file ID; independent Apple listing should show the file absent
and its exact ID recoverable in Trash. A fresh-process remount should preserve
the absence without another mutation.

Arms and endpoints:

1. Fresh feature-gated mounted Create in a new UUID validation folder.
   Endpoint: one uploaded file and one applied folder-create receipt, exact
   IDs and complete bytes independently verified.
2. Mounted empty-folder removal of only that generated child. Endpoint:
   recoverable folder Trash and the file's exact ID and bytes unchanged.
3. Mounted file unlink using `ICloudFileTrash`, followed by fresh-process
   remount. Endpoint: prepared exact ID, applied recoverable removal, complete
   parent listing without that ID, exact Trash restore path and no later
   mutation. The synthetic classifier must distinguish stale rejection from
   HTTP 429/5xx and incomplete receipts, which remain uncertain.

Before each arm, save the command, binary SHA-256, PID, expected duration and
private btrfs-backed `TMPDIR`/`SQLITE_TMPDIR` in a manifest. No compilation
runs concurrently. This is a functional trial, not a latency measurement.
Only the new Cirrove-owned test items may be mutated. A passing arm does not
establish real timeout/concurrent-client reliability or authorize normal
iCloud account writes.

## Results

The feature and non-feature adapter tests passed. All four controlled live
arms used generated fixture `4a8420e4-f2ba-45eb-bead-f0699daa5ec9` and
binary SHA-256
`6b0337f4546ccf3fd847f610c03010dc5cd1e6934524d28eb6a5363503de0fe0`.
Create (PID 3865772, 104.5 seconds) reached one uploaded file and one
applied folder receipt; independent Apple reads matched both IDs and the
file's complete bytes. Empty-folder removal (PID 3867536, 123.8 seconds)
reached recoverable Trash while the file's ID and bytes remained unchanged.
The general-adapter file unlink (PID 3869694, 91.8 seconds) reached an applied
exact-ID removal; independent Apple listing lacked the file, and Trash held
the same ID with a restore path. A fresh-process remount (PID 3871317, 30.8
seconds) again found both generated items absent without another mutation.
All four manifests record btrfs-backed temporary storage and retain private
logs.

The common transport now classifies HTTP 429/5xx and missing success receipts
as uncertain instead of a clean refusal. That classifier has synthetic
coverage; no live throttling, server failure, lost response or concurrent edit
was induced in this trial. The ordinary account path remains read-only.

## Follow-up: verify Trash bytes before resolving a lost receipt

Question registered before the run: after a conditional Trash request, can
the general adapter prove the version in Trash is the saved version, rather
than trusting only its exact ID and restore path? In an uncertain or lost
response, a concurrent client could edit the same ID before it is trashed.

Prediction: on a new Cirrove-owned UUID fixture, fresh mounted create,
empty-folder removal, file removal and independent remount again pass. The
file-removal arm now reads the complete Trash bytes and checks the saved
SHA-256, size, name, exact ID and stable Trash ETag before recording Applied.
If the digest is absent, reconciliation after a lost receipt remains
Indeterminate. The endpoint is an applied exact-ID recoverable removal with
matching full bytes in Trash and an absence on remount; a mismatch must never
become Applied. This functional trial still cannot reproduce a concurrent
client edit or timeout. Each arm has a private manifest with binary hash,
command, PID, expected duration and btrfs-backed temporary storage. No
compilation runs during the trial; only the fresh generated items may change.

The first follow-up fixture, `a794b789-e9d0-4e39-8670-4560bb749768`,
created successfully (PID 3900380, 103.8 seconds) and removed its empty
folder (PID 3901820, 117.1 seconds). File removal (PID 3903602,
118.3 seconds) stopped in journal state Conflict. No Applied receipt was
recorded and the remount arm was not run. Private manifests and output are
retained under `.local-state/icloud-general-trash-verify-run-47a30e9a-de86-4aac-818b-1bd6cdce5ed3`.

Diagnostic follow-up registered before another live arm: use a new owned
fixture with the same create/folder/file sequence, adding only category
markers for source metadata, source digest, Trash identity, Trash metadata
and Trash digest mismatch. Prediction: the marker will identify which
post-Trash proof needs correction without exposing provider responses or
weakening the safe Conflict result. Stop after a failed file arm; no retry
against the same item. Each arm again has a private manifest and btrfs temp.

The second owned fixture was `c426a9fc-e6f7-4e97-8d9c-13bf5eacdacb`.
Create (PID 3909126, 132.1 seconds) and empty-folder removal (PID 3911120,
119.2 seconds) passed. File removal (PID 3912938, 121.2 seconds) again
stopped as Conflict. The category marker was Trash metadata mismatch. A
separate read-only inspection (manifest in the same private run) found exact
ID uniqueness, expected size, a present Trash ETag and matching document ID,
but a different displayed Trash name. Its first attempt used the ordinary
folder endpoint, which does not parse the special Trash response; the second
used the Trash-specific reader and yielded these booleans. No raw provider
body or credentials were logged.

Correction registered before the third owned run: do not require the Trash
display name to equal the pre-delete name. Keep the source name and ETag
preflight, conditional Trash ETag, exact Trash ID and document ID, complete
listing and restore path, complete size and SHA-256 of the Trash bytes, and
stable post-read Trash ETag. A missing digest cannot send Trash or become an
Applied reconciliation. Prediction: all four fresh-fixture arms pass, while
the two earlier journal conflicts remain untouched. A pass supports this
exact fixture shape only; concurrent edits and lost responses are still
unreproduced live.

The corrected third fixture `6e5675bc-2953-4246-b276-ac9c8af364f6`
passed all four arms with binary SHA-256 and btrfs temporary storage
recorded under `.local-state/icloud-general-trash-final-run-baa9744d-68ad-481d-b7d7-fedba14578be`.
Create (PID 3924079, 100.9 seconds) verified the mounted file's complete
bytes and folder receipt. Empty-folder removal (PID 3925555, 116.5 seconds)
left the file intact. File removal (PID 3927308, 120.0 seconds) reached an
Applied recoverable receipt after checking the saved SHA-256 against all
downloaded Trash bytes, its exact ID, size, restore path and stable ETag;
independent Apple listing showed the file absent. Fresh-process remount
(PID 3929054, 30.1 seconds) confirmed the absence without a new mutation.
This confirms the corrected fixture path, not lost-response or concurrent
edit reliability. The ordinary iCloud account writer stays disabled.
