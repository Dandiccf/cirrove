# OneDrive 1.0 product milestones

This is the acceptance plan for the six product milestones agreed with the user.
The earlier [engineering roadmap](roadmap.md) describes the existing foundation;
its implementation checkmarks do not imply completion of these release milestones.
All six remain open until their acceptance evidence is recorded and reviewed.
Private account measurements belong in local records, not the public repository.

## 1. Reliable read-only foundation

- [ ] Installable user service, login startup and clean intentional shutdown.
- [ ] Recovery after process failure, suspend/resume and loss of network access.
- [ ] Real token expiry/refresh and visible reauthentication when consent expires.
- [ ] Responsive navigation during initial indexing and competing downloads.
- [ ] Push-triggered metadata updates, reconnection/catch-up and periodic recovery checks, with provider delivery and local reaction latency measured separately.
- [ ] Bounded memory and background work with large libraries and long sessions.
- [ ] Real restart/outage checks and at least 24 hours of sustained operation.

Evidence must include actual kernel mounts, provider-backed reads and ordinary
desktop applications, in addition to deterministic transport/recovery fixtures.
Graph Socket.IO and provider-neutral coalescing hints are now implemented. This
does not close the live-response gate: notifications can be delayed upstream.
An isolated business-drive run passed actual long-session renewal and subsequent
notification delivery; the provider and outage-recovery matrix remains open. Bounded
revalidation of recently used directories now has service and actual FUSE fixtures,
plus a limited real business-drive create/rename check through an isolated mount.
Larger provider scenarios and ordinary desktop freshness still need measurement. See
[the notification decision](adr/0003-change-notifications.md).

## 2. Safe file changes

The [local edit journal](adr/0002-durable-local-edits.md) protects mutable local files
and immutable save generations. Uploads, moves, deletion and replacement use a
shared resource queue with confirmed receipt ordering. Uncertain and conflicted
operations retain data and block dependent changes. Synthetic transport and journal
checks cover bounded upload fragments, resume, lost replies, conditional conflicts,
transaction rollback and namespace publication. The isolated business-drive check
has exercised basic generated-file operations, a competing remote edit, and two
actual mounted saves with automatic uploads and independent verification.

Experimental FUSE supports regular-file create/write/truncate/fsync, metadata-only
move, unlink with retained descriptors, and replacement of both local and online-only
sources. An online-only replacement first commits its namespace and preparation
intent, then captures the original source version outside the kernel directory lock.
Target publication and source cleanup use separate identities and prerequisites.
Old readers are preserved, and later local edits cannot overwrite the earlier
snapshot. Joint namespace publication handles delayed callbacks and chained transfers.
Actual synthetic mount fixtures cover two consecutive atomic saves, paused downloads,
independent saves during old reads, and remount after interrupted preparation.

Fully acknowledged working copies can retire after the last user closes, retaining
local identities that follow remote changes. Restartable cleanup, generation fencing,
checksums, quota failures and interrupted final publication have synthetic checks.
The normal daemon remains read-only. Writable directories, broader ordinary editor
and office behavior, live provider replacement/unlink scenarios, physical-fault
coverage, restored remote identities, detached-data recovery and bounded long-session
history remain acceptance gaps. This implementation progress does not close the
safe-file-changes milestone.

- [x] Provider-neutral create, update, rename, move and delete contracts (regular-file deletion; folder removal remains an explicit gap).
- [ ] Durable local file contents and journal before local-save acknowledgement.
- [ ] Persisted upload progress, resumable transfers and idempotent recovery.
- [ ] Version-conditional changes and preservation of both sides of conflicts.
- [ ] Distinct local-save, pending-upload, uploading, uploaded and failure states.
- [ ] Correct application save patterns, truncation and atomic replacement.
- [ ] Crash/fault tests at every durable transition and concurrent remote edits.
- [x] Opt-in write consent and live tests in a dedicated Cirrove test folder.

Writing throughout a work drive is not enabled on the strength of synthetic tests.
Live mutation fixtures must be isolated from the user's existing documents.

## 3. Offline availability

- [ ] Persistent per-file and recursive-folder pinning with storage reservations.
- [ ] Clear unpin/free-space behavior and accurate availability status.
- [ ] Offline access and editing of pinned content, including across restart.
- [ ] Unsent changes excluded from cache eviction and connection cleanup.
- [ ] Disk-full recovery preserves edited data and explains required action.

## 4. Complete OneDrive coverage

- [ ] Personal and business accounts, including multiple simultaneous accounts.
- [ ] Explicit account, drive and SharePoint library selection.
- [ ] Linked folders, duplicate links, moved/deleted links and folder-only access.
- [ ] Per-item capabilities, restricted permissions and revoked-access behavior.
- [ ] Remote creates, edits, moves and deletions update mounted views correctly.
- [ ] Defined filename, package/notebook, trash and unsupported-operation behavior.
- [ ] Documented compatibility matrix backed by real-account checks.

Full integration means the supported filesystem feature set works consistently;
unsupported Microsoft features are documented rather than silently emulated.

## 5. Polished desktop experience

- [ ] GTK4/libadwaita setup and settings without a terminal in ordinary flows.
- [ ] Account picker, mount controls, reconnect, connection removal and cleanup.
- [ ] Tray status and actions using the daemon as the source of truth.
- [ ] Nautilus badges, pin/unpin actions and consistent status refresh.
- [ ] Actionable errors, progress, cancellation and conflict resolution.
- [ ] Keyboard navigation, accessibility, localization and visual verification.

The UI must distinguish online-only, cached, pinned, pending, transferring,
conflicted and failed states without reporting unsent content as uploaded.

## 6. Installable OneDrive 1.0

- [ ] Arch package/AUR recipe, release artifacts and reproducible build procedure.
- [ ] Fresh installation through sign-in and reboot verified outside development.
- [ ] Upgrade/migration rollback protects settings, credentials and pending work.
- [ ] Clean uninstall and explicit retention/removal choices for local data.
- [ ] User documentation, redacted diagnostics and supported-version policy.
- [ ] Dependency/security review, extended testing and tracked release blockers.
- [ ] Tagged release and verified installation from the published artifacts.

## Provider extensibility throughout

Account lifecycle, filesystem projection, cache, journal and desktop status are
shared. Adapters implement identity, changes, transfer operations, errors and
capabilities. Microsoft-specific semantics stay in the OneDrive adapter.
Write and conflict contracts must be concrete and exercised, not placeholder APIs.

A small Google Drive integration will validate the shared boundaries once the
OneDrive core is stable. Shared drives and document exports require explicit
capabilities. iCloud has a separate feasibility gate before feature parity is
promised. Completion of OneDrive 1.0 does not claim either adapter is finished.
