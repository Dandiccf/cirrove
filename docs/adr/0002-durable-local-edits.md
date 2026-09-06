# ADR 0002: Durable local edits and explicit remote acknowledgement

Status: local upload-journal component implemented; provider and writable-filesystem
integration remain in progress. This does not enable cloud writes.

## Save contract

An application save will first protect a complete local edit generation. Local-save
success and remote-upload success are separate states. A network failure must not
remove the only copy of an edit, and a restart must not blindly repeat a mutation
whose response may have been lost.

`UploadJournal` implements the first part of that contract: it copies a guarded
source into a private spool file with bounded buffering, hashes and fsyncs it,
publishes it under a unique operation ID, and fsyncs its directory before committing
the pending operation to a separate SQLite journal with FULL synchronous mode.
The caller must prevent changes to the source while sealing that generation.
The existing read cache has no reference to these files and cannot evict them.

If publication succeeds but the journal transaction fails, the unpublished-to-queue
file remains available for recovery. It counts against storage limits and is not
automatically uploaded or deleted. Source-read and pre-publication quota failures
leave no queued operation. Actual hardware power-loss and physical disk-full tests
remain required; injected failures and process termination are narrower evidence.

## State and restart semantics

| State | Meaning |
| --- | --- |
| Pending | Complete local snapshot is durable and awaits a worker |
| Uploading | A worker owns a durable attempt token; the remote outcome may be uncertain |
| Verify required | Remote state must be checked before retry or acknowledgement |
| Verifying | A worker is reconciling an uncertain result, without starting another upload |
| Uploaded | A validated remote receipt is durably recorded |
| Conflict | Local bytes retained; a new resolution intent is required |
| Failed | Local bytes retained; corrective action and verification precede retry |

Restart changes interrupted uploading and verifying attempts to verify-required.
Attempt tokens prevent a late response from acknowledging a later attempt. An
uncertain success can be reconciled with a fresh verification token without another
upload. A verified non-commit can return to pending. No API here automatically
deletes pending, failed, conflicted or uncertain content. Only acknowledged payloads
can be pruned, leaving their durable receipts in place.

Edits of the same remote identity stay ordered. An unresolved edit does not block
independent files. A corrupt local snapshot fails before transfer and leaves the
next independent file eligible. Creates carry parent/name and require failure on
name collision. Replacements carry an item ID and the originally observed metadata
ETag; provider rules must enforce those preconditions.

## Provider integration still required

Microsoft documents resumable upload sessions, server-reported missing ranges,
name-conflict behavior and conditional headers. These contracts belong in the
Graph adapter. Session URLs are preauthenticated credentials: persist them privately
through credential storage, pass references through the journal, and never log them.
The Graph bearer token must not accompany uploads to a preauthenticated session URL.
See [Microsoft upload sessions](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0).

Before replacing existing cloud content, test a competing edit during the upload
itself. Do not infer final-commit conflict protection merely from a precondition
accepted when the session was created. A lost response must be reconciled against
the session and remote content; matching only a filename and size is insufficient
proof that our upload committed.

The remaining integration must also implement durable session/progress references,
new generations created during an active upload, metadata operations and their
dependencies, writable FUSE open/write/truncate/fsync/atomic-save behavior, user
conflict resolution and retention policy for old receipts. Live tests use a dedicated
test folder after opt-in write consent. The currently installed mount stays read-only.

## Validation

Synthetic journal tests exercise private file permissions, Unicode names, quota and
source failure, a database failure after file publication, checksums, operation
ordering, conflict retention, stale attempt tokens and malformed remote receipts.
A separate child process is killed at pending, uploading and acknowledged phases;
each recovered journal retains the expected state and exact local bytes.

These checks validate the local component. They do not demonstrate working Graph
uploads, application-save semantics or completion of the safe-file-changes milestone.
