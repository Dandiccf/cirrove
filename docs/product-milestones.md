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
- [x] Remove Graph metadata checks per cache block from the validated read-session
      fast path; implement and measure shared session setup, bounded sequential
      windows and safe renewal as specified in [read-session efficiency](adr/0004-read-session-efficiency.md).
- [ ] Push-triggered metadata updates, reconnection/catch-up and periodic recovery checks, with provider delivery and local reaction latency measured separately.
- [ ] Bounded memory and background work with large libraries and long sessions.
- [ ] Reclaim inactive namespace views with correct FUSE lifetimes; pass the
      500,000-file traversal, invalidation and 24-hour churn memory gates in
      [namespace memory](adr/0005-namespace-memory.md). Content-cache limits do not
      satisfy this requirement.
- [ ] Real restart/outage checks and at least 24 hours of sustained operation.

Evidence must include actual kernel mounts, provider-backed reads and ordinary
desktop applications, in addition to deterministic transport/recovery fixtures.
The first namespace-memory correction removes mount-lifetime retention of entries
only returned by plain READDIR. Resolved file and directory views now retire after
kernel lookup references and open/in-flight/child leases end. Parent leases retain
required ancestor routes; actual-kernel tests cover deep paths, distinct shared
links, held snapshots and offline revisit. The 500k-file fixture returns to one
root view after invalidation in each of three passes, but process RSS still rises
across those passes. View byte budgets, compact referenced payloads, complete
directory-pipeline paging, memory-slope attribution and broader interrupted/failed-reply
accounting remain open. A synthetic kernel fixture now verifies cleanup after a
client is killed during delayed cold LOOKUP and OPENDIR, including offline metadata
revisit. It does not inject failed reply writes or cover experimental CREATE.
These partial corrections do not close the 500k-file/long-session gate; see
[the measurements and their limits](benchmarks/namespace-parent-lifetime.json).
Store directory snapshots now use indexed rows and a streaming visitor, avoiding
the earlier JSON-array decode and whole-directory map merge. Tests cover ordered
reads without a SQL sort, concurrent publication and atomic migration rollback.
Cached read-only OPENDIR now streams into anonymous disk snapshots; READDIR uses
bounded positioned pages, with a separate per-mount logical storage/handle budget.
Cached read-only name lookups now use indexed matches while preserving listing
visibility, and known absent names avoid provider calls. An actual-kernel release
fixture performs 17 direct metadata requests in a 500k-file directory in 11.06 ms,
without a directory snapshot or provider calls; see
[the measured scope](benchmarks/indexed-name-lookups.json).
Cold foreground publication now stages provider pages in bounded temporary SQLite
storage and atomically commits the complete listing, with stale-response checks,
absence/move semantics and cancellation. A synthetic 500k-file cold mount, offline
revisit and remount pass; writable overlays and compatibility APIs still materialize
lists. Read-only snapshot construction now publishes an immutable flushed prefix
before completing the scan; waiting readers distinguish completion from late errors.
A controlled pair of frozen release binaries opens a single 500,000-file directory
alternately in one session: the first entry takes 8.80-10.76 seconds before this
change and 4.33-5.23 ms after it, against an unchanged 500 ms bound, while whole-pass
traversal stays the same. Sustained acceptance and the memory gate remain open. See
[the giant-directory measurements](benchmarks/early-directory-prefix.json).
This remains partial progress toward the paging and memory gate; see
[foreground publication measurements](benchmarks/directory-publication.json).
Separate three-pass kernel runs now cover 500k files in one directory and in
500 directories, with snapshot storage fully released after close. Those earlier
completed-snapshot measurements took 5.49–7.31 seconds to open the giant directory,
and process RSS rose across passes; see [the measurements](benchmarks/directory-snapshot-pages.json).
Metadata changes now select affected live projections through revision and identity
indexes, including shortcut sources, targets and old content versions. Kernel
notifications are batched outside the view lock; local namespace and recovery events
retain a coalesced full sweep. Synthetic mount tests cover targeted updates, bursts,
held versions and alias/scope changes. Resident payload budgets and combined sustained
load acceptance remain open; see [measurements](benchmarks/targeted-invalidation.json).
Resolved view clones now share immutable metadata and sibling routes, including
scopes used by the live invalidation index. A representation benchmark measures the
reduction; persistent inode keys, old content versions and parent leases retain their
semantics. Resident byte budgets, reconstructible evicted views and the combined
large-library/24-hour gate remain open; see
[shared-payload measurements](benchmarks/shared-projection-payloads.json).
The resident representation now removes duplicate inode-index storage, boxes uncommon
link-target data and reuses exactly equal live alias payloads. A 500k projection-only
comparison reduces RSS by about 20 percent, with somewhat longer population time and
high RSS after retirement. It introduces no new cache or disk state. Actual-kernel,
byte-budget and sustained acceptance remain open; see
[the comparison and its limits](benchmarks/compact-resident-metadata.json).
A combined kernel workload runner now stats all projected files across deep paths
and duplicate aliases, preserves held old files/snapshots during updates, and checks
offline remount. Its sustained mode measures a fixed active set without forced full
sweeps each round. Separate 500k indexed/750k projected comparisons now pass three stat traversals
and offline remount for both many-directory and giant-directory layouts. The
compact representation reduces final released RSS by about 15 and 18.7 percent,
respectively, while references retire to the root. These zero-byte fixtures do not
close the combined content, 24-hour, byte-budget or reconstruction gates; see
[the measurements and their limits](benchmarks/compact-namespace-churn.json).
A shared conditional read-session prototype now removes per-block Graph checks in
an explicit developer path. A synthetic 1 GiB adapter read uses two Graph requests
instead of 512; an isolated business-file comparison also verifies subsequent
conditional ranges without additional Graph calls. Bounded disk-staged windows now
also reduce the conservative fallback to 40 Graph calls in the actual-adapter/shared
cache 1 GiB fixture. The narrow eightfold-reduction gate passes; the full provider
matrix and wider application latency/resource measurements remain open. Actual-kernel
fixtures now cover overlapping window readers, cached navigation during stalled
transfer/final validation, version rejection and shutdown with open descriptors;
they do not yet cover live indexing or desktop thumbnail load on the optimized path.
Cached inode batches now avoid waiting for the SQLite writer when their mappings
already exist. Both store and actual-kernel tests hold a competing writer while
reading those mappings; this protects cached navigation during publication. The
full indexing and load gate remains open.
Ordinary accounts still use the original conservative path pending that acceptance.
A separate mounted application workload now verifies cold 3.1 MB reads, reopens,
sparse seeks, concurrent readers and sequential 1 GiB reads with actual adapter/HTTP
counts. It closes the narrow request-count and stable-session implementation gates,
but retains the live-provider, indexing and thumbnail-decoder gates. Measurements
identified one content request per block on the strong range path. Conditional
windows now combine two Graph setup requests with twenty content requests in the
mounted 1 GiB fixture. Shared renewal and partial-transfer discard have adapter and
kernel coverage; windows still trade extra local staging for fewer network round
trips, and the real-provider acceptance gates remain open. See ADR 0004.
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
The extended developer validator is ready to exercise atomic replacement and
conditional source cleanup in a fresh OneDrive test folder, including an online-only
source after remount. That expanded live sequence ran on 2026-09-09 and passed,
after a third direct save was added that shrinks the file in place — truncation
was named by the box and exercised by nothing until then.

