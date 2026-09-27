# iCloud worker registration-response loss — 2026-09-27

Registered before the live run. This tests the shared upload worker rather
than the earlier standalone handoff journal. Both processes use the isolated
`iCloudGuiValidation` session and one fresh Cirrove-owned folder/file. The
ordinary iCloud mount remains read-only.

Question: after Apple accepts a new file's registration but Cirrove loses its
response, can a fresh worker process use its durable checkpoint and journal to
bind exactly one remote ID and publish `Uploaded` without another upload?

Arm A: create a UUID-named test folder and file, enqueue its immutable bytes
in the shared upload journal, save the account/folder/name/size/hash checkpoint,
then send the native upload and registration. Deliberately discard the received
registration response before parsing its item ID. Expected end state is
`VerifyRequired`, retaining the spool and checkpoint. Arm B: exit Arm A, load
the same journal and saved session in a fresh process, check that the exact
folder ID still exists at root, list that folder, require a single matching
name and complete bytes, and publish its remote ID as `Uploaded`. The Arm B
adapter refuses any `upload_part` invocation, so it cannot silently replay.

Prediction: Arm A reaches `VerifyRequired`; Arm B reaches `Uploaded` with the
same operation ID and a verified remote item. Absence or ambiguity must leave
the operation unresolved. Each arm runs once, so there is no within-arm spread
and no latency claim. A deliberately discarded response after a server reply
does not test an in-flight timeout, a failed content-slot upload, or power loss.

Endpoint: durable worker state and receipt after each process. The two private
manifests record command, binary SHA-256, PID, expected duration and a
disk-backed temporary directory before launch. The owned fixture remains in
iCloud Drive. No credentials, signed URLs, raw provider bodies or file bytes
are recorded here.

## Observed

Arm A ended at `VerifyRequired` for operation
`0375a5be-b270-4901-bc15-7f954d548858`. It retained the immutable upload
and checkpoint after the registration reply was deliberately discarded. Arm B
started in a separate process, found one matching file under the exact saved
folder ID, read and hashed its complete bytes, and published the same operation
as `Uploaded` with a remote item receipt. The Arm B provider rejected any
`upload_part` call, so this recovery did not replay the upload. Both processes
used binary SHA-256
`d9144da609dd14dd529439e1b56cdb8aaf329daa8e9767b8cddcce4c3625d18a`.
Their private manifests and journal live under `.local-state/` and used btrfs
temporary directories. This confirms one successfully committed registration
whose response was deliberately lost; it does not establish behavior when the
request is still in flight or when the uploaded content slot is incomplete.
