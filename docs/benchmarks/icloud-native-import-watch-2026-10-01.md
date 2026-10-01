# Retained native import observation

Question: can a restarted public daemon confirm an already uploaded package from
its exact durable operation and fresh metadata, without submitting or changing
any upload? This resumes observation, never a mutation or upload attempt.

The isolated public import is run `ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`, operation
`b534c269-8b9e-47e0-97a0-a512d0127765`, exact allocated document
`FILE::com.apple.CloudDocs::8A909C8D-32D7-4E39-9202-DF1862E46C5A`.
Its independent semantic/FUSE/offline/refetch verification already passed.
Prediction: fresh metadata matches its retained legacy receipt, the public watch
job succeeds, and the mounted owned folder exposes that same document while its
upload record remains unchanged.

## Controls and evidence

Require a stopped isolated daemon, one exact Uploaded package operation, private
disk temp and an exclusive new manifest. Record binaries, command, source state,
PID, expected duration and pre/post journal fingerprints. Start only the isolated
account, warm its owned folder, then submit `watch-native-import` with exact
account UUID and operation. The request contains no archive or new destination.
Verify the success receipt and mount identity. Stop only that daemon. No installed
service, account, Strata binary or default file manager is changed.

The observer has a 20-minute bound and a 30-second fresh metadata-read bound.
The live runner has a separate bounded lifetime. Failure does not authorize
resubmission; the same retained operation remains the only target.

The first tests-only negative timed out because its synthetic socket directory
was not mode 0700; that run does not prove the feature boundary. After correcting
the fixture, `native-watch-negative2` failed at the intended missing-watch
assertion. The implementation was then applied. Positive synthetic and live
results remain pending. Public tests exercise restart with no upload workers,
exact account/operation refusal, canonical metadata with an unchanged legacy
receipt, stale publication rejection, cancellation and unchanged journal rows.

## Focused results before the live observer

The corrected public watch scenario passed. Forcing resumed observation to trust
the old publication record made it fail at the intended stale-publication
assertion; restoring fresh exact-ID metadata made it pass again. The CLI parser
case passed and rejects missing account/operation and archive/source arguments.

Review found a same-Engine mount replacement during a journal await. A deterministic
paused-journal test failed before the correction with "receipt from withdrawn
mount was accepted". The manager now rechecks the exact open WriteControl and
current registry/status after journal reads. Its lock order was independently
reviewed against manager removal/remount/shutdown and matches existing enqueue
checks. All 27 native-import tests then passed (one real-FUSE scenario ignored in
that normal library command). Evidence arms: `native-watch-positive`,
`native-watch-freshness-negative`, `native-watch-restored`, `native-watch-cli`,
`native-watch-remount-negative`, `native-watch-remount-positive`.

These checks do not replace the planned live observer. The installed daemon has
not been restarted or changed.

## Live result

The isolated live observer completed successfully at 2026-10-01T11:24:45.725654+00:00.
The public job reported Succeeded for the exact retained operation/allocated ID;
the warmed mount and public paths reply showed that same package. SQLite upload
records had identical pre/post fingerprints. The observer CLI exited 0 and the
separate daemon stopped cleanly with exit 0. Evidence is
`/var/tmp/cirrove-public-native-ec7f82e1/watch-attempt1/`.

The exact new document was subsequently opened from iCloud Drive in Apple Pages.
Its document ID and ec7 title matched the receipt; both accessibility text and
visual inspection showed the retained synthetic sentinel from the source archive
(originating in source run0c5 and carried through ac9). No content was edited.
The local observation is `public-pages-ui-observation.json` under the public run.
This is web Pages open acceptance, not native macOS editing or Numbers/Keynote.
The original import job's historical false failure is preserved; this successful
observer does not rewrite it or replay the upload.
