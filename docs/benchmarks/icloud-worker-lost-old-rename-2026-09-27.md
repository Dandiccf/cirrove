# iCloud shared-worker old-rename response loss — 2026-09-27

Registered before the live run. Both processes use only a new UUID-named
Cirrove folder and two small owned files in the isolated
`iCloudGuiValidation` account. The ordinary mount remains read-only.

Question: when the old-ID recovery rename completes but the worker drops its
result, can a new process load the saved first-phase checkpoint, verify the
exact old and staged IDs and full bytes, and safely execute only the second
rename before publishing both identities in the shared journal?

Arm A: prepare and seal one replacement, reserve the old recovery identity,
save the first-phase checkpoint, send the old rename and deliberately discard
the returned result. Require `VerifyRequired`, a retained checkpoint and a
reserved hidden recovery object. Arm B: exit Arm A, open the same journal and
credential vault in a new process, rebuild the adapter from the saved
checkpoint and run the shared worker. It may advance to the second phase only
after independently observing the old exact ID at recovery and the staged
exact ID with their expected full hashes. No comparative arm.

Prediction: Arm A ends uncertain but durable; Arm B saves its second-phase
checkpoint before the new rename, then ends `Uploaded` with the staged ID as
current and old ID as remote-owned hidden recovery. An absent or divergent
observation must stop without replaying an uncertain rename. One run per arm
has no within-arm spread and cannot establish general concurrent-edit or
in-flight timeout safety. Apple's stale-ETag rename behavior still prevents
mounted writes.

Endpoint: process exit, durable upload state, saved checkpoint and exact
current/recovery identities. Separate private manifests record command,
binary SHA-256, PID, expected duration and disk-backed temporary storage
before each process starts. No credentials, signed URLs, raw provider bodies
or file bytes are recorded here. Both test versions remain in iCloud Drive.

## Observed

Arm A discarded an already received old-rename result for operation
`cd8bf0fd-ff8a-4a92-a902-b40eeaf0fa8d` and exited with `VerifyRequired`.
The shared journal retained its first-phase checkpoint and hidden recovery
reservation. In a new process, Arm B loaded that checkpoint, observed the old
exact ID at its recovery name and the staged exact ID with both full hashes,
saved the second-phase checkpoint, installed the staged ID, and reached
`Uploaded`. The shared journal then held the new current binding and the old
remote-owned recovery binding. Arm B did not resend the old rename. Both
private manifests record binary SHA-256
`21f43b2735bb4fd02c58965632cb9da5a5ddc41c1907f7e2fd7fe34583e9f551`
and btrfs temporary storage. This is one run per arm on one owned fixture;
it does not cover a request still in flight, the second rename's lost result,
concurrent edits or broader account behavior.
