# 0016: Explore a direct Linux iCloud Drive adapter against the web transport

Status: proposed; corrected after surveying existing Linux clients and inspecting a
working Fedora/GNOME installation on 2026-09-24.

## Product goal and correction

Cirrove's goal is to show a person's **existing iCloud Drive files on Linux** in the
same provider-neutral filesystem used for OneDrive and Google Drive. A requirement
for a running Mac would not meet that goal. A Cirrove user has an existing
Fedora/GNOME installation whose iCloud files are visible and writable in Files.
Read-only inspection over Tailscale established that `~/iCloud` is a writable
`fuse.rclone` mount from `icloud:`; `rclone-icloud.service` runs it. A separate
`rclone-icloud-photos.service` exposes iCloud Photos read-only. The machine is
Fedora 44 with GNOME 50.5 and rclone 1.74.3. This is first-hand evidence for a
working Linux iCloud Drive mount, not evidence for a built-in GNOME provider or
for Cirrove's own recovery guarantees. No credentials or cloud files were read.
The 2026-09-27 read-only follow-up confirmed that the service still runs and
uses `--vfs-cache-mode full`, `--dir-cache-time 1h` and `--poll-interval 0`.
Only those allowlisted flags were read from the process command line; its
configuration, credentials, cache and mounted data were not inspected.

The first version of this ADR proposed a macOS companion because Apple does not
publish a general iCloud Drive file API. That conclusion about Apple's published
APIs remains true, but it overlooked working Linux clients. Rclone has an iCloud
Drive backend; pyicloud exposes listing, download, upload, rename and delete; and
icloud-linux builds a local-first FUSE projection. They use iCloud's web transport
rather than CloudKit. The direct Linux route is therefore technically plausible
and deserves a measured feasibility spike. The Mac companion is a fallback for a
future, separately scoped product, not Cirrove's planned iCloud route.

Cirrove's iCloud implementation must be independent: no rclone executable,
library, daemon, mounted filesystem, configuration or credentials in its runtime,
build, tests or sign-in flow. Existing clients are research references and the
Fedora mount is feasibility evidence only. Cirrove owns its protocol adapter,
authentication, metadata, reads and (if proven safe) writes.

