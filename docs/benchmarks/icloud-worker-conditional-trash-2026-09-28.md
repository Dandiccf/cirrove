# iCloud shared-worker conditional Trash handoff — 2026-09-28

Registered before the live run. The isolated `iCloudGuiValidation` account
will receive one new UUID-named Cirrove folder containing two small new
Cirrove-owned files. The normal mount and other account files are untouched.
The former item may remain recoverable in Trash; no permanent deletion is
authorized by this run.

Question: can the native fixture adapter carry a conditional old-file Trash
step and staged rename through the shared upload worker, then publish both
exact provider IDs at their real locations in one durable journal receipt?

One arm: create and verify the two owned files, reserve the old identity in
the journal, move the original to Trash with its saved ETag once, verify its
exact ID and full bytes in complete Trash metadata, rename the staged ID to
the original name once, and independently verify both IDs/full bytes before
publishing the receipt. A refused or uncertain result must stop without
blindly repeating a cloud mutation. No comparative arm.

Prediction: the worker will publish the staged ID as current and the old ID
with the opaque Trash parent as a hidden, recoverable namespace object. A
failure would identify an adapter/journal mismatch. One successful run does
not establish repeatability, concurrent-client safety, response-loss safety,
general-file writes or latency. There is no within-arm spread estimate.

Endpoint: journal state, both exact IDs, old recovery parent, bounded full
content hashes and stable ETags/restore metadata. A private manifest records
the command, binary SHA-256, PID, duration and disk-backed temporary storage.
No credentials, signed URLs, raw provider bodies or file bytes are recorded
here.

## First observed run

The new folder and original file were created and the original was read back
byte for byte. The worker then ended `VerifyRequired` after four failed
attempts without publishing either remote receipt. The journal retains its
Trash-location reservation and checkpoint, with no backup or current ID
committed. The first run therefore does not answer the handoff question.
The exact remote phase is not established by that journal alone.

## Read-only phase inspection registered before follow-up

Question: did the first run leave both files prepared, move the old exact ID
to Trash, or complete the remote handoff without publishing a receipt?
One arm: reopen only the retained isolated journal and saved checkpoint,
then observe its exact IDs and bounded bytes with no mutation request.
Prediction: the inspection will distinguish a prepared pair from an old
item in Trash or a completed remote handoff, or return an explicit uncertain
error. The endpoint is the fixture phase only; no general-provider claim or
latency comparison follows from one observation. A separate private manifest
records the binary, process and disk-backed temporary storage.

The read-only inspection found the old exact ID and bytes in Trash but
refused a receipt because its displayed Trash name differed from the former
folder name. This is a presentation change, not evidence that the item ID
changed. The adapter had incorrectly required identical names. Its revised
receipt keeps the actual observed Trash name, checks that it is bounded and
stable through the download, and continues to bind recovery by exact ID.
Before the second read-only observation, the prediction is `OldAtRecovery`:
the old exact ID and full bytes are in Trash, while the staged ID remains in
its original folder. The arm uses only the same saved checkpoint and exact-ID
reads; the endpoint is the phase returned by the revised adapter. A different
phase or error will be recorded without mutating either file.

The revised read-only observation returned `OldAtRecovery`. It verified the
saved old ID, complete bytes and stable recovery metadata in Trash; the
staged exact ID remained in its folder under the staging name. No cloud
mutation was sent by either inspection.

## Fresh-process completion registered before resume

Question: can the retained shared-worker checkpoint resume from the verified
`OldAtRecovery` phase, rename only the staged exact ID, then atomically bind
the current and recoverable Trash IDs in the journal?
One arm: reopen the original journal in a new process, inspect its saved
checkpoint and remote state, send at most one staged rename if still needed,
then require a complete two-ID, full-byte receipt. The former ID must remain
in Trash. No second Trash request is permitted. An uncertain observation or
rename response leaves the journal at `VerifyRequired` for manual review.
Prediction: the staged ID becomes current, the old ID remains bound to the
opaque Trash parent, and the journal reaches `Uploaded`. This is one owned
fixture, not a general-write or concurrency claim, with no latency spread.
A new private manifest records the command, binary hash, PID and disk-backed
temporary directory before execution.

The fresh-process worker remained `VerifyRequired`; the journal still has
neither published remote ID. This does not show whether Apple's staged
rename committed. Before another read-only inspection, the prediction is
either `Complete` (rename committed but receipt verification failed) or
`OldAtRecovery` (rename was not accepted). The arm reuses the saved exact-ID
checkpoint and sends no mutation; the endpoint is only the remote phase or
an explicit observation error. No further rename is authorized until that
state is known.

The read-only post-resume observation returned `Complete`: the staged exact
ID has the original name and full bytes while the old exact ID remains in
Trash. The worker still has no committed receipt. Before a further read-only
check, the question is whether the adapter can construct a complete two-ID
receipt and whether its current size matches the journal. One arm constructs
that receipt without sending a mutation; the predicted result is two valid
identities with matching size and revisions. The endpoint is receipt validity
and these boolean checks, not a further cloud operation. A private manifest
records the binary, PID, duration and disk-backed temporary storage.

The read-only receipt check succeeded: current size matched the journal,
the old exact ID was in Trash, and both revisions were present after full
byte verification. The remaining failure is publication through the worker
or its timing, not missing provider bytes.

## Local receipt publication registered before run

Question: will the shared journal accept this freshly reverified two-ID
receipt and atomically bind the actual Trash parent without any further
iCloud mutation? One arm obtains the same exact-ID/full-byte receipt, claims
a new fenced verification attempt, then calls the journal's ordinary
`acknowledge_identity_handoff` transaction. If the transaction rejects it,
the attempt returns to `VerifyRequired`; no provider request is repeated.
Prediction: the record reaches `Uploaded` with the staged current ID and a
hidden old ID bound to Trash. This is a local recovery of one owned fixture,
not proof that worker timeouts are solved. Endpoint: durable journal state
and both mappings; no latency comparison. A private manifest precedes the
run and records the binary, PID and disk-backed temporary storage.

The local publication succeeded without a cloud mutation. Reopening the
SQLite journal showed `Uploaded`, one current binding to the staged exact ID,
and one hidden, owned recovery binding to the old exact ID whose parent is
the opaque iCloud Trash root. This proves the journal accepts the verified
receipt in this one owned fixture. It does not prove the automatic worker
meets its request deadline: its prior complete-state checks still ended
`VerifyRequired` and required this local recovery.
