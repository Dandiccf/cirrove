# iCloud conditional Trash handoff — 2026-09-27

Registered before the live run. Only one new UUID-named Cirrove folder with
two freshly uploaded small files in the isolated `iCloudGuiValidation`
account may be changed. The normal mount remains read-only. The old file is
left recoverable in Trash; no permanent delete or restore is attempted.

Question: can an ETag-checked move to Trash protect a newly revised original
file, then retain its exact ID and bytes as recovery while a separately
staged ID takes the original name? This combines earlier independent stale
ETag, Trash download and rename observations into one bounded sequence.

One arm: create and read back original and staged files. Change only the
original under the same ID, verify its new ETag and bytes, then send the old
ETag once to `moveItemsToTrash`. Require refusal with both exact IDs and
bytes still at the parent. Send the newly observed ETag once; require only
the old ID to enter complete, recoverable Trash and remain byte-readable.
Send one rename for the staged ID to the original name; require the staged
exact ID and bytes at the parent and the old exact ID/bytes still in Trash.
No request is blindly retried and there is no comparative arm.

Prediction: the stale Trash step will reject without moving the newer old
version; the current step will succeed; the staged rename will take the
vacated name, retaining both versions. This would establish one coherent
owned-fixture request sequence, not atomicity, concurrent-client safety,
in-flight response recovery, a durable shared-journal receipt or account
class coverage. One run has no within-arm spread or latency claim.

Endpoint: exact identities, full bytes, ETags and complete Trash recovery
metadata after each phase. A private manifest records command, binary
SHA-256, PID, expected duration and disk-backed temporary storage before
execution. No credentials, signed URLs, raw provider bodies or file bytes
are recorded here.

## Observed

The owned original was updated under its same ID and a new ETag. The stale
ETag Trash request was refused; both original and staged IDs and their full
bytes remained in the parent. The current ETag request succeeded. The old
exact ID entered complete, recoverable Trash and was still downloadable with
the revised full bytes. One rename then placed the staged exact ID and its
full bytes at the original name. A final exact-ID download confirmed that
the old revised bytes remained in Trash. The private manifest records binary
SHA-256
`ef37046556959018a4e76e44237b7c41283a818453c79dabedb631b07fe91e18`
and btrfs temporary storage. This is one sequential run on one owned
fixture, with no within-arm spread. It does not test an external edit in the
small window between the final read and Trash, a lost response, interrupted
process, name collision, Trash retention, or the shared journal's ability to
publish a Trash-backed recovery identity.
