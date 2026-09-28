# iCloud owned empty-folder Trash — 2026-09-28

Registered before the live run. The only target is the nested folder created
by Cirrove operation `5cfd6d16-3b24-4485-8f44-967c4bc80e8f` in the isolated
`iCloudGuiValidation` account. Its parent is a fresh UUID-named Cirrove test
folder. The source ID and ETag come from that exact applied Create receipt,
not from a name search. No existing user folder or other cloud client is
changed. The normal iCloud mount stays read-only.

Question: can the shared mutation worker implement non-recursive `rmdir` for
one owned empty folder, preserve its exact ID across a lost Trash response,
and reconcile the resulting recoverable Trash item in a fresh process? The
adapter obtains a complete child listing immediately before sending Apple's
`moveItemsToTrash`. An incomplete listing, nonempty folder, changed ETag or
unexpected parent must stop the mutation. Apple's request may itself remove
recursively; this preflight cannot make the emptiness check atomic against a
concurrent external writer.

Arm A: the first process prepares the exact folder ID, confirms emptiness and
discards the accepted Trash response. Prediction: its journal retains
`VerifyRequired` and the prepared ID. A second, reconciliation-only process
must find that ID in a complete Trash listing and publish `Removed` without
another Trash request. The endpoint is the independently reopened SQLite
mutation record and exact-ID provider observation. If the Trash item cannot
be established, leave the operation uncertain; do not replay the request.

This is one injected-response-loss trial. It does not prove a real network
timeout before Apple responds, cross-account reliability, a concurrent child
write, or general mounted deletion. Private manifests record command, PID,
binary SHA-256, expected duration and disk-backed TMPDIR/SQLITE_TMPDIR before
each process starts. The live run does not overlap compilation. Credentials,
signed URLs, raw provider bodies and cursors are not logged.

## Observed

Both isolated processes exited zero. The first process obtained the exact
folder ID from the applied Create receipt, verified a complete empty child
listing immediately before the Trash request, deliberately discarded the
response, and left operation `4938b3e2-3f36-4856-9100-a93d81a2c5eb` at
`VerifyRequired` with the prepared ID. The second process was
reconciliation-only. It found that same ID in Apple's complete Trash listing
and reached `Applied` without sending another mutation.

An independent read-only SQLite query after both processes exited found one
`removed` receipt for
`FOLDER::com.apple.CloudDocs::F10A7DF2-4515-47E0-84C9-3EC3433E7B7B`;
the prepared and removed IDs matched, and the state was `applied`. Both
private manifests record binary SHA-256
`5ccb0b82d6786f6f22d3e540868893f51e17e4b925f13ccd3b5a9bccd0988c5b`
and btrfs-backed temporary storage. This one arm supports exact-ID recovery
for a deliberately lost accepted response, not arbitrary network failure or
atomic `rmdir` under concurrent external writers.

A synthetic HTTP test also serves a nonempty child listing to the mutation
adapter and asserts that no Trash request follows. With only the emptiness
guard temporarily disabled, that test failed because the adapter attempted a
fourth HTTP request; restoring the guard made it pass. This verifies the test
can detect removal of the guard, but the synthetic fixture cannot prove the
size of the live race window between the final listing and Apple's mutation.
