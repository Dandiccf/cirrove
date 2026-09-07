# Architecture

## Product contract and current boundary

Cirrove makes remote files usable through ordinary Linux applications. Cached
metadata should stay available when a provider is slow; file bytes arrive on demand.
The current implementation is a **read-only preview under validation**. Ordinary mounts do not
upload through mounted paths or pin files. A separate experimental constructor
now exercises local writes and offline recovery on isolated synthetic FUSE mounts. A separate
upload journal and worker protect sealed edits, persist resumable Graph sessions
through the keyring, and reconcile uncertain attempts. They have synthetic HTTP and
crash coverage; broader live-provider checks and writable application-save integration remain in
progress. See
[durable local edits](adr/0002-durable-local-edits.md).

A cloud API cannot provide instant uncached access or complete local POSIX semantics.
The service reconciles metadata using incremental delta feeds. OneDrive Socket.IO
notifications now wake those feeds, while polling continues at 30 seconds by default
as a safeguard. A notification is a hint to fetch changes, not file content or proof
that the visible index is current. Provider delivery can itself be delayed; this is
not an instantaneous real-time guarantee. See [change notifications](adr/0003-change-notifications.md).

```mermaid
flowchart LR
    CLI[CLI: connect / status / desired state] --> Settings[Private account settings]
    CLI --> Auth[Browser OAuth + keyring broker]
    Desktop[GTK account overview] --> Settings
    Desktop --> Status[Private status socket]
    Manager --> Status
    Apps[Linux applications] --> FUSE[Read-only FUSE projection]
    FUSE --> Engine[Per-account service]
    Engine --> Store[SQLite metadata and stable inodes]
    Engine --> Cache[Version-keyed disk blocks]
    Engine --> Graph[OneDrive / Microsoft Graph]
    Graph --> Auth
    Settings --> Manager[Mount and worker manager]
    Manager --> Engine
```

## Identity and linked libraries

A scope is `(account, provider, collection)`; Graph collections are drive IDs.
Item IDs are identity, while names and parents are mutable presentation. A shortcut
retains its target drive/item and target kind. Duplicate projections receive distinct
inodes, while cached bytes share the same remote identity and version. Directories
retain their inodes; regular-file inode keys additionally contain the content revision
and size to separate kernel pages belonging to different versions. A metadata-only
rename reuses the inode and bytes when a provider content tag is available.

Resolving an already allocated batch of inode mappings uses read-only queries.
Cached directory listing therefore does not reserve SQLite's writer merely to
return existing identities. Missing mappings still acquire an immediate writer
transaction and recheck every key after admission, preserving concurrent allocation
and duplicate-key identity. An actual-kernel fixture keeps a separate writer
transaction open while directory listing/stat completes within the navigation bound.

Graph packages, such as OneNote notebooks, are projected as read-only child containers.
They have neither a file nor a folder facet; treating that as a malformed entry would
abort a whole delta page or directory. This projection does not implement OneNote
editing or claim compatibility with native notebook applications.

Discovery follows indexed ancestry and starts one delta worker per linked drive.
Reachable roots are persisted; obsolete subscriptions are removed only when the
remaining reachable scopes have complete indexes. Discovery stops at duplicate roots
and limits traversal to 256 roots. Projected ancestry detects cycles and excessive
depth. Invalid/cyclic entries currently produce generic projection warnings; polished
per-entry UI feedback is future work.

A failed linked-drive feed does not stop the primary drive. Cached metadata remains
last-known data. A folder shared without permission to enumerate its entire backing
drive may need foreground directory requests; this case is not claimed as verified
until tested with a real tenant. A cache is not proof of current remote authorization.

## Atomic metadata and foreground observations

Each collection has an optional provider notification session, independently of its
delta worker. A bounded generation signal retains hints received during a refresh.
The worker consumes the generation before starting network work and coalesces bursts
with at least 250 ms between refresh starts. Failed refreshes retain their backoff;
push cannot bypass Retry-After. A single coalescing discovery task prevents push
bursts from accumulating linked-library discovery tasks.

OneDrive obtains the signed Socket.IO endpoint through authenticated Graph, then
uses a separate Rustls WebSocket connection without Graph bearer headers. Engine.IO
heartbeats detect silent disconnects; namespace acknowledgement is required before
reporting connected. The service retries failed subscription attempts and each
successful connection requests a catch-up delta. A healthy session renews after
50 minutes. Message/frame limits and cancellation bound socket work. Connection
state is visible per feed in the status response; it is separate from metadata
freshness. An isolated business-drive check passed the actual renewal and a new
notification afterward; personal accounts and recovery after suspend/network loss
remain live validation gates.

The store stages paginated changes and advances the continuation in one transaction.
Visible nodes and the completed cursor change together only on the terminal page.
A replacement baseline is built alongside the last visible index. Interrupted work
resumes; an expired cursor does not immediately empty a usable directory tree.

Cold foreground listings are atomically recorded separately from the delta index.
Metadata schema 4 introduced a persistent logical revision before each network
observation and feed round; completion wall time is used only for age reporting.
Publication checks the observation's ticket against intervening commits for that
item, its affected directories and its scope. A superseded response yields the
current committed view. If a complete directory view is still unknown, the engine
retries from the first page, at most three times within the total listing deadline.
An item with no current view returns ESTALE rather than discarded metadata.

An incremental round invalidates older observations for changed identities and
their affected parents; an empty or unrelated delta keeps fresher directory
listings. A replacement baseline removes observations whose requests started
before that round. Observations started afterward remain visible across its commit.
Single-item observations overlay cached directory snapshots, including both sides
of a move. A complete listing's absent children are hidden by separate negative
observations; this does not remove rows from the committed delta baseline or claim
that an absent item was deleted remotely. Later observations or feed rounds can
supersede that absence. Directory reads use one database snapshot so their base,
positive observations and negative observations agree.

