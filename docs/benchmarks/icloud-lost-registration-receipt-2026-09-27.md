# iCloud staged-file registration response-loss experiment — 2026-09-27

Registered before the live run. The two rename-boundary trials recovered
after deliberately lost receipts, but their plan began only after a staged
file had already been uploaded and independently identified. This experiment
targets the earlier boundary where the registration request may have reached
Apple while the remote file ID has not been saved. It uses a new Cirrove-owned
folder and two small test files; the normal service stays read-only.

Question: Can a unique staged name and full content hash be durably reserved
before registration, then used after a lost response to discover and bind the
new exact remote ID without sending a second upload or registration request?

Arm A: create and verify the first test file. Reserve a UUID-suffixed staged
name and full SHA-256 under an account/collection/folder/old-ID-bound private
record; fsync `prepared`. Fsync `registration_pending` before allocating an
upload slot, uploading bytes and sending `add_file`. Deliberately discard the
registration response and exit before inspecting the folder. Arm B: a new
process loads the pending record, re-lists the exact parent and checks the
original ID/hash, then requires exactly one UUID-named candidate with the
expected full hash. It binds that candidate's remote item/document IDs and
fsyncs `complete`. If absent or ambiguous, it stops without replay. No
comparative arm.

Prediction: after a completed registration with a discarded response, the
second process will bind the unique file ID and preserve both fixtures. This
does not test a request still in flight, an interrupted upload-slot transfer,
power loss during fsync, concurrent remote clients, or Apple account classes.
Absence after a pending request remains uncertain and cannot authorize retry.

The private record and per-process manifests live under
`.local-state/icloud-lost-registration-receipt-validation/`. Each manifest
records command, binary hash, PID, expected duration and disk-backed private
temp filesystem before its process starts. No credentials, signed URLs,
receipts, raw provider bodies or file contents are recorded.

## Observed first run

The first process exited normally after 81.3 seconds with a private `0600`
record at `registration_pending`, with no staged remote ID recorded. The
fresh process completed in 33.2 seconds using the same binary hash and
account session. It found one candidate under the reserved name in the
exact folder, checked the unchanged original ID and both complete content
hashes, bound a distinct staged item/document ID and fsynced `complete`.
No upload or registration request was replayed and no temporary checkpoint
file remained. These are one run per arm without a within-arm spread. The
response was deliberately discarded *after* Apple replied; an in-flight
timeout and interrupted content-slot upload remain untested.