Fully acknowledged working copies can retire after the last user closes, retaining
local identities that follow remote changes. Restartable cleanup, generation fencing,
checksums, quota failures and interrupted final publication have synthetic checks.
Experimental folder creation now supports pending nested folders, sibling file
saves and file moves whose destinations are still awaiting cloud confirmation.
Synthetic journal and actual mount tests cover independent progress, retained bytes
after uncertain creation, stable identities across remount and editing remote
children after local cleanup. This has not been exercised against a live provider.

Experimental edits now retain their traversed ancestor routes, including source-side
SharePoint links, when local work depends on them. Synthetic mounts verify access
after remote metadata removal and restart, without recreating cloud folders. Resolved
file-link targets also have a local-edit/recovery fixture. Live acceptance and a
complete recovery/conflict flow, including older journals without captured paths,
remain outstanding.

A writable mount now has capacity coverage, which it had none of: every fixture in
`filesystem::capacity` mounted read-only, and the writable path is not the same one.
It drops the content revision and size from its inode key, so a file's revisions
collapse onto a single inode instead of each getting its own, and it puts a
`Writeback` overlay in front of every listing, so namespace retention measured
read-only did not transfer to it. `real_writable_namespace_retires_and_stays_bounded`
holds 20,021 views and retires to the root on each of three passes, with no provider
request and no content read. It is deliberately moderate in scale and does not
measure a 500,000-file library or a memory budget; it establishes that the writable
regime retires at all, which was previously unknown.

