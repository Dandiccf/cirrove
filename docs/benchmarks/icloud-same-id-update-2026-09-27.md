# iCloud same-ID update experiment — 2026-09-27

This protocol experiment is registered before the live run. It is not a
performance or reliability gate. It uses the isolated `iCloudGuiValidation`
saved session and creates a **new** root fixture folder and file. Only that
file may be the update target. Existing user items and other clients are not
read or changed. The normal Cirrove iCloud mount remains read-only.

Question: Will `update/documents` accept a new upload receipt with the
existing document ID and `allow_conflict: false` (plus its observed ETag),
preserving the Drive/document identity and exposing the exact revised bytes?
The request format is an experiment, not a documented Apple contract.

Arm: one fresh unchanged-base attempt. The probe first verifies the original
ID, revision and exact bytes. It allocates and uploads replacement bytes, sends
one update request with the original document ID, then independently lists and
reads the resulting file. It never retries a possibly committed request. A
rejection is only called unchanged if the original ID, ETag and bytes remain;
any other state is indeterminate. No comparative arm is part of this run.

Prediction: Apple's web transport will likely reject this request or treat it
as a new-file collision; existing clients allocate a new document ID and do
not demonstrate same-ID replacement. If it succeeds, this still does **not**
prove stale-revision rejection, because the server may ignore the supplied
ETag. That requires a separate competing-edit trial with its own fixture.

Endpoint: report accepted with exact same-ID readback, rejected with unchanged
readback, or indeterminate. Record elapsed time and stage without tokens,
signed URLs, provider response bodies or contents. Leave the test fixture in
iCloud Drive. Use disk-backed private `TMPDIR`/`SQLITE_TMPDIR`, compile before
the run, and register binary hash, command, PID and expected duration in the
ignored `.local-state/icloud-same-id-validation/manifest.json`.

## Observed first run

The pre-registered run completed in 71.0 seconds. The probe created and read
back its own new file, then sent an update with that existing document ID and
the old ETag. Apple accepted the request. A subsequent folder listing retained
the same Drive/document ID and only one entry with the test name, and an
independent read returned the exact revised bytes. The fixture remains in
iCloud Drive. This is a single positive same-identity update, **not** proof
that Apple enforces the ETag: the base was unchanged at commit. The next trial
must deliberately reuse a stale ETag after a prior update and check whether
the server rejects it without changing the file.
