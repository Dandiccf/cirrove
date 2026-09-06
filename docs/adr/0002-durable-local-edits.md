# ADR 0002: Durable local edits and explicit remote acknowledgement

Status: journal, transfer worker, experimental Graph upload adapter and initial
local FUSE write path implemented with synthetic validation. Live provider and writable-filesystem validation remain
in progress. Ordinary mounts stay read-only; only an explicit developer command
performs isolated cloud write checks.

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

## Provider integration and remaining validation

Microsoft documents resumable upload sessions, server-reported missing ranges,
name-conflict behavior and conditional headers. These contracts belong in the
Graph adapter. Session URLs are preauthenticated credentials: the worker persists
them privately through credential storage and passes only references through the journal.
The Graph bearer token must not accompany uploads to a preauthenticated session URL.
See [Microsoft upload sessions](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0).

Before replacing existing cloud content, test a competing edit during the upload
itself. Do not infer final-commit conflict protection merely from a precondition
accepted when the session was created. A lost response must be reconciled against
the session and remote content; matching only a filename and size is insufficient
proof that our upload committed.

The worker saves each checkpoint before transmitting the next fragment. Desktop
credential saves include an independent readback before reporting success, with
a bounded retry for a mismatched stored value. A deterministic per-operation key recovers a secret-store success whose reply was lost before the
journal reference was saved. Accepted byte counts do not imply a completed file.
Retry deadlines survive restart and apply per file. A missing session triggers
content reconciliation, including SHA-256 readback, before another attempt is allowed.
An empty or malformed stored checkpoint takes the same content-reconciliation path
as a missing session; invalid credentials cannot become an endless parse/retry loop.
The replacement path requests deferred commit and sends the original ETag at final
commit. Business and document-library drives use a zero-length POST to the session;
personal drives use Graph's source-URL PUT. Selecting the correct endpoint does not
prove that the provider enforces a precondition. The explicit developer validation command includes a live competing-edit check;
one business-drive fixture passed. That does not complete the broader account and
concurrency matrix.

The experimental working-file layer now seals new generations during an active
upload and exercises FUSE open/write/truncate/fsync, including offline restart.
An explicit writable-session owner starts bounded upload workers, wakes them after
sealing, and drains accepted local callbacks before disconnecting its FUSE session.
Cancelled remote attempts remain subject to reconciliation; shutdown does not wait
for cloud acknowledgement or discard unsealed dirty copies on snapshot failure.
The remaining integration must implement dependencies between metadata operations,
atomic-save behavior, user conflict resolution and retention policy for old receipts. Live tests use a dedicated
test folder after opt-in write consent. See [the developer workflow](../write-validation.md).

## Namespace journal

Schema 3 stores create-folder, relocate and remove-file intents alongside upload
snapshots under the same ownership lock. A shared sequence/resource queue orders
changes of the same identity and colliding source/destination names. Applied
receipts and queue completion are one transaction. Interrupted applying/verifying
states reopen as verify-required, and stale attempts cannot acknowledge newer work.

A namespace worker performs network calls outside the journal lock and has bounded
cancellation, deadlines and durable retry delays. Unprovable outcomes become
`NeedsReview`; this includes a lost folder-create reply or a missing item after an
uncertain delete. Matching paths alone cannot adopt an unrelated folder. Pending
and conflicted records are retained. Hierarchical dependencies, local generation
rebasing and atomic replacement still belong to the forthcoming writable namespace.

## Validation

Synthetic journal tests exercise private file permissions, Unicode names, quota and
source failure, a database failure after file publication, checksums, operation
ordering, conflict retention, stale attempt tokens and malformed remote receipts.
A separate child process is killed at pending, uploading and acknowledged phases;
each recovered journal retains the expected state and exact local bytes.

Synthetic HTTP fixtures additionally verify exact upload ranges, bearer-header
separation, redirects, expiry, shared throttling, conditional headers and content
reconciliation. Worker tests interrupt partial transfers and lose completion replies;
keyring failures, cancellation and conflict responses preserve local bytes.

These checks do not demonstrate working live Graph uploads, application-save
semantics or completion of the safe-file-changes milestone.


## Mutable working files and consecutive generations

Schema 4 adds linear upload-generation dependencies; schema 5 adds working files.
The current journal refuses a newer schema and never temporarily downgrades its
version during migration. A complete, quota-reserved hydration is required before
an existing remote file becomes editable. Working-file metadata records dirty state
before in-place changes; immutable snapshots and their generation links commit
atomically. A later save follows the confirmed predecessor receipt, not the old
pre-upload ETag. Conflicts retain both sealed and newer mutable local bytes.

Working-file tests inject failure between byte changes and metadata updates and
between snapshot publication and queue commit. Actual process-kill tests preserve
saved generations separately from subsequent unsealed changes. A synthetic transfer
worker resumes an interrupted first generation before uploading its successor.
The kernel suite also exercises writable-file recovery after killing its mount
process and remounting offline. These are process-failure checks, not power-loss
or live-provider application-save evidence. See [architecture](../architecture.md)
for the experimental API boundary and remaining writable operations.