The normal daemon remains read-only. Folder rename/removal, broader ordinary editor
and office behavior, live provider replacement/unlink scenarios, physical-fault
coverage, restored remote identities, detached-data recovery and bounded long-session
history remain acceptance gaps. This implementation progress does not close the
safe-file-changes milestone.

- [x] Provider-neutral create, update, rename, move and delete contracts (regular-file deletion; folder removal remains an explicit gap).
- [x] Durable local file contents and journal before local-save acknowledgement.
- [x] Persisted upload progress, resumable transfers and idempotent recovery.
- [x] Version-conditional changes and preservation of both sides of conflicts.
- [x] Distinct local-save, pending-upload, uploading, uploaded and failure states.
- [x] Correct application save patterns, truncation and atomic replacement.
- [ ] Crash/fault tests at every durable transition and concurrent remote edits.
- [x] Opt-in write consent and live tests in a dedicated Cirrove test folder.

Writing throughout a work drive is not enabled on the strength of synthetic tests.
Live mutation fixtures must be isolated from the user's existing documents.

## 3. Offline availability

- [x] Persistent per-file and recursive-folder pinning with storage reservations.
- [x] Clear unpin/free-space behavior and accurate availability status.
- [x] Offline access and editing of pinned content, including across restart.
- [x] Unsent changes excluded from cache eviction and connection cleanup.
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

The first GTK4/libadwaita window now displays saved accounts and confirmed service
state, changes the desired mount preference by account UUID, and opens confirmed
mounts in Files. Slow status I/O stays off GTK's main loop. Synthetic native-window
tests cover mount acknowledgement and keyboard focus. This is an initial settings
slice, not the complete setup experience; see [Desktop preview](desktop.md).
Settings/status snapshots now retain sanitized failure categories instead of
discarding their causes. The window distinguishes unreadable or invalid settings,
service connection/timeout/response errors and incompatible protocol versions,
including simultaneous settings and service failures and recovery through Retry.
Native synthetic checks cover the rendered messages. Save/open operation errors,
account repair and the wider recovery flow still need implementation and acceptance.

- [ ] GTK4/libadwaita setup and settings without a terminal in ordinary flows.
- [ ] Account picker, mount controls, reconnect, connection removal and cleanup.
- [ ] Tray status and actions using the daemon as the source of truth.
- [ ] Nautilus badges, pin/unpin actions and consistent status refresh.
- [ ] Actionable errors, progress, cancellation and conflict resolution.
- [ ] Keyboard navigation, accessibility, localization and visual verification.
- [ ] Verify a declared session matrix covering native Wayland and X11, and GNOME
      and KDE Plasma: windows, dialogs, system dark-style preference and
      portal-backed folder opening, including cancellation and missing-portal
      behavior. Record the actual backend and supported session combinations;
      Xwayland is not native Wayland coverage. Current CI uses Xvfb (X11) only
      and does not exercise a complete GNOME or Plasma session.