A single-item NotFound response uses the same ticket ordering. It hides the cached
entry without modifying the completed baseline. A newer positive observation or
complete parent listing supersedes an older NotFound response, so a delayed failure
cannot remove a file that a later request has already observed.

The schema migration preserves existing metadata and seeds the logical clock from
legacy ordering values. The observation database remains separate from the upload
journal. This ordering prevents local publication races; it cannot establish a
globally consistent snapshot across an eventually consistent provider's endpoints.

Metadata schema 5 stores directory snapshot entries as individual indexed rows,
replacing a single JSON array per directory. Reads merge snapshot/index rows and
newer item observations in name/identity order within one read transaction. The
Store visitor decodes one node at a time; tests check that both query paths use
their ordering indexes without a full-directory sort or unrelated-row scan.
Other connections can publish while this reader retains its consistent snapshot.
For a cached read-only OPENDIR, Engine now passes this iterator directly to a
blocking snapshot builder. Projection and persistent inode assignment use batches
of 128 nodes and a separate WAL writer connection; the complete listing is not
retained in a vector. The compatibility `children()` API, point/name lookups,
writable local-overlay projection and cold foreground publication still collect
lists. These remaining consumers keep the full-pipeline memory gate open.
Long-lived read transactions also retain WAL history until released; the visitor
is for bounded local work, never network waits or idle open directory handles.

Schema upgrades recheck the version under the writer lock and commit all migration
steps, validation and the new version together. Invalid legacy arrays, duplicate
entry identities or an unusable metadata clock fail without losing the old schema
and data. Concurrent opens and rollback across schemas 3 and 4 have regression
coverage. Older binaries reject schema 5; deployment rollback must preserve a
compatible metadata backup rather than attempting an in-place downgrade.