Apple's [CloudKit](https://developer.apple.com/documentation/cloudkit) addresses an
app's own containers and does not expose the person's general Drive tree.
[File Provider](https://developer.apple.com/documentation/fileprovider) publishes an
app's remote storage to Apple's file browsers. Neither is a substitute for the
Drive transport the existing Linux clients use.

## What the existing implementations establish

| Implementation | Evidence for existing Drive files | What Cirrove can learn |
| --- | --- | --- |
| [rclone iCloud Drive](https://rclone.org/iclouddrive/) | Direct Linux backend, FUSE mount and read/write operations | Current authentication and web request shape; opaque IDs and ETags in its [Drive backend](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/iclouddrive.go) and [web transport](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/api/drive.go) |
| [pyicloud](https://github.com/picklepete/pyicloud#file-storage-icloud-drive) | Lists, streams and changes files under `api.drive` | Independent confirmation of direct access, but not Cirrove's identity or recovery guarantees |
| [icloud-linux](https://github.com/IsmaeelAkram/icloud-linux) | Cached Linux FUSE filesystem with background sync | Usability patterns and conflict copies, not proof of Cirrove's provider contracts |
| User's Fedora/GNOME installation | `findmnt` identifies `~/iCloud` as writable `fuse.rclone`, with an active rclone iCloud Drive service; Files shows it as a local mount | Gives Cirrove a real Linux behavior baseline, but not a reason to reuse another client's config or mount |
| [GNOME Online Accounts](https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/services.html) | The current published provider/service table has no Apple or iCloud Drive Files provider | Published upstream capabilities do not identify the backend of this user's working installation |

The GNOME table is for its current upstream implementation. The inspected Fedora
setup uses rclone FUSE, so the iCloud entry in Files does not imply a GOA iCloud
provider. Do not inspect credentials, dump account configuration, or alter the
existing rclone mounts or services. Apple's
[third-party account authorization](https://support.apple.com/121539) describes
Mail, Calendar and Contacts, which are different services from Drive files.

Rclone's backend is the most useful transport reference, but it is not a drop-in
Cirrove provider. Its code gives Drive items `drivewsid`, `docwsid`, `item_id`,
parent ID and ETag. It lists children through `retrieveItemDetailsInFolders`,
obtains a download URL by item ID and can request byte ranges. The current code
reports no content hash. It does not expose a Drive change cursor through the
backend. Its `Update` first moves the old item to Trash and then uploads a new
item, so its replacement behavior cannot be adopted for Cirrove's durable local
save contract. Its rename, move and trash helpers can retry an ETag conflict with
the new ETag when `force` is true; Cirrove must instead surface that conflict.
These are source observations, not yet live Cirrove results.

Recent [shared-folder failures](https://github.com/rclone/rclone/issues/9477) and
[2FA failures](https://github.com/rclone/rclone/issues/9730) in rclone's own tracker
show why each account class and operation needs evidence. These reports do not
prove that all accounts fail.

## Authentication boundary

Rclone's current [configuration guide](https://rclone.org/iclouddrive/) says the
web flow uses the ordinary Apple Account password, SRP and two-factor approval;
app-specific passwords do not work for Drive. It reports a trust token valid for
about 30 days, after which reauthentication is needed. Advanced Data Protection
requires web access to be enabled and may require device approval for PCS cookies.
This is not the browser OAuth grant Cirrove uses for Google and Microsoft.

Cirrove should implement an interactive, local authentication flow in which the
password and 2FA code are never logged or sent to Cirrove's servers. The password
is used in memory for the SRP exchange and discarded; the resulting session and
trust material belong in the local Secret Service vault. A fresh challenge asks
the person to authenticate again. This is a proposed design, **not** a claim that
password-free renewal works for every Apple account. Rclone stores an obscured
password in its config; [rclone itself says obscuring is not secure encryption](https://rclone.org/commands/rclone_obscure/).

The first live spike must use a Cirrove-owned native Rust probe and its own private
credential store. It must not call rclone or read or change another client's
configuration, credentials, mount or service. No credentials, cookies, token
values, signed download URLs or raw response bodies enter the evidence artifact.

On 2026-09-27, a feature-gated Cirrove probe using the isolated, saved
`iCloudGuiValidation` session successfully created a uniquely named test folder
and one small ordinary file in it, confirmed their returned provider identities,
and read the file back byte for byte. The [pre-registered run artifact](../benchmarks/icloud-write-fixture-2026-09-27.md)
records the scope and elapsed time. No pre-existing user item was changed. This
supports the **new-file** request shape for one account; it says nothing yet
about safe overwrite, conflict detection, delete, or long-term reliability.
The feature-gated shared-worker create path now persists Apple's allocated
document ID and signed content slot in its encrypted checkpoint before any
visible file registration. A synthetic test first demonstrated that the old
name-and-byte rule could accept a foreign same-name document; it now rejects
that item even if its bytes match. In one
[two-process owned-file trial](../benchmarks/icloud-worker-reserved-create-identity-2026-09-28.md),
the first process discarded a registration response, and the second matched
the saved exact document ID and full bytes without replaying an upload or
registration. This closes a receipt-identity flaw in the small owned-fixture
path; that trial did not cover ordinary names, large files or mounted writes.
The owned-fixture Create worker now accepts ordinary Linux filenames up to
255 UTF-8 bytes within its new Cirrove test folder, while retaining exact
parent and allocated-document-ID checks. One
[live Unicode/space filename trial](../benchmarks/icloud-worker-ordinary-name-create-2026-09-28.md)
created and independently verified `Résumé 2026 final.txt` through that
worker. It confirms this name shape on one account, not all legal names,
existing folders, collisions or general mounted writes.
The same owned folder then supplied a
[controlled occupied-name trial](../benchmarks/icloud-worker-ordinary-name-collision-2026-09-28.md):
a second Create with different bytes and a different reserved document ID
stopped at `Conflict`. The existing exact ID and full bytes stayed intact,
and the second journal retained its payload without a remote receipt. A
collision arriving during an in-flight registration is still unverified.
The reserved-document Create worker now has a 4 MiB per-file validation
ceiling and selects `application/octet-stream` for non-text filenames;
older direct probes retain their 4 KiB cap. A first 1 MiB trial stopped
locally because the probe journal still had an 8 KiB quota, before any
content request. After recording that correction and raising only the
isolated journal quota, one
[owned binary-file trial](../benchmarks/icloud-worker-bounded-binary-create-2026-09-28.md)
uploaded and verified a full 1 MiB `.bin` file through the shared worker.
This does not establish streaming, files above 4 MiB, other MIME types or
ordinary mounted writes.

## Direct Linux architecture if the gates pass

```mermaid
flowchart LR
    Apps[Linux applications] --> FUSE[Cirrove FUSE]
    FUSE --> Engine[Shared metadata cache, pinning and read engine]
    Engine --> Adapter[Experimental iCloud ReadProvider]
    Adapter --> Web[iCloud web transport]
    Adapter --> Vault[Local Secret Service]
```

The adapter must return Cirrove's account + collection + item identity. Use Apple's
opaque item ID and zone, not a path, and verify their stability over rename and
move. Directory listing and change tracking must stage a complete bounded
snapshot before publishing its metadata and cursor. If Apple offers no usable
Drive change cursor, use a bounded, scheduled tree reconciliation with explicit
freshness limits; never pretend it is an incremental feed. For large directories,
prove pagination or a safe response bound before accepting the transport.

For reads, bind the item ID to a verified content revision. ETag, size and
modification time are candidates, not yet a proven content lineage. A ranged read
must reject a changed item before cache publication; an expiring download URL is
transport material, never identity or a persisted cursor. If content revision
cannot be established, return a clear error rather than serve bytes under a stale
version key. The existing cache, offline pinning, desktop state and Dolphin
integration can then remain shared.

## Milestone I0: direct Linux read feasibility

A [native Rust read probe](../icloud-native-probe.md) now implements an
interactive SRP/trusted-device sign-in, bounded root/folder listing and a bounded
small-file read by opaque ID without any rclone dependency. Its SRP proof,
endpoint validation and metadata parsing have local tests. It has **not** yet
completed the full I0 gate. One real account has passed sign-in, root listing
and a small file download through the native probe. A nonzero range matched a
saved complete file, and a keyring-backed session resumed in a separate process
without another password or code prompt. Revision semantics, complete refresh,
renewal after expiry and account-class coverage remain open. An experimental
read-only account path, command and desktop picker now exist on the draft branch,
but no iCloud account has been mounted through the installed service.

The first probe is isolated and read-only. It may inspect owned fixtures, but it
must not mutate a cloud account without a separate explicit authorization.

The Fedora/GNOME connection is already identified as rclone FUSE, so it establishes
that direct Linux access is possible. Do not wrap, call or test through its mounted
path: a filesystem path alone cannot supply Cirrove's account + collection + item
identity, provider revision, and version-bound read contract, and nesting FUSE
mounts would obscure ownership and recovery. The live probe must use Cirrove's
native transport and independent sign-in, never the existing rclone configuration,
mount or service. The same Apple account may be used only through that separate
Cirrove sign-in; a new Apple account is not a prerequisite for the read probe.

1. Reproduce account sign-in and re-sign-in for a dedicated test account, with and
   without Advanced Data Protection if accounts are available. Record only success,
   failure class and elapsed time.
2. Enumerate the root, a normal folder, an app folder and a shared folder. Measure
   page/response sizes and whether complete listing is possible.
3. Check opaque item and zone IDs across a rename, move, restart and another-device
   change using owned test fixtures. Any remote mutations for these cases require
   separate authorization.
4. Read an exact small and large file, including nonzero ranges; compare hashes
   with a separately downloaded copy. Inject cancellation, stale metadata,
   expired signed URL and interrupted response.
5. Prove whether the ETag or another field distinguishes content changes from
   metadata-only changes. A same-size replacement is mandatory.
6. Establish a complete refresh method that does not lose the last visible
   metadata or completed checkpoint when interrupted. Bound both memory and
   provider requests on a large synthetic tree before any live-scale claim.

Completion means Cirrove's own read path can satisfy its existing `MetadataProvider`
and `ReadProvider` contracts with stable identity, verifiable bytes and honest
freshness. A successful `rclone ls` alone is only evidence that login and listing
work. If the transport cannot meet a gate, record the exact limitation and keep
iCloud out of Cirrove's account picker.

## Writes require a separate gate

Rclone and pyicloud show that uploads, rename and deletion are possible. Their
existence does not establish Cirrove's requirements for conflict preservation and
crash recovery. In particular, rclone's current replace path trashes the old file
before uploading the replacement, and its optional ETag conflict retry updates
the precondition. Cirrove must not copy either behavior.

Each intended write needs an owned remote test item, a demonstrated stale-write
conflict, an interruption after the request but before its receipt, and exact-ID
reconciliation. If the web transport cannot offer a conditional replacement with
recoverable identity, the iCloud mount remains read-only. A local upload reaching
HTTP success is not, by itself, proof that the intended remote item now owns the
exact bytes.

### Write gate after the native read preview (2026-09-27)

The native iCloud adapter currently implements only `MetadataProvider` and
`ReadProvider`. Cirrove's durable `UploadProvider` and `MutationProvider` workers
already exist. The iCloud work is to prove and implement their provider-specific
contracts, while every iCloud account remains read-only.

Current upstream [transport code](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/api/drive.go)
demonstrates `createFolders`, `renameItems`, `moveItems`,
`moveItemsToTrash`, `/upload/web`, and `/update/documents`. The
namespace calls carry an item ETag and can report `ETAG_CONFLICT`. Rclone's
optional `force` path retries with the newer ETag; Cirrove must instead retain
the local intent as a conflict. Its [file replacement path](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/iclouddrive.go)
trashes the existing
item before a new upload, and the document update request uses `add_file` with
`allow_conflict: true` and no original-item ETag. Neither path proves a safe
same-identity, conditional content replacement. These are source-code
observations, not live Cirrove mutation evidence.

Advance through these gates in order, using only Cirrove-owned test items in an
isolated account state. A cloud-mutating live run needs separate explicit task
authorization and a registered measurement protocol:

1. Add a native, disabled write transport and synthetic fault fixtures. Validate
   exact account/zone/item IDs, response status, ETag conflicts, bounded uploads,
   signed URL validation, and cancellation. Persist the upload identity/session
   checkpoint before any mutating request. Do not add a UI write toggle yet.
2. On a run-owned fixture, test creation of a new file and folder, then resolve
   an intentionally lost response by exact identity and exact content bytes. A
   same-name sibling is never sufficient proof of success.
3. Determine whether an existing file can be replaced in place under a stale
   revision precondition. Test an unchanged base, a different-device edit before
   commit, a same-size content change, and a response lost after commit. Confirm
   identity and full content hash independently. A preflight ETag read is
   insufficient if the commit itself ignores the stale precondition.
4. Test exact-ID conditional rename, move and trash, including stale ETags,
   destination collisions, repeated requests and interrupted receipts. Folder
   removal must verify emptiness and acknowledge that a list-then-delete window
   may remain non-atomic; never implement it as unchecked recursive trash.
5. Connect only proven operations to the existing journal and FUSE writer in a
   disabled, isolated mount. Exercise ordinary editor atomic saves, offline
   restart, conflicting remote edits, quota/permission errors, expired sign-in,
   large files and shared/app folders. Preserve local bytes until a verified
   receipt; demonstrate recovery after process death at every boundary.

The decisive gate is step 3. If Apple's web transport cannot atomically refuse
a stale content replacement and reconcile an uncertain result by stable identity,
Cirrove must not offer general writable iCloud mounts. A narrower, explicitly
labelled create-new-only experiment may still be possible, but it is not full
write support. Apple's general Drive web transport has no published stability
contract, so even a passing single-account test remains experimental until
repeated and account-class validation is complete.

### Refined route if the web transport has no in-place overwrite

The Fedora machine's actual rclone version,
[1.74.3](https://github.com/rclone/rclone/blob/v1.74.3/backend/iclouddrive/iclouddrive.go),
trashes the previous remote item before beginning an upload. Its VFS `full`
mode makes editor writes locally compatible and [writes them back after close](https://rclone.org/commands/rclone_mount/);
that is not a remote compare-and-swap. [pyicloud](https://github.com/picklepete/pyicloud/blob/master/pyicloud/services/drive.py)
only implements a new-file `add_file` upload; [icloud-linux](https://github.com/IsmaeelAkram/icloud-linux/blob/master/driver.py)
also deletes the old remote item before uploading its dirty file. The independent
[idrive-cli notes](https://github.com/nktknshn/idrive-cli#upload) explicitly say
the web path cannot overwrite and remove the old item first. None of these
sources proves that a safer private request is impossible, but none supplies
the same-ID conditional replacement Cirrove currently requires either.

Before attempting a fallback, run a disabled, run-owned probe of Apple's
`update/documents` request. Try an existing document ID with conflict creation
disabled, and compare unchanged versus deliberately stale revisions. Check
whether Apple retains the document/Drive ID and either rejects stale content or
creates a separate conflict version. Do not infer semantics from the request's
`allow_conflict` name; verify the final bytes and both identities independently.

The [unchanged-base trial](../benchmarks/icloud-same-id-update-2026-09-27.md)
found a same-ID update that returned exact revised bytes. The subsequent
[stale-ETag trial](../benchmarks/icloud-stale-etag-2026-09-27.md) used the same
request shape with `allow_conflict: false` and the original ETag after a first
update. Apple accepted the stale request and exposed the second candidate
bytes under the same ID. The direct same-ID route therefore fails Cirrove's
conflict gate for this request shape and cannot be enabled for general writes.
This is one account and one request shape, not a proof that every undocumented
Apple mechanism lacks a conditional replacement.

Two follow-up rename trials narrowed the namespace question. A rename with
an ETag made stale by a content update was [accepted](../benchmarks/icloud-conditional-rename-2026-09-27.md).
A metadata-only trial then renamed the file once, observed a new ETag, and
submitted a second rename with the pre-rename ETag; that too was
[accepted](../benchmarks/icloud-metadata-rename-etag-2026-09-27.md), with the
same item ID and bytes. The known `renameItems` request shape therefore also
fails the conditional namespace gate on this account. Rclone's branch for
`ETAG_CONFLICT` cannot be treated as proof that every stale ETag is rejected.
Cirrove must not wire these requests into its current conditional mutation
contract.

A separate [HTTP `If-Match` trial](../benchmarks/icloud-http-if-match-2026-09-27.md)
put the stale ETag in a standard conditional header as well as the JSON body.
Apple still accepted the same-ID content update and exposed the candidate
bytes under the original ID. This closes another plausible request variant
for this account. The next write work must preserve versions independently
of any assumed web-API ETag enforcement; direct overwrite remains disabled.

The following remains only a *non-atomic, conflict-preserving research
candidate*, not an implementable safe write protocol yet. The two rename trials
show that the known ETag parameter does not provide the required conditional
move of the old item. A design that uses it as a compare-and-swap is invalid:

An [occupied-name probe](../benchmarks/icloud-occupied-name-2026-09-27.md)
uploaded a separately identified staging file next to an owned original,
then renamed the staged item to the original's occupied name. Apple kept both
IDs and exact byte sequences and gave the staged item a numbered conflict
suffix. This demonstrates one non-destructive collision outcome, not a way to
install the new content at the original name without a race.

A separate [staged-handoff probe](../benchmarks/icloud-staged-handoff-2026-09-27.md)
then moved the owned old file to a unique recovery name, verified both IDs
and byte strings, moved the staged file to the vacated original name, and
verified both again. That sequence worked once without deleting either
version. It still has a visible gap at the original name, has no enforced
ETag precondition, and has no durable restart checkpoint. The result is
feasibility evidence for preserving both versions, not permission to expose
normal iCloud writeback.

An initial [durable handoff trial](../benchmarks/icloud-durable-handoff-2026-09-27.md)
persisted account-bound exact IDs and full hashes before the first rename,
stopped a process after the old item reached its recovery name, and completed
the second rename in a new process after independent reconciliation. Both
versions survived. This is an isolated process-boundary result, not yet a
lost-response or power-loss result by itself, and the staged upload itself
still lacks a checkpoint before its first mutating request.

A separate [lost old-rename receipt trial](../benchmarks/icloud-lost-old-receipt-2026-09-27.md)
left `old_rename_pending` after Apple accepted the request. A fresh process
reconciled both exact IDs and full hashes, recognized the committed rename,
completed the staged handoff, and retained both versions. This validates one
deliberately lost receipt after a completed request. It does not cover a
request still in flight or make either rename conditional.

The [second-rename receipt trial](../benchmarks/icloud-lost-new-receipt-2026-09-27.md)
likewise left `new_rename_pending` after sending the request. A fresh process
verified that both exact IDs and hashes had reached the target and recovery
names, then closed the checkpoint without resending the request. This covers
both deliberate post-response receipt-loss boundaries once each. It remains
insufficient for normal writeback: the initial upload is not yet crash-safe,
the two renames are not atomic or conditional, and concurrent edits were not
exercised.

A further [staged-registration trial](../benchmarks/icloud-lost-registration-receipt-2026-09-27.md)
fsynced a unique name and full hash before the first upload request, then
discarded the registration response after Apple replied. A fresh process
found one matching candidate under the exact parent ID, checked both file
hashes, bound the new remote ID and fsynced it without reuploading. This
closes one post-response registration boundary in the isolated probe. It
does not prove recovery from an in-flight timeout or an interrupted content
slot upload, and this checkpoint is not yet integrated with the normal
writeback journal or mounted filesystem.

The [linked handoff trial](../benchmarks/icloud-reconciled-stage-handoff-2026-09-27.md)
then took that remotely discovered and fsynced staged ID, verified both
files again, fsynced a separate handoff plan and completed both renames
without another upload. Both versions remained. This joins the two
experimental recovery mechanisms for one owned fixture. It does not prove
the entire sequence under simultaneous remote edits or arbitrary process
death, and it does not satisfy Cirrove's existing same-ID replacement
journal contract.

The isolated handoff checkpoint now records the original and staged item
ETags alongside their IDs and full hashes. Reconciliation refuses a prepared
handoff if either revision changed, even when a concurrent edit left the same
bytes; it also refuses the second rename if the staged revision changed while
the old item was at its recovery name. Rename requests retain the recorded
ETag instead of silently refreshing it. This closes detectable revision drift
in the probe, but Apple's tested rename endpoint can accept a stale ETag. A
change between the final read and rename remains a race, so this is not a
conditional write protocol and does not enable mounted writes. Older
version-1 handoff checkpoints lack the frozen revisions and cannot authorize
further mutations.

1. Upload new bytes under a unique temporary name without touching the old
   item. Persist the new exact ID and upload receipt first, then read it back
   and verify its full hash.
2. Re-read the old exact ID, revision and content. Any subsequent rename to a
   recovery name is currently **unconditional** on that revision. It could
   move a concurrent remote edit, so preserve that item as a recovery copy and
   record this as a conflict, never assume the preflight protected it. A
   genuinely conditional operation or an equivalent non-destructive mechanism
   still needs to be found and proven before ordinary writeback is enabled.
3. Move/rename the verified new item into the original name. If that fails or
   its response is lost, reconcile both exact IDs and preserve the old recovery
   item. Do not decide success by a matching path or file size.
4. Never automatically retire the old recovery item under the currently known
   request shapes. A crash at every transition must be restartable without
   reuploading under an unknown identity; user-visible conflict resolution
   would be required before any old version could be removed.

This protocol has a visible interval when the original path is absent or held
by another name, and concurrent clients may create a destination collision.
It also changes the remote item ID. Cirrove's current upload journal explicitly
requires `UploadIntent::Replace` to acknowledge the *same* remote ID, so this
fallback needs a new, provider-neutral identity-handoff operation and mounted
application tests. It could preserve both versions without pretending to offer
atomic POSIX replacement; it cannot be called full parity until its limitations
are reflected in the product and pass live conflict, crash, folder and account
class validation. The preferred path remains a proven conditional in-place
update if the Apple web transport offers one.

An isolated [stale-ETag Trash trial](../benchmarks/icloud-stale-trash-2026-09-27.md)
and [current-ETag control](../benchmarks/icloud-fresh-trash-control-2026-09-27.md)
found a narrower conditional operation: after a confirmed same-ID content
change, `moveItemsToTrash` refused the old ETag while preserving the newer
bytes; the same fixture procedure then accepted the observed current ETag
and the exact ID disappeared from its parent. This is one account and one
run per arm. Absence from the parent is not independent proof of Trash
recoverability. It does not make same-ID content updates or renames
conditional, and normal deletion remains disabled until restart and
response-loss reconciliation, concurrent-edit behavior and recovery are
validated.

A [bounded special-Trash listing](../benchmarks/icloud-trash-listing-2026-09-27.md)
returned a complete item array with restore metadata. The first
[owned-file restore cycle](../benchmarks/icloud-trash-restore-cycle-2026-09-27.md)
stopped indeterminate after a successful restore response because its
combined metadata check failed. A separate
[identity follow-up](../benchmarks/icloud-trash-restored-identity-2026-09-27.md)
confirmed that the restored file's document ID and complete bytes matched,
while Apple changed its displayed name. The Drive ID after that restore was
not classified. A normal implementation must reconcile the returned
provider identity and actual name, expose any collision, and never assume
that restoring from Trash recreates the old pathname. It still needs
response-loss and restart validation before deletion can be enabled.

The feature-gated native `ICloudOwnedFixtureUpload` now implements the shared
`UploadProvider` for one small `Create` inside a freshly created Cirrove-owned
validation folder. It binds a checkpoint to account, collection, folder,
unique name, size and SHA-256 before network mutation. Its successful path
re-lists the exact parent and reads back complete bytes before the shared
worker commits the remote ID and ETag. The first
[live worker trial](../benchmarks/icloud-worker-create-2026-09-27.md) reached
the durable `Uploaded` state. A missing response leaves the operation uncertain;
the adapter does not infer a safe retry from temporary absence. This is a
staging-file building block, not an enabled writable mount: the bounded file
size, in-flight response loss, broader restart coverage, `Replace`, directory
mutation and recovery UI remain unvalidated or unimplemented.

A further [two-process worker trial](../benchmarks/icloud-worker-lost-registration-2026-09-27.md)
discarded an already received file-registration response after the shared
worker had persisted its checkpoint. The first process stopped at
`VerifyRequired`; a second process rebuilt the owned folder handle from its
exact root ID, verified the single staged file and complete hash, and committed
the same journal operation as `Uploaded`. The second process's adapter refused
all upload-part calls. This validates that one known-committed lost response
can be reconciled without replay. It does not settle an in-flight request,
content-slot interruption, or concurrent changes.

The shared transfer worker can now persist and execute a bounded sequence of
commit checkpoints. A synthetic two-phase test first failed at the second
phase, then passed after each checkpoint was saved before its provider call.
The feature-gated `ICloudOwnedFixtureHandoff` now uses these checkpoints for
the old-ID recovery rename and new-ID installation. A single
[live worker handoff](../benchmarks/icloud-worker-handoff-2026-09-27.md)
published the new ID and retained the old ID as a hidden recovery object in
the shared journal after reading and hashing both complete files. This was
one owned fixture. A further
[two-process old-rename response-loss run](../benchmarks/icloud-worker-lost-old-rename-2026-09-27.md)
stopped the first worker at `VerifyRequired` after discarding an already
received result. A fresh process loaded the exact saved checkpoint, verified
both IDs and full hashes, and completed only the second rename before the
shared journal published both identities. A separate
[two-process new-rename response-loss run](../benchmarks/icloud-worker-lost-new-rename-2026-09-27.md)
discarded the already received second result. Its fresh-process adapter
refused all rename calls, verified the exact finished IDs and full hashes,
and committed both identities without replay. In-flight timeouts and
concurrent edits still need live validation.
Apple's rename request can accept a stale ETag, so this does not permit
mounted replacement or ordinary-file writes.

A separate [owned-file Trash download trial](../benchmarks/icloud-trash-backup-read-2026-09-27.md)
found that one ETag-bound, recoverable Trash item remained downloadable by
its exact ID and byte-identical while complete Trash listings retained its
ETag, size and restore metadata. This makes a conditional Trash step a
plausible alternative to the unconditionally renamed old recovery copy:
the stale-ETag Trash trial rejected a newer revision, while the current-ETag
control succeeded. The full staged-replacement sequence, recovery binding
in the shared journal, concurrent changes and retention are not yet proven.
Do not enable mounted writes on this evidence alone.

One [combined conditional-Trash handoff trial](../benchmarks/icloud-conditional-trash-handoff-2026-09-27.md)
then exercised the entire owned-fixture sequence: a same-ID edit changed the
old ETag; the stale Trash request refused it and preserved both full files;
the current ETag moved only the old ID to recoverable, byte-readable Trash;
and the staged ID took the original name while the old revised bytes remained
in Trash. This is a more promising replacement primitive than unconditional
old-file rename. It still needs a durable provider-neutral receipt for a
Trash-backed backup, restart/response-loss and collision handling, and a
live concurrent-edit test. The request pair has a visible interval without
the original name, so it must not be presented as atomic POSIX replacement.

The shared upload journal can now reserve an opaque Trash parent before the
first provider call and atomically publish the new current ID with the old
exact ID bound to that Trash parent. A synthetic lost-response test first
failed under the previous same-folder-only check and then passed with a
freshly reopened journal; the existing renamed-sibling path still passes.
The feature-gated owned-fixture adapter now selects this location in its
restartable checkpoint, conditionally sends one Trash request for the saved
old ETag, and only issues the staged rename after exact-ID Trash and full-byte
verification. Its final receipt requires a complete Trash listing, stable
ETag/size/restore metadata and full hashes of both IDs. The first
[live shared-worker trial](../benchmarks/icloud-worker-conditional-trash-2026-09-28.md)
reached both remote phases but left the worker at `VerifyRequired`. An exact-ID
read-only inspection found that Trash changes the item's displayed name; the
adapter now retains the actual stable Trash name rather than assuming the old
folder name. A later read-only receipt passed all identity, size and revision
checks, and the ordinary journal transaction atomically published both IDs
without another cloud mutation. The worker's repeated full Trash verification
inside one provider deadline was then reduced to one observation supplying
both phase and receipt. A fresh
[owned-fixture end-to-end run](../benchmarks/icloud-worker-conditional-trash-one-pass-2026-09-28.md)
then reached `Uploaded` through the shared worker, with both bindings
confirmed after reopening SQLite. This one run supports the narrowed request
sequence; it does not establish repeatability. No normal iCloud write is
enabled; concurrent edits after the initial precondition, collisions,
folder writes and Trash retention remain unresolved. One further
[two-process live trial](../benchmarks/icloud-worker-lost-conditional-trash-receipt-2026-09-28.md)
discarded the old Trash response after the request, retained
`VerifyRequired`, then reconciled the old exact ID and published both IDs
in a fresh process without sending Trash again. A lost staged-rename
response and in-flight Trash timeout remain untested.
Another [two-process live trial](../benchmarks/icloud-worker-lost-conditional-rename-receipt-2026-09-28.md)
discarded the staged-rename response after Apple was contacted, then
reopened the same journal in a reconciliation-only process. It verified
both full byte streams and published the current/Trash identities without
either mutation being resent. An in-flight timeout before Apple's response
and external concurrent edits are still open; this does not enable general
iCloud writes. A subsequent
[intervening-edit worker trial](../benchmarks/icloud-worker-intervening-edit-2026-09-28.md)
changed the owned original after the worker's prepared observation and
before its conditional Trash request. Apple refused the saved stale ETag;
both exact IDs and full bytes stayed in the fixture folder, and the worker
persisted `Conflict` without a replacement receipt. This proves that one
deliberately injected race is fenced in the current request path. It does
not establish protection after Apple accepts a request, across other client
edits, or for ordinary mounted writes.
The shared worker's deadline is also now exercised in one
[two-process owned-fixture trial](../benchmarks/icloud-worker-deadline-after-trash-2026-09-28.md):
Apple accepted the conditional Trash request, the adapter held its answer
past the 125-second worker deadline, and a fresh process verified and
completed the handoff without resending Trash. This covers cancellation of
the provider future after Apple has answered the adapter, not a transport
timeout before Apple's response or an unresolved server-side request.

A feature-gated `ICloudOwnedFixtureRemove` now implements the shared
`MutationProvider` contract for one small, newly created Cirrove fixture.
It accepts only an exact prepared file identity, checks the observed ETag and
full bytes, sends one `moveItemsToTrash`, and recognizes success only when the
exact ID is absent from its parent and present with a restore path in a
complete Trash listing. An unchanged item after an uncertain response is
`Indeterminate`, never permission to resend. A one-run
[live adapter trial](../benchmarks/icloud-owned-trash-adapter-2026-09-27.md)
returned and independently reconciled `Removed`. It took over three minutes;
at the time of that trial the adapter was not wired to the mounted service. The shared mutation
worker then completed a [durable owned-file Trash trial](../benchmarks/icloud-worker-trash-2026-09-27.md)
after a journal fix that binds a `Removed` receipt to its exact prepared ID.
In a [two-process lost-response trial](../benchmarks/icloud-worker-lost-trash-receipt-2026-09-27.md),
the first process discarded an already received Trash response and retained
`VerifyRequired`; the second reconciled that exact ID in complete Trash
metadata and committed `Applied` without permission to send another delete.
Both are single-fixture results. In-flight timeouts, concurrent edits, other
account classes, folders, permanent deletion and restore remain open; normal
iCloud mounts remain read-only.

Later feature-gated mounted trials now cover
[Create](../benchmarks/icloud-mounted-create-2026-09-28.md),
[empty-folder Trash](../benchmarks/icloud-mounted-empty-folder-trash-2026-09-28.md),
[file Trash](../benchmarks/icloud-mounted-file-trash-2026-09-28.md), and one
[two-ID file replacement](../benchmarks/icloud-mounted-replace-2026-09-28.md)
inside new Cirrove-owned validation folders. The replacement stages the sealed
save bytes through the shared upload worker, checkpoints Apple's allocated
document identity and content receipt, conditionally moves the old exact ID to
recoverable Trash, then installs the staged exact ID under the original name.
A fresh process read the new bytes through FUSE and independently found the old
ID with a restore path in Trash without another mutation. The write arm's
separate validator exited nonzero because its Trash predicate was inverted;
that correction and two failed read-only checks remain in the same artifact.
The isolated mounted replacement adapter subsequently gained a 32 MiB bound:
active files are SHA-256 checked in revision-bound 4 MiB ranges, and the exact
old ID in Trash is hashed as a bounded stream. A later
[5-to-6-MiB mounted replacement](../benchmarks/icloud-mounted-large-replace-2026-09-28.md)
passed with a fresh read-only remount. The one replacement arm took over
eight minutes; it gives a functional result, not a reliability or throughput
claim. The rest of the size range is not live-validated.
This is bounded functional evidence, not proof of atomicity, timeout recovery
before Apple's response, concurrency safety, large-file support or ordinary
iCloud write reliability. Normal iCloud mounts remain read-only.

Reviewing another contemporary [Linux iCloud client implementation](https://github.com/IsmaeelAkram/icloud-linux/blob/master/driver.py#L1419-L1433)
did not reveal a conditional replacement primitive: its file-sync path
attempts to delete the old node, ignores that delete's exception, then
uploads the local bytes under the same name. That implementation is useful
evidence of API usage, but this sequence cannot meet Cirrove's durable
conflict-preservation contract.

## Immediate implementation sequence

1. Build a synthetic protocol fixture from documented observations of the open
   clients, without copying their implementation. Keep it isolated from live
   accounts and from the current provider crates. Add only dependencies needed by
   this concrete native probe.
2. Implement a Cirrove-owned, read-only Rust transport probe with private vault
   storage and interactive Apple sign-in/2FA. Run I0 through that probe and record
   the results. The existing Apple account can be used after separate Cirrove
   sign-in; the current Google and Microsoft accounts are unrelated.
3. Continue validating the native Rust adapter behind the existing provider
   contracts. Synthetic shared-engine tests pass, but the isolated preview mount
   and installed account flow still need live evidence before a release claim.
4. Only then decide whether the maintenance cost of Apple's undocumented protocol
   is acceptable for a released Linux provider. If not, document the tested
   limitation and leave the feature experimental.

This is a direct Linux strategy. Its cost is ongoing compatibility work whenever
Apple changes the web transport; no official stability promise exists. That cost
must be measured alongside the benefit of using Cirrove's stronger cache, pinning
and recovery machinery rather than maintaining a second filesystem.