- [ ] One application identity across the desktop entry, installed application
      icon, AppStream metainfo, desktop application's D-Bus name and Wayland
      `app_id`. Verify launcher/window association, including X11, on every
      declared shell; naming consistency alone is not a runtime check.
- [ ] Install application-specific scalable and symbolic icons in standard theme
      locations, replacing the generic application icon. Check dock, launcher
      and software-centre presentation, including symbolic recoloring.
- [ ] Implement the tray as StatusNotifierItem over D-Bus. Document the GNOME
      extension requirement, detect missing tray support and keep all actions
      available from the window when no tray host is present.
- [ ] Publish a supported file-manager list beyond the initial Nautilus target,
      with an explicit Dolphin decision and a shared daemon status contract
      behind each integration. Unsupported managers must still access mounted
      files; badge/control availability must be explained.
- [ ] Preserve actionable, sanitized error causes in the window. Unreadable or
      invalid settings, an unreachable service and an incompatible service remain
      distinguishable, with recovery actions and diagnostic detail appropriate
      to the failure. Never expose credentials or raw provider responses.

The UI must distinguish online-only, cached, pinned, pending, transferring,
conflicted and failed states without reporting unsent content as uploaded.

Cross-desktop reach depends on freedesktop interfaces, available desktop services
and the daemon's status contract; the toolkit alone cannot establish it.
StatusNotifierItem is a de facto D-Bus tray interface, independent of the display
protocol, not a Wayland tray protocol. Stock GNOME Shell needs an extension for
this interface; distributions may already supply one. See the
[KDE interface](https://api.kde.org/kstatusnotifieritem.html) and
[GNOME Shell extension](https://github.com/ubuntu/gnome-shell-extension-appindicator).
File-manager integration uses manager-specific APIs, such as
[Nautilus InfoProvider](https://gnome.pages.gitlab.gnome.org/nautilus/iface.InfoProvider.html)
and [KIO overlay plugins](https://api.kde.org/koverlayiconplugin.html).
In-process extensions must stay small and responsive, delegating status/work to
the daemon; each manager needs an adapter, not necessarily a different language.
Tray and file-manager extensions are optional surfaces: no control or state may
be reachable only through a tray or only through one file manager.

## 6. Installable OneDrive 1.0

- [ ] Native Arch/AUR, Debian/Ubuntu `.deb` and Fedora `.rpm` packages from the same
      release, with the clean-system checks in [Distribution](distribution.md).
- [ ] Signed APT and COPR update channels, release artifacts, source/provenance,
      supported-version matrix and measured reproducible build procedure.
- [ ] Fresh installation through sign-in and reboot verified outside development.
      Validate this on each declared distribution family, including Fedora with
      SELinux enabled; an Ubuntu CI build is not an installation check.
- [ ] Upgrade/migration rollback protects settings, credentials and pending work.
- [ ] Clean uninstall and explicit retention/removal choices for local data.
- [ ] User documentation, redacted diagnostics and supported-version policy.
- [ ] Dependency/security review, extended testing and tracked release blockers.
- [ ] Validate the desktop entry, installed icons and AppStream metainfo in CI,
      then compare their identity with the running window and application bus
      name in the supported desktop sessions. Current desktop-file validation
      alone does not satisfy this gate.
- [ ] Build and install the daemon and CLI without GTK4/libadwaita present, with
      separate desktop packaging. The root's default members now exclude the
      desktop; explicit `--workspace` builds and full contributor checks still
      include it. A package installation on a clean headless host remains required;
      headless buildability does not imply unattended browser/keyring setup.
- [ ] Tagged release and verified installation from the published artifacts.

## Platform integration constraints

These findings constrain milestones 5 and 6; they are not completion claims for
the gates above. They describe Cirrove's intended host service and ordinary
Flatpak application sandboxes, not every possible container or privileged helper.

- Bubblewrap creates a separate mount namespace. A filesystem mounted there does
  not by itself become a host-visible Cirrove mount; directory access permissions
  are not a mount-export interface. Cirrove therefore needs a host-side mount
  service for ordinary host applications. The document portal is an example of a
  separate FUSE service exporting selected documents to applications, not a
  general API for exporting an application's arbitrary mount to the host. See
  [Bubblewrap's model](https://github.com/containers/bubblewrap#usage), its
  [mount propagation setup](https://github.com/containers/bubblewrap/blob/main/bubblewrap.c), and
  [Documents and FUSE](https://flatpak.github.io/xdg-desktop-portal/docs/documents-and-fuse.html).
- `NO_NEW_PRIVS` prevents gaining privilege from setuid execution. Where mounting
  relies on a setuid `fusermount3`, running that helper in such a sandbox does not
  supply its host privilege. Cirrove additionally requires access to its own
  `/sys/fs/fuse/connections/.../abort` control descriptor for shutdown with open
  files. Ordinary Flatpak permissions do not provide that access by default.
  Do not assume all sandboxes hide all sysfs, or all distributions install the
  helper identically. See the [kernel contract](https://docs.kernel.org/userspace-api/no_new_privs.html)
  and [Flatpak permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html).
- The daemon's user service, host-mounted filesystem and host file-manager
  extensions need host installation and lifecycle management. The current
  credential adapter talks to the desktop Secret Service. A sandboxed frontend
  can consume permitted host APIs; it does not remove these host dependencies.
  Native packages are the release model. An optional separately packaged frontend
  could be evaluated later with an explicit host-service contract; this is not a
  claim that every bundled application is inherently unable to use host services.
- GTK4 provides no `GtkStatusIcon`; the tray implementation needs a separate
  StatusNotifierItem client. Its D-Bus interface must work under both display
  protocols and must not become the only route to configuration or recovery.
- Toolkit version features establish an API minimum, not a runtime version pin.
  The current GTK 4.14/libadwaita 1.5 floor constrains supported native packages;
  compatible libraries loaded at runtime come from the distribution. Validate
  portal backends and theme preferences separately from GTK compilation.

Supporting code observations from this review:

- The [desktop entry](../packaging/desktop/io.github.Dandiccf.Cirrove.desktop)
  uses `Icon=folder-remote`. Its basename already matches the application ID in
  [main.rs](../crates/cirrove-desktop/src/main.rs), but a branded installed icon,
  AppStream metainfo and actual shell/window identity checks are still missing.
- [model.rs](../crates/cirrove-desktop/src/model.rs) now preserves typed, sanitized
  settings/status causes and protocol mismatch details. The initial `Result<_, ()>`
  loss is corrected for snapshots; save/open operation errors and complete recovery
  actions remain incomplete. See [Desktop preview](desktop.md).
- [Cargo.toml](../Cargo.toml) now selects the non-GTK crates by default. The
  explicit `--workspace` flag overrides that selection, so full CI still needs
  the desktop development libraries. See
  [Cargo package selection](https://doc.rust-lang.org/cargo/reference/workspaces.html#package-selection).
- [docs/roadmap.md](roadmap.md) is the canonical engineering roadmap. Any local
  root-level convenience copy should link to it and this milestone plan instead
  of maintaining another stage list.

## Provider extensibility throughout

Account lifecycle, filesystem projection, cache, journal and desktop status are
shared. Adapters implement identity, changes, transfer operations, errors and
capabilities. Microsoft-specific semantics stay in the OneDrive adapter.
Write and conflict contracts must be concrete and exercised, not placeholder APIs.

A small Google Drive integration will validate the shared boundaries once the
OneDrive core is stable. Shared drives and document exports require explicit
capabilities. iCloud has a separate feasibility gate before feature parity is
promised. Completion of OneDrive 1.0 does not claim either adapter is finished.