The initial WAL transition retries SQLITE_BUSY within a three-second total
deadline before beginning migration. Concurrent journal-mode lock upgrades can
return BUSY without invoking the configured busy handler, as described by
[SQLite](https://www.sqlite.org/c3ref/busy_handler.html). Only that autocommit
initialization step is retried; existing current-schema databases do not reserve
a writer or repeat journal-mode changes when opened. Persistent contention still
returns an error, and the normal per-operation busy timeout is restored afterward.

Reading a directory registers a 60-second activity lease. Each account retains at
most 32 recently used directories and has one revalidation worker, with at least
two seconds between listing starts. A successful listing becomes due again after
five seconds; multiple directories share that budget, so this is not a five-second
freshness guarantee. Background checks do not renew their own lease. The service
knows filesystem activity, not whether a file-manager window stays visible.

Cached listings return immediately while that worker fetches updates. Per-directory
gates coalesce cold foreground work with background checks. Unchanged results do
not emit another filesystem invalidation. Backoff survives repeated directory use;
throttling and authentication failures also postpone other background listings in
the account. Status exposes active, refreshing and delayed directory counts.
Directory fetches have a 60-second total deadline and repeated-cursor/entry limits.
Each listing can require multiple provider pages. The single worker can use one
of the existing foreground metadata slots, leaving capacity for other navigation;
it never starts content downloads. This behavior is shared by provider adapters.

SQLite uses WAL and FULL synchronous mode. Network awaits never occur inside its
transactions. Database work runs on blocking workers. Shortcut discovery starts
at a partial index of actual links and follows their ancestors, so an idle poll
does not walk every file in a large library. Batched inode assignment avoids a
separate database transaction for every directory entry.

## Authentication and ownership

Microsoft authorization code flow uses PKCE S256, random state and nonce, a bound
loopback callback, explicit account selection and validated RS256 OIDC claims.
Issuer, audience, tenant, expiry and nonce are checked before an identity is accepted.
The CLI displays the verified identity and selected drive before saving it.

Non-secret configuration is atomically written and fsynced. Tokens are stored in a
Cirrove-labelled Secret Service item over an encrypted session, never in the metadata
database. Secret writes are explicitly set and read back through a fresh encrypted keyring
session before reporting success. A mismatched readback gets at most three write
attempts; lock and transport errors return to the caller. The shared account broker
serializes refresh, checks the refreshed Graph identity and persists rotation before returning a new access token. Delayed 401s
invalidate only the rejected token, not a newer grant.

A daemon ownership lock prevents competing managers. Per-account leases cover its
worker and filesystem lifetime. Reauthentication first disables that account and
waits for its lease. It verifies the same identity before replacing credentials.
Other accounts keep running. A killed login command can leave that account disabled;
`enable` is the explicit recovery action.

## Transport, responsiveness and content consistency

Persistent reqwest clients reuse connections. Authenticated Graph requests do not
follow redirects; continuation URLs must stay on the configured origin and drive.
Signed downloads use a separate client without Graph bearer headers. Provider bodies,
tokens, signed URLs and opaque cursor material are excluded from application errors.

Per account, there are four foreground metadata slots, four content slots, two
upload slots and one background metadata slot. Transfers cannot occupy directory
request slots. Upload fragments are bounded to 5 MiB and use a separate client that
neither follows redirects nor sends Graph bearer tokens to session URLs.
429/503 cooldown applies across metadata, download and upload operations. Requests and
credential operations have finite deadlines; account cancellation interrupts them.
The scheduler distinguishes authentication, permissions, missing items, expired
cursors, throttling and transient failures, and uses bounded backoff with jitter.

Content cache keys include account, drive, item, version, size and block offset.
Blocks are at most 4 MiB, while an individual read result is capped at 8 MiB. Four
loaders and eight in-memory cache blocks bound content buffering independently of the
remote file's total size; namespace and outstanding application buffers add memory.
A small failure cooldown prevents coalesced failures from becoming a retry storm.

Each uncached Graph range currently uses metadata checks before and after download.
The shared content cache also bounds each provider range call to 30 seconds and
observes cancellation even when an adapter ignores its token. The local cache
publication afterward is awaited rather than abandoned by that provider deadline.
The content revision and size must match the opened version. Cirrove prefers Graph's
content-only cTag and falls back to eTag when absent; the two tag namespaces are
distinct in cache keys. Response range and byte count are
validated before publication. This favors version consistency but adds **two Graph
metadata requests per uncached block**. Removing this request amplification is an
explicit OneDrive 1.0 gate: [read-session efficiency](adr/0004-read-session-efficiency.md)
specifies shared version-bound sessions, bounded transfer windows and validated
renewal. A developer-only conditional-session prototype is now implemented;
ordinary accounts still use the conservative path. A changed file yields ESTALE
instead of mixing observed versions.

The cache coordinates optional provider-neutral read sessions keyed by scope, item,
content revision namespace/value and size. It retains at most 64 entries, evicts
idle entries after 60 seconds on subsequent use, and never evicts in-flight entries.
If every slot is active, another identity uses the bounded original read contract.
Session creation coalesces without keeping the residency map locked during I/O.
Existing versioned disk blocks and publication/recovery rules are unchanged.

The experimental OneDrive session establishes its first range with the existing
Graph before/after checks, then retains the content origin's own strong HTTP ETag.
Later ranges and streamed windows send If-Match and check the effective resource,
returned strong tag, exact range, identity encoding and body length. A rejected/expired URL, changed
resource or changed validator triggers coalesced revalidation of the original
Graph identity. This also permits metadata-only origin-tag changes without leaving
an unchanged content revision permanently stuck. Setup failures share a short
cooldown. Each transport binding has a 60-second lease; the account's existing
content budget, cancellation and provider cooldown remain shared. Initial weak or
missing validators use the original before/after path. Transport URLs and tags are
not persisted or exposed in diagnostics.

Renewal depends on the same Graph revision evidence as initial setup; it is not an
immutable-version API or a transaction across Graph and its content origin. Neither
a signed URL nor a matching ETag across different resource URLs alone proves that
association. Real mutation, URL-expiry, revocation and Personal-account acceptance
are still required before enabling this path in normal accounts.

Experimental sessions optionally stream windows into a caller-owned staging sink
after the first validated read. Strong bindings use the same conditional validation
as ranges, without per-window Graph calls. A rejected or expired binding renews
through the requested window itself: Graph before, one body, Graph after. Ranges
and windows share the renewal gate and failure cooldown. A body or sink error,
cancellation or lease expiry after streaming starts discards the entire window;
a replacement download is never appended to a partially written sink. Weak or
missing validators retain the conservative before/after Graph checks per window.
The first cache miss still fetches one 4 MiB block (or the remaining shorter file). Sequential misses can then grow windows through 8, 16,
32 and at most 64 MiB. Sparse access resets growth. Observed transfer speed caps
growth to a five-second target; this is a sizing hint, not a latency guarantee.
The existing 30-second cache-provider deadline remains. Abandoned or failed windows
reset prediction to small reads; overlapping requests share staging while unrelated
ranges/files can proceed independently.

Staging reserves up to one quarter of the configured cache allowance, rounded to
4 MiB units and capped at 128 MiB. If that would provide less than 8 MiB, windows
stay disabled. The remainder is the persisted-block quota. Reservation exhaustion
uses exact-range reads rather than waiting for another file's window. Existing
single-block publication overhead remains separate from this staging bound.
Temporary files are anonymous/unlinked and are not restart state. Blocking writes
retain both the file and its reservation after their async caller is cancelled.
Adapters deliver at most 64 KiB per sink call. The service computes per-block hashes
while streaming, validates each staged block when reading it, then uses the normal
durable checksum/publication path. No staged byte is readable until the adapter
has validated the complete window against the original content identity. Failure
never promotes partial content into the cache. Uploaded/local working data is not
part of staging and cannot be removed by its cleanup.

A synthetic 1 GiB test now runs the actual OneDrive HTTP adapter through the shared
disk cache: 40 Graph requests and 20 content requests, compared with the previous
512/256 conservative cost, with exactly 1 GiB downloaded. This closes the narrow
sequential-window amplification fixture. Actual-kernel fixtures also verify
overlapping reads, cached navigation during paused windows, failed final validation
and cancellation with open descriptors. Real provider windows, wider application
load/latency and the recovery matrix remain open. Strong conditional windows now
combine two Graph setup requests with twenty content requests for the same mounted
1 GiB workload; they retain staging overhead rather than claiming universal latency
improvement. See
[the measured record](adr/0004-read-session-efficiency.md#bounded-sequential-window-fallback).

Blocks have SHA-256 checksums. Temporary bytes and the containing directory are
fsynced before publication is indexed. Startup removes interrupted temporary blocks,
accounts for orphaned publications and enforces quota. Corrupt blocks are fetched
again. Cached data survives restart, but unpinned blocks may be evicted: this is not
an offline-availability guarantee.

## FUSE and lifecycle

The pure-Rust `fuser` adapter uses the Linux FUSE protocol and `fusermount3`, without
libfuse development headers. Callbacks dispatch work to Tokio and keep namespace
locks short. Directory handles have stable listing snapshots. File handles bind a
content version. Ordinary read calls use kernel direct I/O. Mount initialization
requires `FUSE_DIRECT_IO_ALLOW_MMAP` to support shared read-only and private mappings;
mapping pages occupy kernel/application memory in addition to Cirrove's block cache.
The disk quota is not a limit on total kernel or application memory.

Metadata commits invalidate known paths and attributes. Pages of an older file inode
remain associated with that revision, so an existing mapping never silently reads
bytes from its replacement. An uncached old block cannot be retrieved after the
provider changes that file: reads return ESTALE, and a failing mapped page fault may
deliver SIGBUS to the application. Historical-version downloads are not implemented.
Already cached old bytes remain readable while retained; cache eviction is still
possible. This does not guarantee indefinite snapshots of open files.

The filesystem admits at most 1,024 content requests and runs 32 at a time. Additional
admitted reads await capacity asynchronously for at most 30 seconds; queue expiry is
ETIMEDOUT and admission overflow is EAGAIN. Account cancellation releases active and
queued reads with ENODEV. Content loaders remain limited to four. Metadata requests
have a separate 128-slot budget, so content contention does not consume those slots.
Open directories store their immutable listing in two anonymous temporary files
under the account state directory: compact inode/kind/UTF-8-name records and an
offset index. READDIR cookies are ordinal positions. Positioned reads use at most
1,024 entries and 64 KiB of encoded data per page, with additional bounded index
and decoded-name buffers. They do not share a mutable seek position. Only the
parent and grandparent routes retain full views; plain listing children are not
copied into the mount-wide map or assigned kernel lookup references.

Each mount reserves at most 256 snapshots and 256 MiB of logical data/index bytes,
separately from content and pending-edit storage. Admission returns EMFILE or
ENOSPC instead of exceeding those limits. Physical blocks, filesystem metadata,
SQLite buffers and kernel page cache are additional. Failed or cancelled builders
are never published. RELEASEDIR removes the handle under a short map lock and
closes files on a blocking worker; in-flight readers retain their own references.
The final reference closes anonymous files, also on process exit, without a
restart orphan sweep. An idle handle holds no SQLite transaction. Snapshot
construction still takes work proportional to directory size before the first
entry is returned; bounded resident buffers are not a latency guarantee.

LOOKUP and namespace operations still
publish their resolved views there. File and directory views now retire after the
kernel has released its lookup references and no open file, directory snapshot,
in-flight operation or child view retains a shared residency token. Entry/create
replies acquire references before delivery;
single and batched FORGET release them. Checked accounting preserves an affected
view on underflow/overflow. A deferred collector examines at most 4,096 queued
candidates each second; it never awaits network or storage under the namespace lock.
Replacing an existing inode's visible path preserves its earlier clones' token.
Every projected child also owns a parent residency lease: the retained parent's
entry protects the rest of the ancestor chain. Old and new clones retain their
respective parent leases after a move. Parent validation rejects missing parents
and cycles before updating the map. Callbacks capture parent views before async
dispatch, so a queued operation still has its ancestor route after kernel FORGET.
The root remains resident. This preserves directory '..' and local-edit ancestor
capture while unused directory chains retire through the bounded collector.

Actual-kernel tests cover deep paths, duplicate linked-drive projections, open
files, continued directory snapshots across rename, and offline inode-preserving
revisit after reclamation. They do not prove arbitrary large-library capacity.
Invalidation still walks the resident map. There is no view-payload byte budget,
and failed reply delivery can conservatively
retain references because the FUSE wrapper does not expose delivery results.
Full-pipeline paging, compact referenced payloads, cancellation accounting and
targeted invalidation remain part of the explicit 500,000-file and long-session release
gate; see [namespace memory](adr/0005-namespace-memory.md).

This first projection has read-only permissions and rejects write opens. File-manager
thumbnail generation still causes real content reads; reserved metadata capacity
prevents those requests from consuming folder-request slots. Thumbnail policy,
pinning and per-file status presentation remain later desktop work.

The manager retains running accounts if settings or mount-state observation fails.
Every mount attempt checks for an empty real directory outside other FUSE mounts.
It never mounts over local files deposited after an ejection. Enabled accounts are
remounted after accidental ejection; `disable` records an intentional unmount.
Shutdown cancels and awaits workers, then unmounts and joins FUSE sessions.

The mount root currently has fixed synthetic attributes. Metadata notifications
invalidate projected child entries, but do not invalidate that root inode: an
invalidation racing its first GETATTR can discard the initial ownership/permission
reply. Directory handles do not request kernel readdir caching. If root attributes
become mutable, their refresh needs an initialized-root lifecycle rather than
reintroducing that startup race.

Linux lazy unmount leaves the kernel connection alive while an application retains
a file or directory descriptor. Cirrove therefore retains its own connection's
FUSE control descriptor when mounting, detaches the mount, disconnects that kernel
connection and joins its request threads. The control identity comes from the
session's device fdinfo, or the exact owned mount's device number on older kernels;
it is never rediscovered by path at shutdown. Acquiring the control descriptor is
a mount prerequisite, and missing control access is reported explicitly. This
closes the read-only session even when a preview or shell retains a handle. The
experimental writable-session owner first drains accepted edits and seals dirty
working files; disconnecting is not upload acknowledgement. See
[Linux FUSE connections and control](https://docs.kernel.org/filesystems/fuse/fuse.html).

After a process crash, startup checks a stale control socket under the daemon's
ownership lock and removes it only when a connection is refused. A disconnected
FUSE mount is detached only if its filesystem type and account UUID match and
the kernel reports ENOTCONN. A live mount is never displaced. Kernel tests kill
a synthetic mount process and verify that a new manager serves readable files.
These checks do not imply that a full-machine power-loss test has passed.

## Conditional namespace changes

The provider-neutral mutation contract supports folder creation, rename/move within
one collection, and conditional regular-file removal. Rename/move sends the original
ETag and refuses destination collisions. File removal checks the remote file facet
and original ETag before issuing a conditional DELETE; a fabricated local file type
cannot turn this path into recursive folder deletion. Shortcuts and root changes
are rejected by the current mutation contract.

Upload-journal schema 3 introduced namespace records and a shared sequence/resource queue.
Metadata changes cannot overtake pending uploads on the same remote identity;
source/destination name reservations also order colliding creates and moves.
Case folding is a conservative queue exclusion, not a definition of the provider's
filename equivalence. Receipts and queue completion commit together. Migration
preserves existing upload sequence numbers, snapshots and pending states. Older
binaries refuse newer schemas instead of trying to downgrade them.

After interruption, a namespace operation requires verification. A matching immutable
item at the requested new name/parent can complete a lost rename/move response.
An unchanged original revision permits a new conditional attempt. A missing item
alone does not establish deletion: lost deletes and unidentified folder creations
can enter `NeedsReview`, retaining their request without an automatic retry loop.
HTTP success is required for a confirmed deletion receipt. File DELETE uses the
provider's recycle-bin behavior; it is not permanent deletion or local POSIX rmdir.

Experimental writable mounts now connect regular-file rename/move to this worker.
Experimental FUSE also supports folder creation and dependent child destinations
as described below. Folder rename and safe removal remain unimplemented. Personal
accounts, permissions changes and broader live concurrency still require coverage.

## Experimental local working files

Schema 5 adds mutable working files alongside immutable upload snapshots. Hydration
reserves its declared logical size against the same spool quota before downloading.
The reserved file is filled with bounded, version-checked chunks outside the journal
and namespace locks. Partial sources never become visible working files. Publication
fsyncs the bytes and directory before inserting their identity and local name.
Orphans after a crash remain counted and require later recovery/cleanup handling.
This logical quota does not guarantee available physical disk space.

Before changing a working file, its journal record becomes dirty durably. A failed
write or metadata update cannot leave edited bytes marked as an unchanged cache.
`fsync`, synchronous writes and closing a write handle seal a complete immutable
snapshot. Its queue insertion and the working file's latest-generation link commit
in the same transaction. Closing a read-only preview does not seal another
application's unfinished edit. Local-save success is not cloud acknowledgement.

Consecutive upload generations form a linear dependency chain. A later generation
uses the preceding validated receipt's item ID and ETag, including after a create
assigns a new remote identity. An uncertain or conflicted predecessor blocks its
successors while independent files remain eligible. Old sealed bytes never change
when the working file is edited again. Schema 6 extends this receipt chain across
uploads and namespace mutations. A local working-file relocation now commits its
new name and queued mutation together. Schema 7 adds separate local object identity,
directory entries, confirmed remote bindings and optional working bytes. Regular-file replacement now uses the same namespace; directory
operations and broader application workflows remain under development.

The developer constructor requires a disabled test account with explicit write
access and a journal owned by that account. It supports regular-file create,
write, append, truncate, flush and fsync. Working metadata overlays the cached
remote namespace, so dirty local content stays visible across offline restart.
The normal manager continues to construct read-only filesystems. No application
save on the user's ordinary mount is routed through this experimental path.

Experimental writable inodes retain identity while their local bytes change.
Memory mapping is currently disabled for these sessions; read-only sessions keep
their existing content-version inode and mapping behavior. Folder rename/removal,
permission/time changes and conflict UI are not connected to writable mounts yet.
Sealing may require space
for both the working file and its snapshot; failure
keeps the dirty source and reports an error. Physical power loss, physical disk-full
recovery and sustained real-provider application editing remain acceptance gates.

## Experimental writable-session ownership

`WritableSession` owns a test engine, its FUSE session, two upload workers, one
conditional namespace worker and one local-copy maintenance worker. Successful
sealing, relocation and confirmed receipts
wake the workers; a one-second fallback revisits persisted
retry deadlines without resetting backoff. Saves finish after durable local
publication, independently of cloud acknowledgement. Paginated journal records
expose each generation's state and fragment progress; desktop presentation and
durable actionable error details remain work for the product integration.

Shutdown atomically stops admission of mutating FUSE callbacks and cancels external
work. It waits for every previously admitted callback, then fsyncs and seals dirty
working files. Provider calls and keyring operations are cancellable at the service
boundary, including adapters that fail to observe the supplied token. Workers stop
before the owned kernel connection is detached and disconnected. If a snapshot
cannot be sealed, shutdown still stops the session, returns an error and retains
the dirty working bytes. No network await occurs under the journal lock.

This drain can wait for local disk work: stopping cloud requests does not authorize
discarding an accepted local write. Dropping the session without awaiting its explicit
shutdown is treated as interruption, not successful draining. Restart recovery
retains durable snapshots and dirty working copies. The ordinary account manager
does not select this experimental owner yet.

## Interleaved saves and namespace changes

A single successor relation spans upload and mutation records. It prevents two
different edits from adopting the same predecessor as if both followed it directly.
Neither worker can claim an unresolved descendant. Before selecting more work,
bounded preparation binds ready operations to their confirmed item IDs and ETags
and reserves the resulting identity/name resources. Uncertain or conflicted
operations keep their descendants pending while unrelated files remain eligible.

Working-file relocation seals dirty source bytes, then commits the new local name,
latest-operation pointer and remote mutation intent in one transaction. A failed
transaction keeps the old name and does not consume the predecessor relation. A
collision with another working copy is refused by this relocation API; mounted
replacement uses the separate pair transaction below. Relocating an already clean
working copy creates no content snapshot.
Its original provider metadata is retained separately from the local content tag.
Legacy working copies with a lost original content tag cannot invent that evidence.

A successful conditional mutation response supplies a new base. Reconciliation
after a lost rename response needs an additional check: the same item at the desired
path might already contain somebody else's edit. Changed size or content revision
becomes a conflict, and missing proof becomes `NeedsReview`; neither result completes
the operation queue or authorizes a later upload against that newer ETag. Matching
content revision/size or an unchanged metadata version allows the chain to continue.
Observed receipts remain available for review without being treated as confirmed
save bases. Providers must distinguish conditional responses from later observations.

These APIs establish journal ordering and working-file transactions. The sparse
namespace model below connects regular-file relocation and replacement to FUSE.
Folder rename/removal and complete detached-stream retention/recovery remain
required. The ordinary manager stays read-only.

## Sparse local namespace and mounted relocation

Journal schema 7 separates stable local objects, directory entries, confirmed remote
identities, operation bindings and optional working-file bytes. A metadata-only
rename or move needs no content reservation or download. Its desired path and
queued intent commit together with an optimistic object revision check. Hydrating
that object later attaches bytes to its current local path and identity, including
when another file has since reused its original name. Truncation to zero retains
the original remote version as a write precondition without copying its old bytes.

Schema migration retains earlier acknowledged remote bindings even when a newer
save is still pending. A receipt updates the remote alias in the same transaction
as operation acknowledgement. Remote IDs assigned to newly created files do not
replace their local identity or inode. The sparse namespace currently permits at
most 10,000 objects. Fully acknowledged objects can release their working bytes and
follow later remote metadata, as described below. Their aliases and operation
history remain retained; total-object limits and memory use still need work for
long-term use with large libraries.

Local presentation IDs and provider IDs now have separate lookup paths. Mounted
views, access leases and open-stream callbacks resolve only local identities;
provider observations and initial hydration resolve the current remote binding.
The in-memory projection keeps separate indexes and checks uniqueness within each
domain. A newly confirmed provider ID cannot become an alternate key for an
existing local stream. Listing projection is the boundary that presents a remote
item under its stable local identity. Lookup separation itself requires no journal
migration. The replacement transaction below additionally transfers active ownership
between durable objects while retaining their separate local streams.

The mount loads a memory projection of these objects and working-file records.
Publication is atomic and revision-ordered, so a delayed save or rename callback
cannot restore an older name or discard a newer remote binding. Cached namespace
reads do not take the spool/journal mutex. The database and memory projection use
the same listing logic, suppressing old remote aliases and retaining foreign name
occupants as explicit conflicts. The developer session exposes those conflicts and
paginated mutation state; desktop conflict presentation is not implemented.

The journal supports an immutable case-sensitive or case-insensitive name policy
per collection. Existing experimental working-file calls default to case-insensitive
comparison. The current mounted preflight conservatively compares names without
case; future provider adapters must supply their own verified name semantics.

Regular-file rename/move is admitted and drained with other local mutations. It
rechecks the source name and parent at the journal serialization point. The current
path rejects cross-collection or cross-shortcut-projection moves, directory and
shortcut mutations and exchange/whiteout flags. Replacing an occupied regular-file
path uses the pair transaction below; `RENAME_NOREPLACE` keeps the occupant.
A remote collision after local acceptance becomes an operation conflict; it does
not authorize overwriting that occupant.

Experimental mounts require `FUSE_ATOMIC_O_TRUNC`. Without that capability Linux
can strip O_TRUNC from OPEN and issue SETATTR afterward, causing the old file to be
hydrated unnecessarily. The supported path creates empty working bytes directly
and preserves the source metadata. Read-only O_TRUNC combinations are rejected;
writable memory mapping and other attribute changes remain outside this preview.

## Acknowledged working-copy retirement

Journal schema 8 distinguishes active local objects from aliases that follow remote
metadata. An active object owns its local directory entry and any working bytes.
After every attached operation is acknowledged, no newer dirty generation exists
and no application is using the file, maintenance can detach the local entry and
working copy. The alias retains the same local identity and revision but takes its
visible name, parent and content metadata from the remote index. Later remote
changes and deletions therefore become visible instead of being masked by an old
local copy. The next edit reactivates the object using the newly observed provider
version; it cannot inherit the obsolete pre-retirement save base.

Open handles and in-flight file callbacks hold per-object access leases, including
read-only previews of edited files. Maintenance checks idleness before requesting
metadata but releases exclusive access during the request. Afterward it reacquires
exclusive access and checks the durable object revision and acknowledged frontier.
A new open or edit during that request prevents an obsolete candidate from being
retired. Publication validates the memory projection before committing the detach,
then applies it while both local locks remain held. Retaining the alias revision
prevents delayed callbacks from reinstating retired working bytes.

The detach and explicit cleanup intent commit together. Physical removal verifies
the private working file, deletes and fsyncs its directory, then clears the cleanup
intent. Restart can finish either side of an interrupted deletion. Unknown spool
files remain retained and quota-accounted. A separate bounded collector removes
only acknowledged immutable upload payloads, retaining their receipts and lineage.
Pending, uncertain, failed and conflicted payloads remain protected.

After publishing pending namespace changes, each maintenance pass checks at most
16 active cleanup candidates and requests metadata for at
most one candidate. It reuses cached metadata only when it matches the confirmed
receipt; otherwise it requests an ordered item observation with a 30-second deadline.
An accepted NotFound observation releases an acknowledged overlay of a missing
remote item without preserving a ghost entry. Provider failures keep the local copy
and have per-object retry delays up to 60 seconds. Background work also has a quiet
interval, and wake notifications cannot bypass failure delays. The worker repairs
missed local projection updates from durable records without replaying provider
operations. It participates in session cancellation and shutdown.

This releases working storage, not all historical metadata. Aliases are still
loaded into the in-memory projection, and operation history has no retention policy
yet. Pins, writable mappings and complete application-save semantics remain separate
acceptance gates. Ordinary daemon mounts still use the read-only constructor.

## Unlinked names and retained file streams

Journal schema 9 permits a working stream without an occupied pathname. A regular
file unlink seals earlier dirty bytes, then removes the local entry and queues a
conditional remote deletion in one transaction. Earlier uploads or moves still
provide its required identity and ETag. A new file can use the released name with
its own local identity. The old object remains a tombstone against stale remote
listings. Directories and shortcuts are not accepted by this unlink path.

Open-file registration happens before asynchronous OPEN preparation. Weak stream
references remain alive through callbacks, including reads that outlive removal
of the handle from the open-file map. Old handles keep their object and working
stream; fstat reports zero links, and ftruncate uses the handle's stream. Writes
after unlink remain local recovery data. Their fsync flushes the local bytes and
does not queue another upload that could recreate the deleted cloud file.

An unopened online-only file can be removed without fetching its contents. If an
open stream still needs remote bytes, a persisted local-reader barrier makes its
DELETE ineligible while the local unlink returns. Preservation runs on the tracked
maintenance worker outside Linux's parent-directory lock. It hydrates the original
version, switches new reads to the local stream, and waits for already registered
remote reads before releasing the barrier. The counters registering reads hold no
filesystem lock during network I/O. Other files remain usable during preservation.

An exhausted spool quota or failed download leaves the remote deletion pending;
existing handles can continue to request their original remote version. Once all
readers are gone, deletion can proceed without hydration. Provider failures back
off per object. Shutdown cancels provider I/O and retains the deletion intent.
Reopening the exclusively owned journal releases only the former process's reader
barriers: handles cannot survive a dead FUSE connection. Remote receipt dependencies
and retained working bytes remain intact.

Regular-file replacement extends these open-handle lifetime rules below. Detached working data, tombstones and operation
history currently remain retained. Their recovery UI, bounded cleanup and handling
of a provider restoring the same deleted item identity are still required. Full
application compatibility and real-provider mounted unlink acceptance remain open.

## Journal replacement of two local files

Journal schema 10 adds completion prerequisites independently of the single
content/ETag predecessor. Both upload and mutation selection, including explicit
upload verification, wait for every prerequisite. Edges must reference older
operations in the same scope; each operation permits at most 16 additional edges.
Failed, conflicted and uncertain predecessors keep descendants pending while
unrelated files remain eligible. The original linear content-successor constraint
still determines which confirmed receipt may supply the destination identity.

Schema 11 introduced a journal transaction for replacing one local file with
another using an existing source working copy. Dirty source/victim streams must
be sealed first; schema 13 extends the transaction to online-only sources below. An immutable source snapshot, destination-path
takeover, detached victim stream, conditional destination upload and guarded source
cleanup commit together. The upload follows the victim's last confirmed operation
or original ETag and separately waits for prior source work. Source cleanup follows
its own original identity/ETag and cannot run until destination publication is
acknowledged. A local-reader barrier additionally delays publication; exclusive
journal recovery releases former-process reader barriers without completing cloud
operations or discarding retained bytes.

Active provider ownership is separate from historical remote metadata. On upload
acknowledgement, the victim loses active ownership, the source takes the destination
binding, and a reserved cleanup object takes the source's previous binding, in the
same transaction as upload completion. Existing local IDs and working streams do
not change. Only actively owned provider IDs hide or alias raw provider listings;
an old victim's local ID cannot hide the new destination owner. A unique owner index
bounds binding replacement lookup and prevents two active provider IDs on one object.
Old descriptor
writes remain detached recovery data. Metadata capacity for cleanup is reserved at
local acceptance, before cloud publication, rather than allocated after success.

Schema 12 records a monotonically ordered, coalesced change marker for each local
object. Triggers update the marker in the namespace transaction, so rolled-back
changes cannot be published. A reader takes the complete changed set and matching
working metadata at one committed database frontier. It does not paginate this set:
a page could split a transfer, or omit the new owner from a later replacement.
The sequence index reads only changed objects during ordinary saves; repeated writes
retain one marker per object. The current 10,000-object journal limit also bounds a
batch. Removing that limit will require bounded group publication and history cleanup.

The mount validates the complete batch before changing any in-memory index, removes
old bindings together, then assigns the new owners and advances its cursor. Delayed
callbacks cannot reverse that frontier. Upload callbacks, working-file publication
and maintenance consume this same stream; a callback's operation ID is only a wake
hint. Maintenance can also recover publication missed by an earlier callback. These
updates follow journal-then-projection lock order, with no provider request or file
hydration. SQLite reads and decoding keep the journal stable while allowing cached
projection lookups; the final index update holds the projection lock. Streams remain keyed to their original local
identities.

Experimental FUSE rename now invokes the pair transaction for occupied regular-file
paths in one collection. It preserves both source and victim streams outside VFS
directory locks. `RENAME_NOREPLACE` keeps the occupant; exchanges, directories,
shortcuts and collisions differing only by case remain explicitly unsupported by
this replacement path. Retained history, restored IDs, recovery UI, ordinary editor
and office save patterns, and broader real-provider replacement/cleanup acceptance
remain open. Ordinary mounts stay read-only.

## Deferred source capture for replacement

Journal schema 13 gives an online-only replacement a `Preparing` upload and a
separate source-capture record. Local path takeover and guarded cleanup commit
without downloading content. The source's original version comes from its captured
remote metadata or its own earlier confirmed receipt; the target's predecessor
supplies the destination ETag separately. Later local saves depend on the same
original operation ID, even while capture is incomplete.

The maintenance worker reserves spool space and fetches the captured version using
bounded cache ranges outside journal, projection and kernel directory locks. A
source-version conflict blocks publication and cleanup; transient failures retain
intent and back off. Preparation materializes the source only if it still lacks
working bytes, preserving any newer local edit. The immutable original snapshot
receives a durable checksum before its final filename. Only afterward may the
operation become `Pending`. An interrupted final SQL update can adopt that complete,
checksum-verified snapshot without redownloading it. Partial downloads retain
operation-labelled temporary names; exclusive startup reclaims those known remote
capture temporaries and preserves unknown files and mutable edits. Download
reservations keep the journal ownership lease alive across asynchronous work.

Before target publication, the reader gate switches future source reads to working
bytes, preserves any open victim and drains tracked remote reads on both objects.
After restart, an unmaterialized source keeps a gate for new-process readers;
recovery does not accidentally release it with the old process's handles. A delayed
hydration carries its expected local object through I/O and cannot attach to a
provider identity's newer owner.

Preparation and immutable snapshots share the edit spool quota. A source that cannot
fit keeps its replacement pending and leaves both cloud actions uncommitted. Changing
that quota and explaining recovery in the desktop UI remain unfinished. The source
can change externally before capture completes; no historical-version retrieval or
complete offline availability is claimed. These APIs remain experimental until the
application, provider, fault and capacity acceptance matrix is complete.

## Local directories and pending destinations

Journal schema 14 adds a destination-parent relation separate from the linear
content predecessor. `mkdir` commits a stable local folder and its queued provider
creation together. Its children are immediately visible through the local namespace;
a folder without a confirmed remote identity needs no provider listing. New child
folders, file creates and move destinations capture that parent's current operation.
Several children can depend on the same folder without consuming one another's
save lineage or adopting a folder as a file identity.

Before claiming work, bounded resolution checks the parent's confirmed receipt,
operation ownership, scope and sequence, then binds the actual provider destination
and adds its resource reservations atomically. Both workers wait for unresolved
parents. File moves can independently wait for a source save and destination folder.
Uncertain or conflicted parent creation retains the local tree and its bytes while
unrelated destinations remain eligible. Restart does not infer successful folder
creation or retry an unidentified creation blindly.

The projection keeps local folder IDs across acknowledgement, cleanup and remount.
Provider listings use confirmed remote IDs and translate returned parent references
back to local IDs. Initial edits of remotely created children reverse that translation
before capturing provider metadata. Fully acknowledged folders can follow remote
metadata through the existing handoff path without owning working bytes.

Folder rename, cross-collection movement and `rmdir` are still outside this mounted
implementation. In particular, listing a business folder as empty and conditionally
calling Graph DELETE is not a proven empty-folder-only operation: its folder ETag
does not cover descendant changes, and business folders do not expose a cTag.
Do not implement folder removal by extending the regular-file deletion path.
The existing namespace/history capacity limits, physical-fault checks and broader
application/provider acceptance gates still apply. Ordinary mounts remain read-only.

## Retained routes to local changes

Before accepting new local creates, directory creates, moves/replacements, writable
opens or pathname truncation, the experimental mount captures the already traversed
ancestors in the journal. The source-side entry is retained separately from a
shortcut's resolved target. A single local transaction records folder and link
snapshots; it never queues provider mutations. Captures are limited to 128 path
entries and share the existing 10,000-object journal limit. No view lock remains
held while the database work runs.

A bounded graph walk identifies ancestors needed by active local objects. Ordinary
parents resolve in the local identity domain; links address scoped provider identities
and can cross collections. A visited set prevents recursive processing of cycles.
Unrelated accounts/providers/collections do not share routes. The resulting directory
and name-reservation indexes are cached until namespace publication changes them.
Capturing a path reuses one index, and ongoing writes at an unchanged occupied name
do not rebuild it. New names cannot take over a retained ancestor's path. A provider
occupant at that name remains an explicit collision.

Protected folders and links overlay remote listings even after their removal from
the provider index. They supply local directory attributes; a NotFound directory
request falls back to the local listing. A link target resolves through current
provider ownership before addressing a local stream. Resolved ordinary file targets
can be edited without changing the source shortcut. A locally removed target cannot be reopened through a dangling
shortcut or cached entry; descriptors already open retain their old stream.
Renaming or removing the shortcut entry itself remains unsupported. Parent snapshots cease to override remote
metadata once their dependent objects have been acknowledged and handed off.

This preserves access to locally recorded changes; it is not remote permission or
an offline-content guarantee. Missing ancestors are not recreated, and failed
operations are not redirected or marked complete. Conflict resolution, cold-directory
behavior during other provider failures and recovery presentation remain unfinished.
Older journals lacking a captured ancestor cannot reconstruct its vanished name from
an opaque parent ID alone; their retained bytes still need the recovery/export flow.
Ordinary daemon mounts remain read-only, and these new route/file-link cases have not
been validated against real OneDrive or SharePoint accounts.

## Desktop account overview

`cirrove-desktop` uses GTK4/libadwaita and consumes local settings plus the private
status socket. Tokio workers handle disk/socket work; GTK receives completed
snapshots on its main loop. Polls do not overlap and an operation generation rejects
results started before a mount preference change. The window does not instantiate
a cloud provider or own the daemon lifetime.

Status protocol 1 adds the stable account UUID, drive/root identity and desired
state observed by the manager. The UI matches these with the saved account's path
and signed-in identity. Older services deserialize as protocol 0 and cannot enable
mount controls; a reachable but unmatched account remains unconfirmed. An unavailable
service never leaves a saved connection labelled as currently connected.

Mount actions reuse the CLI's private settings transaction and account-operation
lock, selecting by UUID rather than a mutable label. Saving a preference is distinct
from the service acknowledging the mount. Rows retain widget identity, expansion
and keyboard focus across updates. Raw provider messages are not displayed.
Native authentication, removal, tray integration, pins and conflict UI remain open.

## Next boundaries

Before enabling ordinary writable mounts, complete the application, provider,
recovery and capacity acceptance matrix. The experimental mounted save path now
uses the journal's ordering, conditional uploads and conflict preservation;
synthetic kernel tests cannot establish real-provider reliability. Local-save
success remains separate from remote acknowledgement. GTK settings, tray and Nautilus integrations
must consume the service's state rather than maintain their own sync logic.

Google Drive will implement provider contracts around its native changes and content
APIs, including shared drives and explicit document export. iCloud must stay isolated
behind a compatibility adapter with visible authentication/API limitations.

## References

- [Microsoft authentication code flow](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow)
- [Graph delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- [Graph Socket.IO notifications](https://learn.microsoft.com/en-us/graph/api/subscriptions-socketio?view=graph-rest-1.0)
- [Graph throttling](https://learn.microsoft.com/en-us/graph/throttling)
- [Graph conditional move](https://learn.microsoft.com/en-us/graph/api/driveitem-move?view=graph-rest-1.0)
- [Graph conditional deletion](https://learn.microsoft.com/en-us/graph/api/driveitem-delete?view=graph-rest-1.0)
- [Graph downloads](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)
- [Graph content and metadata tags](https://learn.microsoft.com/en-us/graph/api/resources/driveitem?view=graph-rest-1.0)
- [Graph packages](https://learn.microsoft.com/en-us/graph/api/resources/package?view=graph-rest-1.0)
- [Graph child containers](https://learn.microsoft.com/en-us/graph/api/driveitem-list-children?view=graph-rest-1.0)
- [Linux FUSE I/O and memory mapping](https://docs.kernel.org/filesystems/fuse/fuse-io.html)
- [Google change tracking](https://developers.google.com/workspace/drive/api/guides/manage-changes)
- [Apple CloudKit](https://developer.apple.com/documentation/cloudkit)
- [Rclone iCloud compatibility notes](https://rclone.org/iclouddrive/)
