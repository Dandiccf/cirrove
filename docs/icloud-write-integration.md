# iCloud Drive write integration boundary

The development branch now exposes explicit, experimental ordinary-file write
access for iCloud accounts; new connections still default to read-only. This has
not been installed into the user's regular daemon or accepted for release.
Three owned-folder application runs passed create, save, replacement, relocation
and recoverable Trash checks; the third includes selected-file admission. Installed
acceptance remains under active validation;
full iCloud support is not yet achieved.
See [application evidence](benchmarks/icloud-real-applications-acceptance-2026-10-01.md)
and [selected-file admission](benchmarks/icloud-selected-write-admission-2026-10-01.md).

The 7 October [genuine Keynote PACKAGE replacement](benchmarks/icloud-keynote-genuine-replacement-2026-10-07-3d901c2b.json)
completed normal CLI import A and one replacement by Apple-edited B. Stopped
independent full semantic V2 reads verified current B and exact original A in
Trash. A separate [normal read-only remount](benchmarks/icloud-keynote-readonly-remount-2026-10-07-f9923c49.json)
returned complete B before any Apple opening; [Apple Keynote then opened that
exact receipt-bound item](benchmarks/icloud-keynote-apple-reopen-2026-10-07-a0c382a4.json)
and retained its one-slide B title and subtitle after one reload, with no edits.
The original managed processes closed and protected source/installed artifacts
remained unchanged. This is one bounded PACKAGE workflow, with no claim for
DATA coverage, ordinary/atomic editor saves, broader export fidelity or repeated
reliability. Full-iCloud acceptance is now four criteria closed and two open;
delivery to the user's regular installation remains on HOLD. The dated closure assessment below combines the
measured replacement, removal, refusal and process-loss endpoints; historical
open-row counts later in this record retain their original checkpoint meaning.

The [documented iWork application criterion](benchmarks/icloud-iwork-application-acceptance-2026-10-08.json)
now covers all twelve DATA/PACKAGE ordinary/atomic archive-copy workflows and
the supported DOCX, PPTX and Apple Excel export-copy checks. Native Linux iWork
saving remains unsupported; the precise tested content and format limits are
recorded in the latest dated assessment below. Installed lifecycle and reliability
criteria remain open; the installed Strata preservation criterion is closed.

## Current checkpoint, 9 October 2026

Criteria 485, 486, 487 and 489 are closed; 488 and 490 remain open. The
[installed Arch trial](benchmarks/icloud-installed-bundled-2026-10-09-432da505-cb17-4658-8982-15a75dc67588.json)
first connected through public native sign-in on aa02 with default read-only
access, then upgraded to attested 7dc061c packages and restored the same ready
account and read-only mount. Strata's direct Keep/Stop and parent Keep/Stop
passed on both versions. The current-package trial began with an uncached
8 MiB file, observed Fetching and kept/inherited badges, subscribed event refresh
and exact readback, and preserved installed provider files, preferences and the
default Nautilus association. This is the bounded installed Strata criterion,
not acceptance of every desktop or general provider reliability.

Same-account native reauthentication subsequently enabled writes. One new folder
was applied remotely, but its ordinary file save remained locally retained as
Conflict. The [parent-lookup regression](benchmarks/icloud-fresh-mkdir-parent-regression-2026-10-09.json)
failed on the old code and passed nine focused local tests with the correction;
a fresh installed cloud save retest is required, and the historical initial
metadata state does not uniquely establish that failure's cause. The same guest
retained and exported a 44-byte sealed save and a 52-byte unlinked dirty working
file while offline, then remained disabled and unmounted. The outer SSH owner
exited 255 after timeout; the six public CLI children exited zero and the exports
matched their registered hashes. This is partial recovery evidence, not a clean
overall run or read-only downgrade. Recovery of these same generations after
genuine read-only reauthentication remains open. Dated counts and failed outcomes
later in this record retain their original checkpoint meaning.

The [final-install conflict validator](benchmarks/icloud-final-install-preflight-validation-2026-10-09.json)
adds an explicit developer-feature pause after the last read-only preflight and
before one native install request. Its old-schedule regression failed and ten
local controls passed; normal commit and reconciliation behavior is unchanged.
The first live arm stopped at a test-controller file-permission guard after
creating its original document, before replacement or competitor import. All
five children closed. Its successor was only partially prepared and never
dispatched. Neither attempt establishes the live conflict endpoint or closes
criterion 490.

Saved-session feeds now [revalidate on each existing scheduled poll](benchmarks/icloud-saved-session-health-regression-2026-10-09-38c5c4b2-2c9a-4538-8b17-c37937a6654d.json).
Previously, the on-demand adapter cached its initial validation indefinitely,
so a later rejected session could leave the account marked Ready. The old-code
regression failed at the intended assertion and three corrected local controls
passed. Observed authentication rejection can now reach the existing
sign-in-required status on the next poll; temporary server failures retain their
separate classification. This adds one complete, nonrecursive root listing per
saved-session poll, normally every 30 seconds plus request time, with the
existing timeout, cancellation and response-size limits. Cached metadata,
completed cursors and pending edits remain intact. Natural Apple session expiry
and subsequent installed reauthentication still require live acceptance.

### Tested samples and implementation limits

These are exact bounded samples or implementation limits, not a coverage percentage
or a guarantee for arbitrary files. A successful read does not establish a save.

| Scope | Tested sample or implemented limit | Evidence and remaining boundary |
| --- | --- | --- |
| Native iWork archive copies | Twelve Pages, Numbers and Keynote DATA/PACKAGE workflows, ordinary and atomic replacement | [Application assessment](benchmarks/icloud-iwork-application-acceptance-2026-10-08.json): exact current/Trash originals, remount and Apple reopen; native Linux iWork saving and arbitrary document fidelity are unsupported or untested. |
| Office export copies | DOCX: one 93-byte paragraph; PPTX: one slide title/subtitle; XLSX: registered values 7/3/10, SUM and source marker | [Content limits](benchmarks/icloud-iwork-application-acceptance-2026-10-08.json): native Numbers imports flattened SUM; Office-copy saves do not write back to the iWork source. |
| Mounted slow-link read | 138,943 bytes, exact checksum, initially zero resident bytes | [Fair-proxy diagnostic](benchmarks/icloud-normal-read-interruption-recovery-2026-10-09-08bdbd8d-8f80-45bb-9555-22220b636611.json): retained-copy read passed; preceding failures remain failed, without a uniquely established cause or repeatability claim. |
| Installed Strata file | 8,388,608-byte ordinary text file, initially zero resident bytes | [Installed trial](benchmarks/icloud-installed-bundled-2026-10-09-432da505-cb17-4658-8982-15a75dc67588.json): four GUI actions, Fetching/event refresh and independent Web-download/readback digest; no successful ordinary cloud save claim. |
| Historical ordinary-file size arm | Create 1,073,741,841 bytes; replace 1,074,790,429 bytes | [Mounted GiB arm](benchmarks/icloud-mounted-gib-account-2026-10-01.md): exact remote digests, predecessor in Trash and fresh-mount read in one isolated arm; not a native-package limit or arbitrary-size/installed-current acceptance. |
| Native write staging implementation | Four reservations per Engine account runtime; each anonymous upload/verification file at most 67,108,864 bytes, I/O chunks at most 65,536 bytes | [Ownership controls](benchmarks/icloud-native-write-staging-budget-2026-10-03.json) and [source](../crates/cirrove-icloud/src/write_staging.rs): reservations survive cancelled waiters; synthetic proof, separate from journal/cache/source capture, filesystem overhead and other runtimes, not a measured host-wide quota. |

### Abandoned native Stage policy

Stage abandonment retains the local archive, encrypted checkpoint and exact
Stage/original identities; it does not confirm a Trash backup or remove the
remote Stage. The [active recovery-only endpoint](benchmarks/icloud-active-abandoned-stage-recovery-2026-10-09-d6cb88f0-470c-4f76-87c5-624481e234f8.json)
read the existing abandonment receipt and exported the exact 66,268-byte retained
Pages archive without invoking abandonment again or replaying a mutation.
Captured journal tables and persistent payloads remained unchanged. No automatic
remote TTL cleanup, abandoned-Stage deletion or safe reclamation is implemented
or claimed by this evidence; retained cloud staging and broader abandoned-work
reliability remain part of criterion 490.

## Document packages remain a release gate

Google native Docs/Sheets are deliberately projected as read-only export folders
with `Node.package = true`; their `.gdoc`/`.gsheet` names are generated by Cirrove
from the provider MIME type. OneDrive also preserves its provider package facet.
These existing protections do not establish iCloud package recognition.

The iCloud directory adapter preserves `APP_CONTAINER` and `APP_LIBRARY` as
protected, readable folders; ordinary `FOLDER` entries stay ordinary. This is a
conservative app-owned-container policy, not proof that every child is a native
document bundle. The [container boundary tests](benchmarks/icloud-app-container-boundary-2026-10-01.md)
cover raw metadata projection and continued nested read traversal. Its on-demand
path also checks Pages/Numbers/Keynote FILE candidates with Apple's representation
lookup and marks only confirmed packages as read-only package folders. A matching
extension alone does not mark a package. Unknown FILE bundle types remain
unclassified in listings. A selected-file admission hook now checks the actual
Data-versus-Package representation before accepting an ordinary-file mutation,
regardless of extension. Metadata and local identity are rechecked around that
lookup. This avoids content downloads or a lookup for every listed file; it does
not yet provide full native-document editing. Its synthetic race tests and the
combined live application arm passed; installed acceptance remains open. A
filename extension alone is not sufficient evidence. The Google folder presentation reported by the user
is a separate desktop usability issue, not evidence of broken MIME detection.

A [read-only account investigation](benchmarks/icloud-document-metadata-shapes-2026-09-30.md)
now confirms that FILE listings can supply different download representations:
one Pages document supplied `package_token`, while one Numbers document supplied
`data_token`. Listing kind alone cannot authorize ordinary-blob replacement.
The transport now retains Data versus Package until its consumer selects a
contract. Ordinary-file hashing and replacement/Trash readback refuse package or
ambiguous download locations. Exact reads keep their existing length, range and
revision checks. A
[bounded range investigation](benchmarks/icloud-document-representation-boundary-2026-09-30.md)
confirmed that the sampled Pages package ignores Range and returns HTTP 200,
while Numbers and Keynote return exact 206 ranges with matching total size.
A package artifact therefore needs its own verified cache and published size.
A new caller-staged complete package download checks source identity/revision
before and after streaming, enforces a caller byte budget and returns actual
artifact length plus digest only on success. A [live disk-staging arm](benchmarks/icloud-package-artifact-2026-09-30.md)
passed: the sampled Pages artifact was 198,458 bytes smaller than its listing
size. An independent local reread matched the stream's digest and actual size.
A [captured-package FUSE arm](benchmarks/icloud-package-mounted-2026-09-30.md)
now passes normal Engine publication through the new bounded streaming staging
hook and full reads after offline Engine/FUSE remount. No fallback provider reads
occurred. The subsequent [normal-adapter arm](benchmarks/icloud-native-package-adapter-2026-09-30.md)
adds confirmed classification, lazy anonymous-file staging and exact refetch from
a published node. It passed a normal read-only Engine/FUSE open, offline remount
and fresh-provider refetch of the sampled Pages document. Apple varies empty ZIP
directory timestamps; the export normalizes those while preserving every regular
file's data and timestamp. The account factory now configures this path, but it
has not been installed into the user's regular daemon. Native-format coverage,
ZIP64, aggregate private staging acceptance and native editing remain release gates.
A [lifetime staging reservation](benchmarks/icloud-package-staging-budget-2026-10-01.md)
now bounds archive data per provider instance even when evicted artifacts remain
held by active readers; it is separate from the block cache and has synthetic
resource-ownership coverage, not a host-wide low-disk acceptance result.
Empty-source write preflight now also requires an ordinary Data representation;
logical size zero no longer skips the package/ambiguity guard. A
[real empty-file compatibility arm](benchmarks/icloud-empty-representation-2026-09-30.md)
passed create confirmation, a fresh-process revision/digest check and mounted EOF.
Native zero-byte package metadata, shared-item identity and native write semantics
remain open; the experimental ordinary-file opt-in does not enable package writes.

## Current code boundary

- `Settings::validate` now accepts an explicit writable iCloud account while
  retaining the exact account/root identity checks. The desktop defaults to
  read-only and offers an explicit access change through reauthentication.
  Context-free `accounts::write_provider` still refuses iCloud; its journal
  context is mandatory. The manager's write factory receives `Account` and an owned
  `WriteContext`: private state root, account metadata index, shared journal
  and checkpoint vault. Engine has acquired the account lock before this
  context is opened. The upload workers use the same journal and vault;
  iCloud selects sealed operation checkpoints and other providers keep
  `DesktopVault`. The production factory now selects the context-aware iCloud
  router. Existing saved account access is preserved unless explicitly changed.
  The router implements new-file uploads, staged replacement, simple folder
  operations and rename/move/Trash of regular files with bounded streaming checks;
  combined move/rename now has a durable three-step implementation and a bounded
  account-router live result. Mounted combined file/folder relocation and final
  file-acknowledgement loss now have live evidence; lost confirmed intermediate
  checkpoints also recover in two mounted arms. In-flight uncertainty remains open.
  A synthetic FUSE regression covers ownership at construction, writer
  failure after ejection, and a later successful writable remount. A writer
  error no longer silently selects a read-only mount. Published old write
  controls are released before rebuilding the journal and replaced after
  a successful remount.
- Normal `ICloudDrive` resolves a cold directory by listing its parent. Its
  single-item `ReadProvider::node` now resolves ordinary FILE IDs through exact
  metadata and a complete, matching parent listing, using the normal package
  projection. Recoverable Trash entries are excluded. The
  [cold-file live arm](benchmarks/icloud-cold-node-2026-09-30.md) passed for one
  active owned receipt and two Trash predecessors with a fresh provider. Ordinary owned CloudDocs folder IDs now use the exact folder-metadata endpoint
  with identity, kind, zone, parent and recovery checks; recovered folders publish
  absence. App-owned/shared folder identities and generated artifacts still
  require their projection context. Missing or unqualified parent identity is
  refused, not inferred from a path.
- The normal-build iCloud Create, folder and file mutation adapters accept
  exact `Node` inputs and sealed account sessions. The account-wide router now
  selects file creation using the durable journal operation ID and verifies its
  scope, intent, payload size and digest. It resolves plain-folder ancestry in
  a scoped metadata snapshot before returning a mutation-free checkpoint.
  That checkpoint captures the parent node and request alongside the inner
  provider checkpoint; recovery uses this captured identity even if the index
  has changed. Unknown operations, mismatched checkpoints and request-only
  calls are refused. Folder creation, rename, same-name move and empty-folder
  Trash now route through the same account journal. They persist a sealed plan
  before sending the mutation, with the exact request, prepared identity and
  destination snapshot. The shared worker now passes the durable operation ID
  through preparation too. New plans distinguish read-only `Prepared` from
  potentially `Sent`: interruption in preparation may retry preflight, but
  the sent marker is persisted before dispatch and authorizes inspection only.
  Legacy plans without a phase remain potentially sent. A prepared plan can
  reconcile as uncommitted even if its item identity had not yet reached the
  journal. Missing plans remain indeterminate. Parent ancestry excludes
  packages, shortcuts and moves into any descendant. The initial direct-child
  negative control passed without the new ancestry guard because the underlying
  adapter already refused that case; the corrected grandchild case fails with
  the guard removed. Synthetic tests also cover checkpoint rebinding and index
  changes. These tests do not establish live account-wide write reliability.
  A process loss after saving the sent marker but before dispatch remains
  conservatively uncertain. Regular-file rename, same-name move and Trash now
  use the existing normal-build adapters. Preparation hashes the full expected
  revision through a bounded streaming read, then seals that digest alongside
  the exact source node before any mutation. Recovery restores this captured
  digest instead of hashing a potentially newer file as the original. The
  source must match the scoped index and have plain-folder ancestry. Missing
  or malformed digests are refused, and cancelled hashing publishes no plan.
  The former 32 MiB verification cap has been removed from normal adapters;
  local numeric/budget bounds and network/worker deadlines still apply. Transient
  or changed remote content during hashing surfaces as uncertain preparation.
  Larger-file live coverage is recorded below and is not an arbitrary-size claim.
  Combined move/rename is routed through a separate sealed three-step plan;
  its bounded live result and remaining gates are recorded below.
  The mounted validator
  supplies those nodes from a bounded test tree and its owned journal.
  Folder-create receipts in that validator now use a separate sealed on-disk
  vault with only the encryption key in Secret Service. Keys bind the account
  and journal operation, and ciphertext authentication separates folder receipts
  from upload checkpoints. Legacy direct-keyring receipts remain readable until
  a later save. Synthetic coverage checks reopen, foreign-account/operation and
  upload-namespace substitution, invalid keys and legacy receipt preservation.
  Removing the authentication binding makes the substitution test fail; this
  storage check is not live acceptance of account-wide folder operations.
- The two-ID replacement and conditional Trash handoff now have normal-build
  `ICloudFileReplace` and `ICloudHandoff` types. Exact-identity inspections and
  receipt checks were extracted from the large write-probe module into shared
  transport code; fixture creation and intervening-edit injection remain gated.
  Existing probe names remain feature-gated aliases for compatibility. Their
  constructor requires an
  exact source node, folder and operation. An existing-file constructor now
  computes the original SHA-256 through a version-checked remote read and
  persists it in the sealed checkpoint; one owned mounted fixture passed this
  path. The validator previously obtained its source node from its owned
  journal. A pre-existing file in a normal account has no such journal row.
  The upload request carries item ID and ETag but neither parent nor original
  digest.
  The isolated validator now resolves its replacement source from the
  account-scoped visible metadata index in one SQLite snapshot, then applies
  its owned-tree guard and remote revision preflight. The shared transfer
  worker now passes its durable journal operation ID through every upload
  phase; the validator uses that ID to reopen the exact row instead of
  searching for a matching request, which can be ambiguous for identical
  pending saves. A synthetic worker test covers duplicate requests. An owned
  mounted replacement and fresh-process read passed the operation-ID path;
  a repeated phase-timed arm put about 466 seconds between local FUSE save
  and upload confirmation, then about 30 seconds in the independent Trash
  check. A subsequent provider-phase run found two handoff commits dropped
  at the worker's exact 125-second limit; inspections of 67.6 and 98.7
  seconds recovered the advanced remote state. The journal retained two
  failed attempts. Inner timing then located both cancellations in full
  postflight/receipt verification; the Apple mutation requests completed in
  19.7 and 2.2 seconds. A phase-split experiment failed when a preflight alone
  exceeded 125 seconds and the saved InstallNew phase could not safely
  advance from OldAtRecovery. That experiment was reverted, and its owned
  fixture remains `verify_required` as a recovery gate. The worker now
  accepts a provider-specific commit deadline; the isolated replacement
  requests 300 seconds while retaining all original checks. One fresh live
  replacement and remount passed with zero failed attempts; commits took
  153.2 and 193.7 seconds and upload confirmation took 398.0 seconds. This
  avoids premature deadline drops in that run but does not resolve the
  retained recovery case or establish stable latency. Other providers keep
  their 125-second default. Checkpoint inspection now has a separate
  provider deadline too: the isolated replacement requests 300 seconds for
  its full integrity readback. A negative-control worker test proved the
  old fixed limit ignored this setting; the corrected worker preserves the
  checkpoint and payload through an inspection timeout and uncertain
  reconciliation, then resumes without starting a second upload. A
  [read-only audit](benchmarks/icloud-retained-replacement-audit-2026-09-29.md)
  confirmed the retained fixture still has its original in Trash and its
  staging name in the test folder. That fixture has not been repaired.
  New handoff checkpoint version 2 now separates read-only `InspectInstall`
  from potentially sent `InstallInspected`. A restart may redo the former's
  full integrity checks; the latter and legacy `InstallNew` remain uncertain
  unless a complete receipt is independently observed. The worker persists
  the potentially-sent boundary before the rename. This does not make the
  Apple rename conditional or resolve its concurrent-editor race.
  Outer replacement checkpoint version 2 also captures the original and parent
  nodes alongside the original digest. Inspection, commit and reconciliation
  reconstruct from that sealed snapshot, rather than requiring the old ID to
  remain in the active metadata index after Trash. The isolated factory keeps
  this exact per-operation context for receipt validation. Cross-account,
  operation, source and payload bindings are checked before use. Version 1
  retains its previous index requirement. The
  [restart experiment](benchmarks/icloud-install-preflight-restart-2026-09-29.md)
  records the failed first live attempt and the separately registered follow-up.
  The corrected follow-up passed one controlled interruption during read-only
  preflight, fresh-process completion and another mounted read. Both upload
  receipts are durable despite the original ID being absent from the active
  index; one failed attempt records the deliberate interruption. The older
  ambiguous partial fixtures remain retained, and no broader reliability claim
  follows from this single run.
  Root replacements use handoff plan version 4 with no invented parent. Older
  nested plans keep their existing meaning. The new root restoration test first
  failed because a remaining checkpoint comparison required a parent; the
  corrected comparison preserves the explicit root identity and rejects an
  invented parent. Normal-build and probe tests cover both paths. The account
  router now dispatches replacement begin, inspection, streaming, commit and
  reconciliation. A fresh operation resolves the original and its plain-folder
  ancestry in one scoped SQLite snapshot, checks the expected ETag, and reserves
  the exact operation's Trash recovery location through the journal contract.
  Checkpoint-bearing calls restore the captured source without consulting the
  active index. The router retains the tested 300-second inspection and commit
  deadlines for full integrity readback. Synthetic root and nested cases prove
  checkpoint restoration after the index changes and reject cross-operation,
  cross-account and malformed checkpoints. Two [account-router live arms](benchmarks/icloud-account-router-uploads-2026-09-30.md)
  have now passed create, replacement, independent digest/Trash checks and reopening
  the durable journal through the real `WriteContext` and `TransferWorker`.
  This developer-only arm uses fresh isolated state and owned fixtures, without
  FUSE. They took 503.121–508.248 seconds (5.127-second spread); the first mounted
  acceptance result is recorded below, while broader write coverage remains open. Prior owned-fixture runs alone do not close these gates.
- `ICloudFileCreate` accepts zero-byte creates and uses the shared streamed
  path for larger files; the current capacity evidence is recorded below. The [zero-byte protocol investigation](benchmarks/icloud-empty-file-create-2026-09-29.md)
  found that Apple's successful content response supplies checksum/reference/key
  fields without a nonempty receipt. Only size zero may omit that field; nonzero
  uploads retain the receipt requirement. Registration omits an absent receipt,
  and both content metadata and the reserved ID remain in sealed checkpoints.
  A regular-router live create plus fresh-process journal/revision checks and
  mounted EOF read passed. A subsequent [mounted truncate/refill arm](benchmarks/icloud-mounted-empty-replace-2026-09-30.md)
  passed nonempty-to-empty and empty-to-nonempty replacements, independent
  digests, both exact predecessors in recoverable Trash, and reading after remount.
  This one arm took 924.709 seconds; it does not establish reliability or useful
  small-file latency.
  Content uses one HTTP POST per file, not resumable network chunks; a lost POST
  result must reconcile the reserved exact item rather than resend blindly.

## Account-router live acceptance

The developer-only `--account-uploads RUN_UUID` and `--account-namespace RUN_UUID`
modes use disabled isolated accounts, fresh owned remote folders, the real
Engine-owned `WriteContext`, sealed state and regular transfer/mutation workers.
They do not mount or enable ordinary account writes. Reused run directories are
refused; interrupted or uncertain state is retained rather than retried blindly.
The queue guards have passing tests and failing negative controls in local checks
and CI. A replacement is queued through the working-file/namespace path, so the
real two-ID handoff contract is exercised.

[Two upload arms](benchmarks/icloud-account-router-uploads-2026-09-30.md) passed
create, replacement, independent digest/Trash checks and journal reopening.
[One namespace arm](benchmarks/icloud-account-router-namespace-2026-09-30.md)
passed nine folder/file operations plus a file create, independent moved content
verification and reopening all Applied receipts. These narrow live results close
the gap between fixture-specific adapters and the normal account router for the
recorded cases. The later mounted, empty-file and combined-relocation results
are described below. Broader FUSE application behavior, root replacement, large
files, concurrent changes and general release acceptance remain open.

A first [mounted account-router arm](benchmarks/icloud-account-router-mounted-2026-09-30.md)
also passed FUSE create, truncate/replace, independent remote verification and
reading after unmount/remount in 524.468 seconds. Its authorization wrapper admits
only confirmed in-tree test identities and delegates the regular writer unchanged.
This is not an installed-daemon or general mounted-operation release gate.

## Combined move and rename

The account router now dispatches requests that change both parent and name to a
[three-step durable relocation](benchmarks/icloud-combined-relocation-2026-09-30.md):
rename to an operation-specific temporary name, move the same ID, then rename
to the final name. Every child has a persisted Ready/Sent boundary and captured
current node. Receipts advance and seal that node before another child can run.
Sent stages reconcile without dispatch; ambiguous observations require review.
A confirmed intermediate step is not reported as the final operation's success.
Recovery never reconstructs a later step from a changed metadata index.
The source content digest is captured once for files and revalidated by each
existing child adapter. Descendant destinations are refused before plan storage.
Synthetic file/folder tests pass; deleting the pre-dispatch save makes the
marker-order assertion fail. One fresh live account-router arm passed both
file and populated-folder combined relocation with both naive orderings blocked,
independent content checks and reopening all seven mutation/three upload receipts.
Its 348.028-second duration is a single functional observation. The first arm
stopped during dotted-folder setup. Inspection exposed a receipt-parser gap:
names were not reconstructed from the separate extension; the parser correction has a failing-before
regression and the fresh arm confirms dotted-folder creation. The retained failed
arm was not replayed. Combined FUSE and live interruption acceptance remain
open. Conflicts may leave a temporary name/location; recovery UI must represent
that before general release. No server-side atomicity is claimed.

## Implementation and acceptance sequence

Steps 1–3 below describe the now-implemented regular router's invariants, within
its documented item/size limits. The account-router and first mounted arms above
exercise part of step 4; they do not close all of it. Step 5 remains disabled.

1. Use the new owned `WriteContext` to construct the iCloud write factory.
   The account state, metadata index, shared journal and sealed upload vault
   are now available after Engine establishes ownership. The operation
   router must resolve `Scope(account, collection, item)` against the indexed
   node and its parent chain, then independently re-observe the exact parent,
   name, ID and ETag before any Apple mutation. Never infer identity from a
   path or a duplicate name. SQLite lookups must end before network awaits.
2. Extend the now-tested mutation-free original-hash preflight beyond the
   owned validator. The account-wide router must obtain the exact source
   node and parent from persisted metadata, then checkpoint the independently
   verified digest together with source ID, ETag, parent and target operation
   before allocating or registering staged content. Reopening a replacement
   must rebuild solely from the journal and sealed checkpoint even if the old
   ID is in Trash.
   A missing or corrupt checkpoint after a recorded mutation remains
   `verify_required`, never an automatic fresh upload.
3. Route Create, Replace, folder Create, file/folder relocate and recoverable
   Trash by request and exact node kind. Use the existing adapter validation
   and two-ID journal receipt contract. Use the sealed per-operation upload
   checkpoint vault for iCloud; keep the shared worker's normal vault for
   other providers. Refuse unsupported item types and sizes before accepting
   a local edit, with a visible explanation.
4. Test synthetic races first: source changes between index resolution and
   preflight, source changes just before conditional Trash, duplicate names,
   missing checkpoint, process death in each handoff phase, and loss of the
   upload POST or registration response. A test offered as proof of a fix
   must fail before it. Then run registered live arms only on new Cirrove-owned
   files, including restart and independent byte/ID verification. Repeat
   reliability arms and report within-arm spread, rather than declaring a
   single pass reliable.
5. Only after those gates pass, expose an explicit iCloud Allow changes
   choice, accept its persisted access mode, and install the account-wide
   writer. Keep existing read-only connections read-only. Validate the exact
   installed daemon and file-manager behavior separately from a green PR.

Remaining product questions include multi-gigabyte files and slow-link deadlines,
prolonged session expiry, quota failures, concurrent editors, ordinary
application atomic saves and recovery UI. No ordinary iCloud write path is
enabled by this document.

## Exact-item transport and replacement verification

The [September 30 controlled comparison](benchmarks/icloud-trash-exact-lookup-2026-09-30.md)
found and corrected the direct-item request/response envelope. Six production
lookups matched full Trash metadata, including restore paths and explicit Trash
parent/type, in 0.817–1.110 seconds (0.293-second range). Full inventory queries
in that repeat took 29.237–29.992 seconds (0.755-second range).

Replacement verification now uses two exact-item observations around the full
content digest check instead of two complete Trash inventories. Both observations
require matching Drive/document IDs, FILE type, explicit Trash parent, recovery
metadata, size and ETag; names and actual restore metadata must remain unchanged.
This is proof of one item's presence only. Absence and complete-folder checks in
other mutation paths still use their existing inventories.

A new owned mounted create/truncate/refill/remount arm passed with independent
full-list Trash oracles and content checks. It took 508.030 seconds versus the
preceding arm's 924.709 seconds; one whole-workflow arm per implementation does
not establish performance spread or production reliability. Roughly 30-second
post-mutation verification delays still need investigation. Ordinary iCloud write
access remains disabled.

## Larger files and pending-byte budgets

The [large mounted account arm](benchmarks/icloud-mounted-large-account-2026-09-30.md)
passed create at 65 MiB + 17 bytes, replacement at 66 MiB + 29 bytes, independent
whole-file digests, exact predecessor in Trash and full read after remount.
It used a fresh disabled account with a 512 MiB budget and the regular router.
The manager now honors `cache_bytes` for its separate pending-upload budget,
rather than always opening a 64 MiB journal. Reopening below retained usage
preserves pending data and refuses new growth; a synthetic manager regression
failed before this correction and passes after it.

Normal adapters use the journal's signed 64-bit representable length bound,
not the old 32 MiB fixture bound. That numerical bound is not a tested Apple
capacity. Content POSTs explicitly allow 900 seconds; verification downloads
allow 300 seconds. Existing outer phase deadlines still apply. Multi-gigabyte,
slow-link and quota/recovery acceptance remain open. A separate local regression
checks cancellation between 64 KiB blocks in the iCloud adapter's upload hash;
it does not establish cancellation behavior for every shared disk operation.

## Editor saves and the next release gate

The [mounted atomic-save validation](benchmarks/icloud-mounted-atomic-saves-2026-09-30.md)
now covers two successive temporary-file replacements through the regular account
router, retained old descriptors, five confirmed uploads, two temporary-file
cleanups, four recoverable predecessor identities and a final read after remount.
Three earlier failed arms remain retained and documented. One complete live pass
establishes this sequence, not repeated reliability or concurrent-editor safety.

The journal now reserves the replaced victim's identity for recovery and commits
all replacement bindings together. A queued successor resolves from its own
confirmed predecessor receipt when the background metadata index has not yet
seen the new ID. Ordinary-file mutation matching does not mistake optional
content-lineage decoration for a changed provider observation; identity, ETag,
name, size, kind and the independent content check remain enforced.

This ordinary-write development table records the earlier router milestone.
Later bounded native imports and Numbers replacement evidence below supersede
its original native-document checklist. The current six full-release criteria
are maintained in [product milestones](product-milestones.md#7-full-icloud-release-acceptance),
with one closed and five open; these rows are not additional release criteria.

| Gate | Current evidence | Required before enabling ordinary writes |
| --- | --- | --- |
| Create/edit/replace and editor saves | Owned live arms, including consecutive atomic saves and [create registration-confirmation loss](benchmarks/icloud-registration-recovery-2026-10-01.md) | Repeat representative application workflows; preserve all failures |
| Rename/move and recoverable deletion | Owned adapter/router arms; mounted combined file and populated-folder relocation, including file acknowledgement-loss and both intermediate checkpoint-loss recoveries without repeating completed steps; mounted unlink recovered after confirmed Trash before journal acknowledgement | In-flight uncertainty, concurrent intermediate changes, other deletion boundaries and repeatability |
| Interrupted replacement | Mounted process-recovery arms passed after [confirmed Trash](benchmarks/icloud-mounted-process-recovery-2026-09-30.md) and [final installation](benchmarks/icloud-mounted-final-recovery-2026-09-30.md), with both versions, journal ownership and remount verified; isolated [staging-body interruption](benchmarks/icloud-replace-stream-interruption-2026-10-01.md) recovered with original preserved, local export and fresh retry; [replacement-stage registration confirmation loss](benchmarks/icloud-replace-registration-recovery-2026-10-01.md) recovered the same staged ID without reupload/registration | Other in-flight boundaries and repeatability; preserve both versions |
| Concurrent changes | Controlled mounted same-ID races passed for ordinary saves, two pending autosaves and one atomic editor replacement and [two consecutive atomic saves](benchmarks/icloud-mounted-atomic-chain-2026-10-01.md); separate versions, receipt-gated editor cleanup and remount verified | More complex chains, intervening namespace operations, repeated competing edits and abandoned internal staging cleanup |
| Recovery UX | Durable retained journals/checkpoints; local export picker and receipt-checked progress dialog; separate account notice and upload activity for unconfirmed outcomes, covered by synthetic journal/event/window tests; offline sealed/working-byte selection and real native-dialog export | Active-account and [read-only downgrade recovery](benchmarks/icloud-readonly-recovery-2026-10-01.md) have core/socket/CLI/FUSE and native-window evidence; installed validation, per-operation explanation and audit of earlier retained fixtures remain open |
| Capacity and sessions | Typed session rejection preserves checkpoints and bytes through [synthetic upload recovery](benchmarks/icloud-write-session-rejection-2026-10-01.md) and [namespace-operation recovery](benchmarks/icloud-mutation-session-rejection-2026-10-01.md); 65/66 MiB and [1 GiB mounted arms](benchmarks/icloud-mounted-gib-account-2026-10-01.md); exact remote digests, recoverable predecessor and fresh-mount reads; explicit deadlines | [Synthetic storage refusal](benchmarks/icloud-storage-refusal-2026-10-01.md) and [actual local ENOSPC export](benchmarks/icloud-full-device-export-2026-10-01.md) passed their bounded arms; [provably unsent folder-create retry](benchmarks/icloud-folder-create-recovery-2026-10-01.md) also passed its bounded synthetic arms; real quota/slow-link/expired-session and larger-file acceptance remain open |
| Native document packages | Owned Pages import, explicit replacement, independent current/Trash content verification and Apple open; Numbers import/formula-open and separate exact-content readback; canonical archive saves, atomic replacement, retirement and backup-first handling have synthetic kernel coverage | Bounded public Pages import and genuine Numbers replacement have since passed; complete the native editor/representation matrix, standalone Trash and conflict acceptance, wider restart/session recovery and installed transitions; arbitrary generated-child editing remains refused |
| Installed release | Experimental isolated mounts | Explicit opt-in, existing read-only accounts preserved, packaged installation and file-manager validation |

The first new recovery test must use a fresh Cirrove-owned fixture, register its
interruption point and predicted endpoint before running, record its binary and
process IDs, and retain all partial state. Previously failed fixtures are evidence,
not retry targets for turning a failed arm green. The normal installed daemon and
its access mode are unchanged while these gates are open.

The first mounted process-recovery gate is now closed for the registered
post-Trash/pre-acknowledgement boundary. A fresh process recovered the same save,
verified both full byte versions and retained separate current/recovery journal
owners. This was an intentional process exit, not an orderly shutdown, but it
occurred after the provider response and postflight were known. The next acceptance
work is final-installation acknowledgement loss and controlled concurrent edits;
ordinary write access stays disabled.

The second mounted process-recovery gate also passed: after final installation but
before journal acknowledgement, a fresh process recovered the same operation by
inspection alone, with zero replacement commit calls. Both cloud digests and
separate current/recovery journal owners were verified. The subsequent competing-edit result below covers the shared keep-both path for
one ordinary current save. These two controlled
points do not establish all crash timings or complete the full-integration goal.

## Shared conflict rescue

The shared journal's `keep_both` previously enqueued a Create separately from
marking the refused save Resolved, without transferring its mounted namespace.
The copy was absent until upload, and the original path retained the local edit.
The journal now publishes the rescue queue row, working-file name, namespace
ownership and resolution atomically. A distinct durable alias restores the
provider entry without changing an existing local descriptor's identity. Kernel
invalidation follows projection publication. No network operation runs inside
this transaction.

Synthetic regression evidence (2026-09-30): the mounted test
`real_keep_both_restores_the_remote_path_and_exposes_the_copy_before_upload`
failed without the change because the rescue path did not exist. An intermediate
fix exposed a second failure: the original path still returned local bytes,
including after five seconds. The final regression checks both versions while
the rescue upload is blocked, an already-open descriptor and a fresh remount.
Three working-file tests also fail with the old `keep_both`: injected failure
when marking Resolved left a second upload queued; occupied names were accepted;
and a newer unsealed edit was accepted as resolved. The corrected transaction
rolls back the entire publication and retains all bytes on refusal.

The first implementation covered an ordinary refused current save. The extension
below now handles untouched linear successor saves and seals newer dirty bytes.
Atomic editor replacement records, native packages, cross-object dependencies and
uncertain successors remain refused and visible as unresolved. Remote staging cleanup remains required. The real iCloud result below complements
this synthetic coverage; neither enables normal writable iCloud connections.

Full validation: `scripts/check.sh` passed at 2026-09-30T18:43:17Z,
including workspace, feature-gated iCloud, kernel-mount, script and ledger checks.
The retained local runner manifest is
`.local-state/icloud-keep-both-check-2026-09-30-final/run.json` (exit 0,
disk-backed btrfs temporary storage). Display-dependent window scenarios were
not run; this change does not alter desktop widgets. An earlier check was stopped
before completion to fix the regression fixture retaining its old Engine across
remount; it is not counted as a pass. The existing rustdoc broken-link warning
for `retry_stuck` remains unchanged. This worktree change is not installed into
the regular daemon.


## Mounted competing-edit acceptance

The [registered live scenario](benchmarks/icloud-mounted-competing-edit-2026-09-30.md)
now passed for an ordinary current save. A feature-gated one-shot hook changes one
byte in the exact owned original after replacement preparation, then returns to
the normal adapter; it does not synthesize the Conflict outcome. The adapter
refused the changed revision, and both cloud/local contents remained intact.
Keep-both after a mount restart uploaded an independently verified rescue copy;
another remount retained both versions. A read-only journal audit confirmed
distinct local and remote owners and no incomplete queue reservations.

The first arm exposed a harness contract mistake in proving absence from Trash;
it is retained as failed, not counted as successful. The corrected arm ran against
a fresh UUID-owned fixture. Staged-file cleanup and atomic editor conflict chains remain open. The later
autosave result below separately covers newer ordinary generations.


## Pending autosaves in a conflict

The [registered mounted autosave arm](benchmarks/icloud-mounted-conflict-autosaves-2026-09-30.md)
passed with two newer saves queued during the prepared iCloud replacement. The
shared journal now follows the untouched linear upload chain, selects its newest
payload, atomically resolves the superseded saves and publishes one rescue Create.
Newer dirty working bytes are first sealed durably; a later publication failure
retains that sealed Pending generation behind the original conflict. Every older
immutable payload remains retained.

The live arm verified both remote contents and both mounted paths after remount.
Its independent audit also reread and hashed all three superseded local payloads.
Synthetic regressions cover unsealed bytes, transaction rollback when a later
resolution fails, preserved open descriptors and refusal of external dependents.
This does not cover atomic editor ownership transfers or uncertain successors,
and no ordinary iCloud write opt-in is enabled by it.

## Atomic editor conflict rescue

The [registered atomic conflict arm](benchmarks/icloud-mounted-atomic-conflict-2026-09-30.md)
passed against a fresh owned iCloud fixture after correcting the test helper's
extra-entry policy. The mounted editor temporary file and refused victim retain
separate identities. Keep-both now restores the changed cloud victim through a
fresh alias and preserves the editor stream under the rescue name. It supersedes
only a never-attempted cleanup, with no fabricated deletion receipt, and appends
a new conditional cleanup behind the rescue Create. The old cloud original is
never that cleanup's target.

The live arm confirmed the competing edit, both full contents, rescue after
remount, independent rescue digest, editor temporary ID in Trash and both named
versions after another remount. A read-only journal audit verified distinct owners,
retained conflicted payload, backward prerequisite ordering and an empty pending
queue. Synthetic tests additionally cover old descriptors, pending source upload,
transaction rollback, uncertain cleanup, external dependents and ordinary rescue
after a previously completed atomic replacement.

This closes one atomic replacement conflict case, not arbitrary replacement
chains or namespace recovery. Provider-internal staging is still retained.
Normal iCloud write settings, installed daemon and existing accounts are unchanged.

## Mounted relocation recovery and explicit content evidence

The [registered mounted relocation arm](benchmarks/icloud-mounted-relocation-recovery-2026-09-30.md)
now covers combined move/rename with collisions preventing either naive ordering,
process loss after the complete file receipt but before journal acknowledgement,
and a subsequent populated-folder move through FUSE. A fresh process recovered the
file operation without a second mutation call. Independent digests, identity checks,
blocker preservation and another remount passed. Intermediate-step and in-flight
uncertainty remain separate gates.

The initial run exposed a real provider-neutral acknowledgement gap: iCloud's
full-byte-verified result lacked a content-version token, so the journal correctly
refused an apparently namespace-only observation as NeedsReview. The corrected
router carries explicit verified-content evidence. The journal binds and persists
account/provider/collection/item, both ETags, size and SHA-256 before allowing later
writes to use the recovered receipt. Normal unproven observations retain their
existing conflict/NeedsReview guard. Source digests come from pre-dispatch capture;
a newly hashed current file alone cannot authorize recovery. Completed relocation
plans and verified ordinary file rename/move inspections use this contract without
inventing cache content-version tokens.

The failed fixture remains retained. The corrected live run proves one final
acknowledgement-loss boundary and the registered mounted operations, not the full
release matrix; ordinary iCloud writes are still disabled.


## Mounted intermediate relocation checkpoints

The [two registered intermediate-boundary arms](benchmarks/icloud-mounted-relocation-intermediate-2026-09-30.md)
passed with fresh owned fixtures. Each exits after a real child response but before
persisting the next Ready plan. Fresh-process recovery inspects the old Sent step,
then performs only remaining child operations: move/final rename after the first
boundary, final rename after the second. Exact dispatch sequence guards and a
separate SQLite audit supplement independent full-content hashes and remount
checks. No production transport or state-machine behavior changed for these tests.
The failpoint vault is compiled only into the experimental write-probe feature.

These results close those two confirmed-response checkpoint-loss cases, not all
relocation failure modes. In-flight response loss, concurrent edits at temporary
paths, repeatability, deletion interruption and installed write acceptance remain
open. Setup for two tiny files still takes minutes in these runs; stage-specific
latency measurement remains necessary before claiming usable ordinary write speed.


## Write-latency diagnosis

The [phase-level diagnostic](benchmarks/icloud-write-phase-latency-2026-09-30.md)
passed create/replacement, preserved the old version in Trash and verified both
contents and reopened receipts. It attributes six roughly 30-second waits to
folder metadata requests; upload, registration, signed download lookup and small
content transfer were much faster. The instrumentation exists only in write-probe
builds and logs static phase labels and durations, never IDs or provider bodies.
It does not establish which folder role or server condition causes the slow calls.

A read-only reduced-metadata experiment did not produce the expected schema and
is not used by production writes. Warm full root listings were fast. No write
performance improvement is claimed yet; controlled post-write attribution and a
replacement that preserves all identity/collision checks remain required.


File-create parent verification now reads the exact known folder's full envelope,
checking its ID, current name, kind and parent identity, rather than scanning all
of its grandparent's children. This also applies to staged uploads. Complete
listing checks are unchanged; no partial metadata projection is enabled. Handoff
sibling-name collision checks remain separate and unchanged. A negative-control
HTTP regression demonstrated the old ancestor request before the implementation
changed; foreign IDs, moved/renamed/non-folder parents, duplicate envelopes and
incomplete replies remain refused. Bounded live results are recorded with the
phase-latency experiment; a general write-speed or release claim does not follow.


## Exact file deletion recovery

File-Trash reconciliation now inspects the journal's exact prepared identity
instead of requiring a complete global Trash inventory. It checks recoverable
Trash binding, document identity, ordinary representation and the saved full-byte
digest, then repeats the metadata check to reject concurrent changes. Unknown or
changed observations retain uncertainty or conflict; they never authorize replay.
The [registered recovery arm](benchmarks/icloud-file-trash-exact-recovery-2026-10-01.md)
passed with a fresh owned file and a fresh recovery process whose adapter refused
any mutation dispatch. An independent Trash lookup and journal audit confirmed
the exact Removed receipt. Mounted deletion interruption and broader repeatability remain
separate release gates.


## Mounted deletion after process interruption

The [registered mounted deletion arm](benchmarks/icloud-mounted-delete-recovery-2026-10-01.md)
passed using the ordinary account router: FUSE unlink retained an open handle's
original bytes, then the process exited after confirmed recoverable Trash but
before returning the receipt to the mutation worker. The fresh process observed
VerifyRequired with the exact prepared ID, completed by inspection with all
mutation dispatch forbidden, and retained the exact Removed receipt. Independent
cloud checks confirmed Trash presence and active-parent absence; a second mount
confirmed the pathname remained absent. No permanent deletion occurred. This closes
only the registered post-confirmation/pre-journal-receipt boundary. In-flight
uncertainty, other boundaries, repeated workloads and installed write acceptance
remain open. Ordinary iCloud write access remains disabled.


## Local recovery export

A [service/CLI export](local-recovery-export.md) now copies one retained sealed
save by operation ID without retrying or resolving it. It uses bounded streaming,
size/digest verification, no-overwrite publication and cancellable background jobs.
[Synthetic real-mount/socket/CLI evidence](benchmarks/local-recovery-export-2026-10-01.md)
confirms copied bytes and unchanged conflict, with cloud mount destinations refused.
The [desktop validation](benchmarks/local-recovery-desktop-2026-10-01.md) adds
bounded version selection, a local save chooser and receipt-checked progress.
Disabled-account CLI export is now covered by [offline recovery tests](benchmarks/local-recovery-offline-2026-10-01.md).
Offline working-byte export is covered by [synthetic recovery tests](benchmarks/local-working-recovery-2026-10-01.md).
The [offline desktop picker](benchmarks/offline-recovery-desktop-2026-10-01.md) now supports both source kinds.
Active-account working-byte export and installed acceptance remain open;
this does not enable ordinary iCloud writes.

## Ordinary streamed reads

The iCloud adapter now supplies version-bound `open_read_session` transports for
ordinary files. Sequential reads can use the shared service's adaptive windows,
up to 64 MiB, with chunks of at most 64 KiB written to private staging. This is a
maximum capability, not eager whole-file prefetch. Single/random reads retain the
exact-range fallback. Four read permits are shared across both paths. Ordinary
reads retain the service's 30-second request deadline.

Each window checks the exact parent/item ETag and size before transfer, obtains a
fresh ordinary content URL, validates the response range/length/encoding, and
checks metadata again after streaming. Only success permits staging publication;
errors and cancellation require discarding the window. Signed URLs are not
revision evidence. Package artifact staging remains a separate interface.

Synthetic tests exercise varied bytes and a partial final block, changed initial
and final revisions, malformed/truncated/oversized/encoded responses, sink errors,
cancellation while waiting for the shared budget, scope and revision binding.
The final-revision test was demonstrated to fail with its guard removed before
restoring the guard. Content transfer fixtures use local HTTP responses directly
at the response-processing boundary; production URL validation remains unchanged.
They are not a complete Apple HTTPS end-to-end test.

The owned mounted validator forwards the new read-session interface with item and
parent ownership checks. Fresh-cache varied-content live comparison, mid-transfer
network interruption and installed acceptance remain open. No speed or release
claim follows from the synthetic tests, and ordinary write permissions are not
changed by this step.

Validation: complete `scripts/check.sh` passed on 2026-10-01, 01:03:52–01:10:39
UTC, using the worktree-specific target and disk-backed btrfs temporary storage.
Manifest/log: `.local-state/icloud-read-windows-check-2026-10-01/`. The five new
adapter tests passed in both normal and write-probe test configurations. The
existing shared-service tests also cover discarding partial staged windows and
resetting after cancellation. No GUI changed; display scenarios were not rerun.
At that checkpoint the windows had not yet been installed or live-benchmarked.


The subsequent [controlled live comparison](benchmarks/icloud-read-windows-2026-10-01.md)
found and fixed a receipt compatibility error: upload receipts carry the same
revision in both `content_version` and `etag`. Matching revisions now qualify for
windows; distinct synthetic revisions retain exact-range reads. The regression
test failed before the fix and passed afterward. On one fresh 64 MiB + 17-byte
varied-content fixture, three fresh-cache arms per variant passed exact bytes,
SHA-256 and cached sparse rereads. Median read duration was 28.394 s for exact
ranges (26.918–31.348 s) and 14.247 s for windows (13.502–14.272 s). First-read
latency did not improve. This validates the cache path against Apple, not FUSE,
GUI or installed behavior; write/recovery release gates remain unchanged.

## In-flight create stream recovery

A [controlled live process interruption](benchmarks/icloud-stream-interruption-2026-10-01.md)
now covers the HTTP body itself: the developer-only probe stalled after yielding
8 MiB of a 64 MiB + 17-byte varied-content create, then exited before worker
acknowledgement. The fresh process recovered the whole local generation and the
allocated/no-receipt checkpoint. Read-only inspection/reconciliation proved it
uncommitted; no mutation was attempted during that phase. Local export passed.
Only afterward did a fresh transport attempt complete, with one visible file,
a new document identity and independently verified remote digest. This is a
successful create boundary, not a blanket transport/replacement or release gate.
The fault observer is feature-gated out of ordinary binaries; installed behavior
and ordinary write permissions were not changed.


The subsequent [replacement-body interruption](benchmarks/icloud-replace-stream-interruption-2026-10-01.md)
also passed in a fresh process. After yielding 8 MiB of the new 64 MiB + 17-byte
body, the probe exited. The original remained the sole visible file with verified
old bytes before and after read-only reconciliation. The new local generation
was preserved/exported; only after the interrupted Stage was proved uncommitted
did a fresh attempt finish the replacement. Independent new-content hashing,
exactly one visible result and the original exact ID in recoverable Trash passed.
This adds an account-router staging-body boundary, not a mounted application or
blanket transport/release result. A local fixture bug (one oversized working write)
was reproduced by a failing regression and fixed with bounded writes; the failed
setup attempt remains recorded. Ordinary write permissions remain unchanged.

## Exact-folder handoff identity

New non-root replacement handoffs use plan version 5: one complete exact-folder
response must match the captured folder ID, actual parent, name and kind before
its children enter the existing file identity, revision and name-conflict checks.
Unrelated same-name sibling folders do not redirect ID-addressed operations and
are allowed under this explicit contract. Saved versions 2/3 retain their original
parent-listing and sibling-name uniqueness checks; root version 4 is unchanged.
The registered [functional validation](benchmarks/icloud-handoff-folder-identity-2026-10-01.md)
separates this reduced request path from unmeasured latency claims. Ordinary
installed iCloud writes remain disabled pending the release gates above.


## Lost create registration confirmation

A fresh account-router live arm now covers a remotely registered 64 MiB + 17-byte
create with its confirmation deliberately unaccepted. The worker retained
`verify_required` and its completed-body checkpoint; independent remote hashing
proved the reserved document already existed before exit 86. A fresh process
exported the retained local bytes and completed through one inspection, with no
mutating replay, duplicate or document/revision change. The
[registered protocol and result](benchmarks/icloud-registration-recovery-2026-10-01.md)
retain the binary/process evidence. This is a create confirmation-loss boundary,
not all replacement-stage, in-flight network or power-loss outcomes, and it does
not enable ordinary iCloud writes.


## Consecutive atomic saves through a conflict

The shared rescue transaction now handles untouched chains beginning with a
refused atomic editor replacement, including ordinary successor saves on those
streams and newer dirty bytes on the final stream. It resolves the superseded
saves together, restores only the oldest cloud victim's alias, and queues each
confirmed editor temporary's conditional cleanup behind the new rescue receipt.
Older detached working streams and immutable save payloads remain retained.
Existing confirmed temporary-upload receipts are locally rebound through the
worker's bounded readiness pass before validation; no operation is claimed there.

The [registered mounted iCloud arm](benchmarks/icloud-mounted-atomic-chain-2026-10-01.md)
passed with two consecutive atomic saves and a real competing edit. Rescue after
restart, independent full hashes, both exact temporary IDs in Trash, a second
remount and a read-only journal audit all passed. Three-link/dirty-tail,
transaction-failure, attempted-cleanup and external-dependency behavior has
synthetic coverage. This does not cover arbitrary interleaved namespace operations,
unconfirmed temporary sources, uncertain successors, or provider-internal staging
cleanup. Normal writable-account grants and installed release acceptance remain
unchanged and open.

## Next ordinary-write opt-in milestone

The [registered acceptance sequence](benchmarks/icloud-ordinary-write-opt-in-acceptance-2026-10-01.md)
defines the concrete access-transition, recovery, fault, application and installed
checks for the next intermediate milestone. It does not close any gate by itself
or redefine full iCloud support as ordinary-file writing. Native editing and
remaining compatibility/reliability work stay open. The
[active working-file recovery core](benchmarks/icloud-active-working-export-core-2026-10-01.md)
now has generation-checked private staging and a
[service/CLI route](benchmarks/icloud-active-working-export-service-2026-10-01.md)
verified through a held writable FUSE descriptor. The
[desktop picker and exact receipt handling](benchmarks/icloud-active-working-export-desktop-2026-10-01.md)
also pass synthetic native-window acceptance. Installed recovery acceptance remains
open.


## Storage refusal before a folder-create receipt

A typed HTTP 507 refusal keeps local work and requires explicit retry. This is a
server storage refusal, not proof of the account's quota or proof that an earlier
mutation did not commit. Signed verification downloads distinguish this response
from expired signed URLs: their 401/403 responses remain uncertain rather than
forcing account reauthentication.

Folder creation records a bound version-2 checkpoint before preflight. Its
`not_sent` state proves that the create POST was not dispatched. After an explicit
retry, this state allows the router to durably return to preparation and repeat
all parent/child checks. Immediately before the POST, the adapter saves
`may_have_sent`; only a confirmed identity changes that to `created`.

Missing checkpoints, legacy operations without a created identity, and
`may_have_sent` checkpoints remain `Indeterminate`. Explicit retry does not guess
an identity, infer absence from a name, or blindly replay creation, including
after a POST returned 507. Storage becoming available cannot by itself settle
those cases. Existing version-1 identity receipts remain readable. Synthetic
adapter HTTP and separate worker/router tests cover these boundaries; they do not
establish Apple's live quota response format or general quota recovery.


## Recovery on a retained read-only connection

The [read-only recovery path](benchmarks/icloud-readonly-recovery-2026-10-01.md)
keeps saved and working versions exportable through the daemon after access is
read-only. The journal remains read-only and the operation cannot start upload
workers. Desktop capability checks expose local copies separately from retry,
discard and cloud mutation controls. Installed iCloud access-transition acceptance
is still separate from the synthetic journal, service and window tests.

The [4 October retained-outcome regression](benchmarks/icloud-readonly-retained-status-2026-10-04.json)
found that the actual read-only manager reported zero warnings and omitted
failure lists despite retained journal records. The corrected local reader
reports complete counts and bounded failure names, and preserves last known
counts while a genuine competing journal owner makes recovery unavailable.
Four manager tests pass after two intended original failures; a targeted count
preservation omission also failed its registered assertion. Settings, retained
bytes and journal records remain unchanged, with no writer or retry.

The [historical activity-name regression](benchmarks/icloud-readonly-recent-names-2026-10-04.json)
also distinguishes downloaded files, whose original control passed, from locally
created and acknowledged files later replaced through Trash, whose original
activity displayed a hidden recovery alias. Completed replacement activity now
uses its confirmed historical receipt name while keeping the original intent
item, operation UUID and recorded collection. Seven tests covering 27 synthetic
cases pass, including later renames, unconfirmed states, damaged receipts,
linked collections and permitted Google filenames. Provider Trash names were
already preserved; this is a local display correction, not DATA-format loss or
Apple application acceptance. Installed transition and full-iCloud gates remain
open.

A [controlled full-device arm](benchmarks/icloud-full-device-export-2026-10-01.md)
also recovered a saved and newer working version from a full ext4 fixture to a
separate filesystem. This is local ENOSPC evidence, not a live Apple quota test.

## Native package creation — isolated validation

The [owned-package experiment](benchmarks/icloud-owned-package-create-2026-10-01.md)
now has a successful one-shot Pages PACKAGE upload, registration and exact
identity readback. Apple Pages opens the imported synthetic document with the
expected text. Independent root-bound archive comparison passed, followed by full reads through
the normal provider/FUSE mount, an offline remount and a fresh provider refetch.
This feature-gated validator does not yet route native saves through the normal
journal or enable package editing in mounted accounts. Ordinary native-package
write refusal remains in place pending durable create/replacement integration
and its separate recovery, conflict and mounted acceptance.

### Explicit native import under development

The development branch now routes a validated Pages archive through the normal
daemon and durable upload journal. This is a create-only import into an active
experimental writable iCloud mount, with an explicit source archive root:

```sh
cirrove import-native-package --label iCloudValidation \
  --account-id '<account-uuid>' \
  --archive /absolute/local/Source.pages --source-root Source.pages \
  --parent 'Validation' --name 'Imported.pages'
```

Use the selected connection's UUID from `cirrove status` for `--account-id`.
The optional flag binds submission and observation to that account and requires
version 1 of the service's `import-native-package-account-binding` capability.
If the label has been reassigned, the daemon refuses the mismatched account
before starting an import. Omitting the flag retains intentional label lookup.
The [CLI identity controls](benchmarks/icloud-native-import-cli-account-binding-2026-10-03.json)
exercise the actual executable and synthetic socket, including unsupported
capabilities and a same-label foreign-account result. They do not establish
live Apple application acceptance.

The source must be a regular local ZIP archive outside Cirrove mounts and private
state. Cirrove validates a private snapshot before allocation; an existing
destination is refused. Completion requires a verified remote identity and local
metadata publication. Closing the CLI stops watching, not the queued upload. If
confirmation is lost, inspect the retained operation before submitting another
copy: uncertain allocation is never blindly repeated.

The [public import artifact](benchmarks/icloud-public-native-import-2026-10-01.md)
records the ec7 arm's durable Uploaded receipt and metadata publication, followed
by independent exact-identity download, semantic comparison, FUSE reads, offline
remount and fresh refetch. Its initial public job falsely reported a revision
mismatch and never reached its original post-success mount endpoint. The narrow
receipt/projection correction passed the project checks; this does not rewrite
that historical result.

The subsequent [retained observation](benchmarks/icloud-native-import-watch-2026-10-01.md)
passed public watch completion after restart, confirmed the exact package in the
warm mount, and preserved the upload journal unchanged. Apple Pages then opened
that exact ec7 allocated document and displayed the synthetic source content,
without editing. The later [fresh public Trash arm](benchmarks/icloud-public-native-trash-2026-10-01.md)
also passed a new CLI import, independent semantic readback and mounted access
before removing its own document. This separate f2dec result is not Apple Pages
UI acceptance for that document or evidence of desktop import submission.

The capability-gated desktop native import dialog has synthetic native-window
coverage. Live portal selection through verified completion and publication on
the original GUI connection's mount remains open, as does installed click-through.
The acceptance-ledger import row remains unchecked. This route creates documents;
existing-document replacement uses a separate explicit operation. Development
admission also accepts matching Numbers/Keynote PACKAGE archives. The
[owned Numbers record](benchmarks/icloud-iwork-next-owned-fixtures-2026-10-02.json)
contains one imported copy opened in Apple Numbers with the expected cells and
formula, plus independent exact-content readback of a second copy. It does not
establish replacement, second-copy Apple reopen or broad application fidelity.
Keynote live application acceptance remains open.

The retained ec7 Apple UI observation file's SHA-256 is
`d7584efd648f02aa945233367aff27c7b82daba1fdbf3bdc2f0b48931a7e97a8`;
its watch job receipt is
`1a17d886dcd3ec7954a49c692352848955563ad97ab77269aeb0cce7766fc7ab`.
These identify the local artifacts cited by the retained-observation benchmark,
not a new browser inspection. The f2dec independent Trash receipt hash was also
rechecked against its benchmark and matched
`40724f9819fd89c1ba90749d1a3d9de96bae7c963684924acedf84d512d9ca75`.

### Observe a retained native import without submitting again

`watch-native-import` is a separate versioned capability and observer-only verb:

```text
cirrove watch-native-import --label <connection> --account-id <account-uuid> --operation <operation-uuid> --socket <socket>
```

It accepts only an existing scoped native Create operation on the selected active
writable iCloud account. It does not accept an archive, source folder, destination
or new name. It never captures, enqueues, retries, allocates, uploads or rewrites
that operation. An in-flight operation may continue through its already-running
workers independently of this observer.

The public request requires `label`, `expected_account_id` and `operation`; unknown
fields are refused. It returns the existing native-import job/receipt shape, with
the operation already bound in the initial job. This allows observation after a
daemon restart, an observer stop or loss of the original CLI connection. Jobs
remain ephemeral; the operation UUID identifies the durable saved import.

For an Uploaded package, the observer requires the saved semantic completion and
fresh exact-ID metadata matching its identity, name, parent, ETag and logical
size. It updates the normal metadata view through the existing revision fence;
it neither downloads package content again nor trusts an old publication record
as evidence of current visibility. A moved, changed or absent item fails the
observation without any upload retry. Legacy receipts with an exact ETag alias
remain unchanged. Account/mount ownership is checked again after the read.

The observer uses the existing twenty-minute overall deadline and cancellation,
with thirty seconds for the fresh metadata read. `cirrove stop` or Ctrl+C stops
only observation. An unavailable result is never an invitation to submit the
archive again: inspect or watch the same retained operation.

### Native document update evidence remains incomplete

The 2026-10-01 public Pages follow-up still found the Drive-backed loader, not
the editor content-update bundle. The public shell separately routes document
opens through `redirectChildApplicationForDocument`; this is routing evidence
only. Neither a same-ID package update body nor its predecessor-revision guard
has been established. Generic CloudKit record change tags do not establish the
Pages/CloudDocs schema or authorization contract. Retained public-source hashes
and findings: `/var/tmp/cirrove-public-pages-followup-vjy10iue/`. A two-ID package
handoff must not be described as identity-preserving native editing.

Likewise, existing Trash readers require non-null, unchanged `restorePath` but
do not establish its schema or destination. Synthetic fixtures disagree on its
shape. The owned checkpoint binds the pre-Trash parent; that is not proof of
restoration to that parent. A bounded structural observation on the exact owned
Trash item is required before adding a destination parser or claiming that gate.

A subsequent supported browser asset inventory of the already opened owned ec7
Pages test document established the actual editor scripts:

- `https://www.icloud.com/applications/ix/15D120/editor/15D120/en-us/main.js`
- `https://www.icloud.com/applications/ix/15D120/editor/15D120/en-us/0.main.js`

Only script URLs were collected, without reading private network responses,
credentials or application state. These concrete public assets permit further
protocol research; their discovery alone proves no update or conflict semantics.

The grounded editor review found SCMP command batches carrying `commandId` and
`basedOnRevision`, with acknowledgements and revision diffs. Older commands cause
catch-up and model reconciliation: this is not evidence of whole-archive CAS.
The editor's `forceUpload` persists its existing server model and takes no
replacement ZIP. Session authorization, model serialization, replay behavior and
verified Drive persistence remain open. Public-only research, without executing
JavaScript or contacting account endpoints, is retained at
`/var/tmp/cirrove-public-pages-editor-g8h19rk8/`.

Source integrity for editor build 15D120 (SHA-256):
`main.js`: `59ad7e5a4b8115b34c2b1d623d2c0f4cb21bee7e1fcfdf3f7890dd9793fb01a0`;
`0.main.js`: `954f9b76293da7f88b3e6e560ec1e9a2c23043e1471fe6a3903d8bf14ee3ae68`.
The next bounded investigation is session initialization and one text-model
command schema, not a guessed package mutation against an account.

An offline standard-library prototype now recognizes a bounded subset of GSSP
mailbox headers and revisions at `/var/tmp/cirrove-gssp-offline-20261001/`. All
five synthetic tests passed. Incorrect negative-reference resolution and a
one-byte relaxation of the input limit each caused test failures; restored code
passed again. It refuses unsupported document slices/variants and has no network
or edit transport. This is parser groundwork only, not live editor compatibility.
Further source analysis also establishes that owned CloudDocs returns no optional
document-access token; cookie/service session bootstrap remains unresolved.

Independent parser review found that a structurally present revision could still
contain null fields. The scratch summary now requires an int32 sequence and a
nonempty bounded identifier without changing low-level null decoding. All six
tests passed; removing this guard made the new test fail, and restored code
passed again (`usable-revision-negative.log` in the scratch directory).

The subsequent public-source request plan corrects fetch URL construction:
the concrete editor uses the manifest's `iwres.url` directly. The exact owned editor address and build constants are now grounded. Bounded
manifest and setup-readiness requests have run against the existing owned test
fixture; no SCMP session or native edit was started. The manifest rejected the
existing session, and setup readiness stopped because the returned Apple-ID did
not match the saved login address. Neither result proves why editor authorization
failed. See [manifest observation](benchmarks/icloud-owned-pages-manifest-plan-2026-10-01.md)
and [readiness observation](benchmarks/icloud-owned-pages-readiness-plan-2026-10-01.md).
A trusted stable account anchor is now captured only after successful account
login; validation cannot adopt an unmatched account or infer an alias. Legacy
snapshots remain supported. A one-shot renewal with the retained token was
rejected with HTTP 421; the next editor test awaits fresh isolated sign-in. See
[renewal result](benchmarks/icloud-owned-pages-renewal-plan-2026-10-01.md).


A fresh native Pages Trash arm now passed protocol-bound stale-revision refusal
(`ETAG_CONFLICT`, HTTP 200), independent unchanged E1/content verification,
current-revision Trash and semantic readback from the exact recoverable item.
A separate read-only process reproduced the durable completion report. See the
[bound-refusal Trash record](benchmarks/icloud-native-package-bound-refusal-trash-2026-10-01.md).
This proves one owned native metadata/Trash path, not native editing, replacement,
restoration or installed package deletion support.


### Native recovery and normal-service integration under development

The exact successful native Trash fixture was subsequently inspected read-only.
Apple supplied a bounded two-segment restore path matching the owned original
parent and renamed document; independent package semantics, Trash metadata and
the source-only original parent remained stable. This does not prove restoration
or server-atomic destination vacancy protection. See
[restore-path observation](benchmarks/icloud-owned-native-restore-shape-2026-10-01.md).

The subsequent single conditional restoration received a bound Apple receipt.
Initial postflight comparison refused a list/detail difference in optional
item_id; a failing HTTP regression and narrow target-only comparison fix were
followed by independent read-only verification of the same Drive/document IDs,
name and full native target/source semantics. The sealed ReceiptObserved history
was retained and no second mutation occurred. See the
[owned restoration record](benchmarks/icloud-owned-native-restore-2026-10-01.md).
This is one owned Pages recovery result, not general restore or replacement safety.

The next normal-service slice is explicit native-document Trash, distinct from
empty-folder POSIX removal. Admission must bind the selected account UUID, active
writable mount lifetime, original native container identity and ETag. Generated
archive children, shortcuts, app containers, pending conflicting operations and
read-only accounts remain ineligible. Metadata lookup and provider preparation
must occur outside journal locks; admission must recheck the same engine/writer
and namespace frontier after awaited work and before durable enqueue.

A queued operation must return its durable UUID and support observer-only status.
Completion requires exact recoverable Trash identity and semantic verification
plus publication of removal into the mounted view. Existing opened read sessions
must retain their version. An uncertain request is inspected, never replayed
automatically; absence alone cannot establish successful recoverable removal.
The new journal intent requires a compatibility fence against older writers.
The explicit service/CLI implementation is present in this worktree, including
bounded retained-operation discovery after a lost reply and restart. See the
[native Trash contract](native-document-trash.md) and
[service validation record](benchmarks/icloud-native-trash-service-2026-10-01.md).
One fresh owned Pages document has now passed the public normal-service import,
exact-revision Trash, retained-operation listing/watch, mounted disappearance,
held-reader and independent recoverable-content checks. See the
[controlled live result](benchmarks/icloud-public-native-trash-2026-10-01.md).
The initial cold-start preflight refusal remains unexplained. This is one
isolated successful arm, not installed release acceptance or reliability proof.

Numbers/Keynote acceptance remains separate. Retained read probes observed DATA
representations, whereas the owned Pages fixture used PACKAGE. Public import
was Pages-only at that validation point. The isolated development implementation
now admits matching Pages/Numbers/Keynote PACKAGE formats through the same
bounded checks; this has not been installed or released. Broadening extensions
does not establish native compatibility. Each format still needs a fresh UUID-owned synthetic document, actual
representation classification, independent content verification, mounted/offline
reads and reopening in the corresponding Apple application before release
acceptance. See the [format acceptance boundary](benchmarks/icloud-native-formats-acceptance-2026-10-01.md).
No existing user document should be repurposed as a write fixture.


## Explicit native archive replacement: current public contract

The current service exposes `replace-native-package`,
`watch-native-replacement` and `list-native-replacements` capability version 1.
The [explicit replacement guide](native-document-replacement.md) documents exact
CLI arguments, account/revision binding and recovery after a lost reply. This is
a separately submitted, validated local archive; ordinary writes to generated
package children remain refused outside the exact canonical working archive.
Development admission now includes matching `.pages`, `.numbers` and `.key`
PACKAGE formats; live Numbers/Keynote application acceptance remains open.

A confirmed replacement creates a new provider identity and records the old
identity in Trash. It does not promise same-ID updates, preservation of revision
history or sharing links, or an atomic replacement against another cloud client.
Stopping a watch stops observation; already queued durable work remains retained
and may finish. Lost replies require retained-operation discovery, not resubmission.

The [replacement validation record](benchmarks/icloud-native-package-replacement-2026-10-01.md)
records actual-worker synthetic HTTPS fault tests, encrypted checkpoint recovery,
router/admission checks, observer/publication checks and bounded read-only
discovery, including their failed negative controls. These synthetic results do
not prove Apple application fidelity or installed reliability. A subsequent
[owned Pages replacement](benchmarks/icloud-native-owned-v2-live-2026-10-01.md)
completed with a typed receipt and mounted publication; independent readback
verified both the edited current document and the original retained in Trash.
[Apple Pages opened the exact replacement](benchmarks/icloud-native-replacement-apple-open-2026-10-02.md)
and rendered its version-B marker. The
[Numbers replacement attempt](benchmarks/icloud-iwork-next-owned-fixtures-2026-10-02.json)
failed admission without a recorded durable replacement operation; it has not
been replayed. A matching local-parent selection defect was reproduced and
[fixed synthetically](benchmarks/icloud-native-parent-admission-2026-10-02.json),
with replacement and safety regressions passing. That result does not confirm
the original failure phase or a live Numbers replacement. The correction is
committed at `cc1470f`; its complete `scripts/check.sh` passed, including the new
regressions and actual kernel fixtures. Desktop window scenarios were not run.
Main reconciliation and observer integration were committed at `621a576` after
the complete contributor check passed, as recorded in the
[observer artifact](benchmarks/icloud-owned-receipt-observer-2026-10-02.json).
The later 3 October checks below also passed their combined full check.
Ordinary application saves, broader recovery and installed acceptance remain
open. Full-iCloud acceptance row 486 remains unchecked.

### Read only observer for owned Numbers receipts

The feature-gated validation binary adds this observer-only command:

```sh
cirrove-icloud-mounted-write-probe --owned-receipt-verify REGISTRATION SHA256
```

Its bounded registration binds a fresh UUID-owned Numbers run to the exact
account, created folder, completed import, optional replacement, distinct A/B
archive digests and semantic proofs. It holds an exclusive read-only journal
lease while independently checking the current PACKAGE and, after replacement,
the original in recoverable Trash. Fresh provider metadata, representation and
revision checks remain enforced, followed by unchanged registration, sources
and journal receipts. It constructs no write workers, submits no mutation,
replays no operation and migrates no journal; local proof artifacts are retained.

The [observer registration](benchmarks/icloud-owned-receipt-observer-2026-10-02.json)
records four focused contract tests passing after correction of an invalid
synthetic semantic fixture. A checksum-guard removal control failed at the
expected tampered-source arm. Source was restored to its recorded digest;
all four restored tests passed. The complete contributor check subsequently
passed after main reconciliation and observer integration, including these tests,
kernel mounts, scripts and the ledger.
The [fresh isolated live arm](benchmarks/icloud-numbers-parent-live-2026-10-02.json)
received explicit scoped authorization and ran once on 3 October. Its new folder
and Numbers import completed; this observer independently verified A's exact
identity, revision and complete semantic content. Replacement then refused at
durable enqueue. Read-only inspection found no replacement operation, and a
second independent read proved A unchanged. No replay, restore, deletion or
Apple GUI open followed. This is partial live Numbers evidence, not replacement
or Apple application fidelity. Installed schema14 state is unchanged and schema19
stays isolated; the later synthetic correction is described below.

### Native working schema19 remains isolated

Schema19 implements native working, successor and backup-first state beyond the
earlier schema17 explicit archive workflow. Synthetic journal and kernel tests
do not establish Apple-backed save or deployment acceptance. Every writable
journal opened by this writer advances, including ordinary-only accounts;
validation remains isolated until recovery and application acceptance. See the
[deployment and recovery policy](development.md#ordinary-metadata-publication-journal-schema20-held-prerelease-policy).
Pre-upgrade exports preserve selected local bytes, not provider rollback, sharing
or revision history. Missing/corrupt bindings must refuse upload while compatible
read-only recovery still exports exact retained bytes without network or parsing.


### Canonical archive in-place writes: synthetic kernel boundary

The development path now routes the exact canonical native archive through a
validated local working stream. New writable handles use that stream; already
opened original readers retain an immutable staged session. Five admission tests
and three actual-kernel synthetic cases cover first write, pathname truncate,
O_TRUNC, typed fsync, held-original reads after a simulated handoff, and RO refusal.
Counterfactual controls fail without the reader drain, snapshot selection or
truncate forwarding. See [the registered evidence](benchmarks/icloud-native-path-writing-2026-10-01.md).

This does not enable arbitrary edits inside package folders. Subsequent
[atomic replacement and working-copy retirement tests](benchmarks/icloud-native-atomic-retirement-2026-10-01.md)
and [backup-first save tests](benchmarks/icloud-native-backup-gap-2026-10-01.md)
passed synthetic journal and actual-kernel checks, including meaningful failing
controls, held descriptors and pending-save reconstruction after remount.
These fixtures use synthetic receipts and do not verify Apple transport.
Apple-backed mounted saves and installed lifecycle acceptance remain open.
No regular account or installed service was upgraded for these tests.


### Synthetic integration checks on 3 October 2026

The [registered development record](benchmarks/icloud-integration-development-2026-10-03.json)
keeps all six full-iCloud requirements open. It records two narrow CI accounting
corrections for `allocation_crash_child` and `interruption_child`: each is an
ignored subprocess entry point driven by its explicitly named, selected parent
test. The actual coverage audit refused those entries before correction and
passed afterward. The complete contributor command now runs the audit too;
ordinary unselected tests gain no broad exemption. The earlier Linux CI failure
at `621a576` is retained in that record; the other six jobs passed. Targeted local
audit success does not turn that historical CI run green.

The concrete GTK archive chooser filter previously excluded mixed-case Pages,
Numbers, Keynote and ZIP suffixes that the form and daemon already accepted.
One actual GTK regression failed with four excluded mixed-case filenames.
After the suffix-filter correction, the same test accepted twelve supported
case variants and refused nine unrelated or misleading names. CI selects this
exact ignored library test under a display with one test thread. This validates
the chooser filter, not the live portal-to-import/publication workflow or Apple
application fidelity.

Nine actual FUSE native archive tests passed across twelve scenario arms and
24 synthetic mount lifetimes, including six new Numbers/Keynote arms. They
exercise matching archive roots, in-place saves, atomic replacement and
backup-first rollback/promotion, held readers/descriptors, wrong-format refusal,
read-only protection and retained pending saves after remount. These are added
coverage of the existing generic implementation, not proof of a new production
fix. Typed receipts and provider behavior remain synthetic; the archives are
not Apple Numbers/Keynote application documents.

These targeted checks and the complete `scripts/check.sh` passed for the new
3 October source. All 55 writable kernel tests executed within the unchanged
150-second CI group deadline; exact timing is retained in the development record.
The GTK chooser regression ran separately; other desktop window scenarios were
not run locally. No cloud request, live replacement,
installed daemon restart or state migration was performed. The prepared Numbers
live arm subsequently ran under separate scoped authorization, as recorded above;
schema19 validation remains isolated from installed schema14 state.


### Actual Canary state compatibility, 3 October 2026

The [controlled compatibility record](benchmarks/icloud-canary-state-compatibility-2026-10-03.json)
uses journal schema14 and metadata schema7 genuinely produced by the exact Canary
commit `a9e2429`, rather than changing current database headers to imitate older
state. Current code exported two pending snapshots and one dirty successor
without modifying those databases, then migrated to schema19/8 while retaining
operation identities, bytes, generations, visible metadata and the interrupted
refresh continuation. The exact Canary code subsequently refused both newer
schemas; independent complete file and logical snapshots remained identical,
including SQLite sidecars. Current read-only recovery exported the same three
byte streams again, and completing the saved refresh published its retained
staged node with the completed checkpoint.

The record includes source and binary digests, the isolated harness sources,
phase receipts and unchanged-state comparisons. A deliberately incorrect producer
manifest digest was refused before exports or state changes. The first current
helper build had a SQL count deserialization error, corrected before fixture
execution; that harness error is retained in the same artifact.

This is one controlled synthetic ordinary-file compatibility trial. It does not
prove installed package rollback, credentials, real-provider pending work or
native Apple application fidelity. It does not remove the schema19 installation
hold or close full-iCloud lifecycle requirement 488.


### One native kernel save through the HTTPS worker

The [registered kernel-to-transport trial](benchmarks/icloud-native-fuse-https-2026-10-03.json)
connects a genuine FUSE-sealed Pages archive to the actual native replacement
coordinator and transfer worker using the existing loopback HTTPS fixture.
Exactly one selected test passed. It checked the queued native representation,
original/current semantic proofs, typed original/current/Trash identities,
uploaded bytes, one call per mutation phase, confirmed metadata publication,
an unread original descriptor, and remount with the confirmed bytes and no
replay. Both mounted sessions joined successfully; no fixture FUSE mount
remained under `/var/tmp` afterward. The complete `scripts/check.sh` for this additional source also passed and
explicitly executed the new test again. Other desktop window scenarios were not
run locally.

This adds synthetic integration coverage without manufacturing a completion
receipt. Initial hydration and observed metadata are fixture providers; one
Pages ZIP generation is not an Apple application document or save. The normal
account router, lost-response recovery and live Apple behavior remain separate
requirements; multiple pending generations are covered by the later synthetic
trial below. No provider account or installed
service was touched, and no full-iCloud release gate closed.


### Native package storage refusal after Trash

The [registered storage-refusal regression](benchmarks/icloud-package-readback-storage-2026-10-03.json)
reproduced a signed PACKAGE HTTP507 losing its typed storage error. One exact
unit test failed at the missing storage type. A separate actual HTTPS worker
trial confirmed one refusal after the original reached Trash and failed because
the worker returned `VerifyRequired` instead of `Failed`. Signed401/403 controls
already remained uncertain before correction.

Package staging now reuses the existing signed-content error classifier before
its unchanged representation, range, encoding, budget, cancellation and identity
checks. Both original regressions passed after correction. All eight package
controls and three native coordinator controls passed, including existing
lost-reply and cancellation scenarios. In the post-Trash trial, the failed row
retained its exact identity reservation, hidden recovery object, encrypted
`move_old` checkpoint and independently exportable source. The one-shot fault
had cleared, yet an idle worker sent no requests. Explicit retry completed
without repeating allocation, upload, registration or Trash; only the remaining
rename ran. Error diagnostics exposed neither response bodies nor signed URLs.

These are bounded synthetic protocol checks. They do not establish live Apple
quota behavior, account reauthentication, dirty-successor reliability or installed
acceptance. The complete `scripts/check.sh` passed for this correction, including the
regressions and mounted scenarios. Other desktop window scenarios were not run
locally. All six full-iCloud gates remain open.

### Native import dialog on the declared desktop library floor

The [registered desktop crash comparison](benchmarks/icloud-native-window-crash-2026-10-03.json)
reproduced the CI segmentation fault with the native import scenario alone on
Ubuntu 24.04, GTK 4.14.5 and libadwaita 1.5.0. A debugger traced the failure to
setting the AlertDialog's content width before presentation. GNOME's
[libadwaita release notes](https://raw.githubusercontent.com/GNOME/libadwaita/main/NEWS)
identify a fix for that sequence in 1.7.alpha; the same unchanged scenarios
passed on this development machine's newer libraries.

Cirrove now presents the dialog before applying its width. The exact scenario
then passed on the older libraries, followed by all 21 synthetic desktop window
scenarios within the existing 30-second deadline. The renderer, scenario
selection and library floor were unchanged, and EGL warnings remained in the
passing runs. This validates the compatibility correction, not live Apple
imports or installed-account transitions. The complete `scripts/check.sh`
passed for this correction, including all 55 writable kernel tests; its result
is recorded in the same artifact. All six full-iCloud release gates remain open.

### Folder revision changes after an owned Numbers import

The [single authorized Numbers trial](benchmarks/icloud-numbers-parent-live-2026-10-02.json)
confirmed its new folder's stable local-to-provider binding, public import and
independent current-content proof, then refused replacement admission. iCloud
had advanced only the parent folder's ETag after the child import; the completed
local binding retained the create revision. The journal's whole-ancestor Node
comparison rejected this change. No replacement was queued; independent
post-refusal readback confirmed the original's identity, revision and content.

The [registered regression](benchmarks/icloud-native-parent-revision-2026-10-03.json)
failed at durable enqueue on unchanged production code. The correction permits
only ETag drift for a linked, remote-owned, following ancestor with no working
file or latest operation. Other ancestor fields and the selected document's
full identity/revision remain exact, along with resource, frontier and publication
fences. No retained ancestor state is rewritten. All eleven replacement controls
passed, including unfinished-parent refusal. This corrects synthetic admission;
the live replacement has not been replayed or confirmed.

### Three pending native kernel saves through HTTPS

The [registered chain trial](benchmarks/icloud-native-pending-https-chain-2026-10-03.json)
queues A, B and C through actual FUSE atomic saves before starting the worker.
Each successor receives its completed predecessor's exact provider identity,
revision and semantic proof. Actual HTTPS worker receipts complete all three;
held unread O/A/B descriptors retain their bytes, C remains visible, and remount
does not replay completed work. The original and chain tests both execute in
the local check and CI through their shared module selector.

Removing only semantic rebasing made the new test fail after A completed,
before any B HTTPS request. The unchanged later confirmation guard remained;
this control demonstrates test sensitivity, not an unsafe provider upload.
The assignment was restored and both tests passed. Metadata/hydration are
synthetic and each adapter is prepared after an explicit resolver call; default
account routing, Apple application saves and real-provider reliability remain open.

### Complete desktop coverage within two unchanged time budgets

After the crash correction, shared-runner CI hit the 30-second timeout for the
combined window group. The [registered group comparison](benchmarks/icloud-window-ci-groups-2026-10-03.json)
separates the expensive read-only receipt scenario from the other twenty, with
the same 30-second limit for each. The custom harness now honors `--skip` and
`--exact`. A coverage audit verifies all 21 declared scenarios are selected;
removing the dedicated scenario or selecting only a prefix correctly fails it.

The supported Ubuntu 24.04 / GTK 4.14.5 / libadwaita 1.5.0 environment ran one
and twenty scenarios successfully in separate groups. No scenario, warning,
renderer or library floor was suppressed or changed. One local run per group
does not establish repeated shared-runner timing reliability. The final source
`658d8d6` subsequently passed all seven jobs in
[shared-runner CI](https://github.com/Dandiccf/cirrove/actions/runs/37119742211),
including the desktop groups and mounted writable scenarios; this remains
separate from live Apple and installed acceptance.

The complete `scripts/check.sh` passed for these final corrections, including
formatting, all clippy/probe checks, workspace and script tests, two native
FUSE-to-HTTPS tests, all 55 writable kernel cases, coverage audit and ledger.
The module audit also rejects an unrelated ignored canary that the earlier
empty-suffix comparison incorrectly counted as covered. Final command, source
hashes and results are retained in the three registered artifacts above.
No full-iCloud gate closed and the installed daemon remains unchanged.

### Unconfirmed predecessor with a newer save and unfinished local edit

The [registered coupled recovery trial](benchmarks/icloud-native-uncertain-chain-2026-10-03.json)
promotes native A and B through actual FUSE, then leaves an incomplete C edit
dirty after its rejected fsync. The synthetic HTTPS server processes A's Trash
request but holds its response; cancelling the worker retains an encrypted
`move_old` checkpoint and no confirmed replacement receipt. B remains unclaimed
and unchanged, with zero requests to its server.

After unmount and journal/vault reopen, the read-only recovery owner exports exact
sealed A/B bytes and C's unfinished bytes without changing recovery inventories.
An explicit A continuation inspects the completed Trash step and finishes without
another allocation, upload, registration or Trash request. B then uses A's exact
confirmed identity, revision and v2 semantic proof. Both acknowledgements,
publication and a final remount preserve C's working identity, generation and
dirty bytes; an idle worker does not replay either completed operation.

The scoped test passed. Omitting only the existing predecessor semantic assignment
made it fail before B's HTTPS request; the production source was restored byte for
byte and all three FUSE-to-HTTPS tests passed together. This demonstrates coverage
sensitivity, not a newly discovered product defect. Metadata and package content
remain synthetic, and standalone adapters use the internal resolver seam. The
trial does not establish Apple application fidelity, default account routing,
live-provider reliability or installed acceptance; all six release gates stay open.

A subsequent documentation-only CI publication
[exposed a folder create/remove race](https://github.com/Dandiccf/cirrove/actions/runs/37123619790):
the writable group passed 54 cases and failed the created-then-removed directory
case at its no-failed-mutations check. The provider-absence assertion was not
reached. This predates the new coupled fixture; the matching controlled correction is recorded below. The
prior green run is retained as one observation, not proof of repeatable success.
The complete local `scripts/check.sh` passed with the new fixture, all three
native FUSE-to-HTTPS cases and all 55 writable scenarios.

The [controlled folder regression](benchmarks/ordinary-folder-handoff-race-2026-10-03.json)
reproduces a matching interleaving: a local view captured before handoff can
restore the creation ETag over the independently observed settled folder node.
The actual mutation worker then receives conditional refusal. Two intermediate
corrections also had demonstrated limits: retaining only the settled journal node
lost a later observed revision, and comparison with the historical pathname
refused a genuinely renamed folder.

The final correction obtains the ordinary folder's current scoped metadata from
the local index outside journal and projection locks, without a provider fallback.
It compares the selected identity, canonical parent, pathname and shape with that
observation, allowing only revision/time metadata to lag. Before adoption,
removal and relocation recheck the complete bound journal snapshot and
cancellation. A fresh observed external pathname can therefore be used, while an
old or altered pathname is refused. Conditional provider mutation still protects
changes occurring after the local observation. File and native-package
materialization retain their prior behavior.

The original stale-view test failed against unchanged production; the newer
revision and external-rename tests failed against their respective intermediate
corrections. All three exact cases now pass. All 12 handoff tests pass, including seven altered-selection removal arms,
missing/wrong-kind cache refusal without provider fallback, a bound-snapshot
drift refusal and an external move through distinct local/provider parent IDs.
The cancellation and snapshot-drift controls exercise source preparation/recheck,
not an instrumented wallclock concurrent-removal race. The complete contributor
check passed for final source, including all three native FUSE/HTTPS and 55
writable kernel scenarios with the original create/remove regression.
[CI for the exact corrected commit 176bd09](https://github.com/Dandiccf/cirrove/actions/runs/37129401664)
then passed all seven jobs. Its Linux log confirms actual execution of the
1+20 desktop window scenarios, all three native FUSE/HTTPS cases and all 55
writable kernel cases. This validates that source snapshot; newer CLI/API work
requires its own CI. These tests do not establish the uninstrumented CI failure's
unique cause, current remote freshness or live-provider reliability.


### Saved native import discovery after restart

The development service advertises `list-native-imports` version 1 and exposes
`list_native_imports` with a required selected account UUID, label, bounded limit
(1–100), and optional sequence cursor. It lists explicit native archive imports
from the retained journal, including when ephemeral transfer jobs have disappeared
or the connection is read-only. Empty legacy pages can still have a next cursor.
Discovery does not capture a source, contact Apple, submit an upload or retry work.

Each result contains the operation UUID, sequence, state, destination parent/name,
and historical completion identity. A recorded completion receipt does not prove
that the document is still present or published now. The existing explicit
`watch-native-import` action checks the exact saved operation on an active writable
connection; listing does not enable writes or mount an account. The development
desktop now offers [saved-import discovery](desktop.md#find-an-import-again-after-reconnecting),
with 25-record pages and explicit checks of existing operations. Enabled
read-only/unmounted connections can discover history after the service confirms
their exact identity; disabled accounts and unavailable, incompatible or
identity-unconfirmed services cannot. Checking still requires an active writable
mount and the exact supported capability.

The [registered discovery checks](benchmarks/icloud-saved-native-import-discovery-2026-10-03.json)
cover exact-account scope, malformed ownership/receipts, bounded indexed and legacy
paging, read-only journal preservation, a real Engine restart with no jobs, and
account withdrawal during a journal wait. They use synthetic retained operations
and do not establish Apple application acceptance or complete recovery UX.

The [desktop discovery record](benchmarks/icloud-desktop-saved-native-imports-2026-10-03.json)
registers bounded page validation, selected account/collection checks, retained
typed operation identity and a synthetic window reconnect/paging/watch scenario.
An accepted observer is published before a delayed status response, so a second
check cannot attach another observer for that running operation. Removing only
that publication makes the exact accepted-operation assertion fail while the
fake service is reachable; restoring it passes. Earlier intact fixture failures
were caused by a queued toast outlasting the deliberately held status request;
the fixture now waits for the earlier toast before arming that hold, with the
same network and scenario limits. This evidence does not establish a live import
submission, provider contents, Apple fidelity or installed lifecycle acceptance.

The restored source passed all 22 host window scenarios in disjoint groups
1+1+20, each retaining its 30-second limit, plus the chooser filter and all five
native-import model tests. The complete combined `scripts/check.sh` also passed,
including all three native FUSE/HTTPS scenarios and 55 writable kernel scenarios.
The subsequent [CI run for source 9527ff0](https://github.com/Dandiccf/cirrove/actions/runs/37140142769)
passed all seven jobs. Linux checked out PR merge
`ef4ee4fc2c86c4d497b80885d54428a2d3895fd3`, whose tree is identical to
`9527ff0dcb31cae3249909cffe2cf47d918559eb`. Its complete log explicitly records
the chooser, all 22 windows in groups 1+1+20, all three native FUSE/HTTPS cases
and all 55 writable cases. The window groups retained their 30-second limits
on Ubuntu GTK 4.14.5/libadwaita 1.5.0; their command-to-summary times were
10.93, 7.35 and 22.61 seconds. The same run executed all five saved-history
service tests, five CLI-binding tests and four new desktop library/model tests.
These are synthetic source checks; live submission, Apple fidelity and installed
acceptance remain open. The subsequent installer protection is outside
this CI snapshot.

### Settings-lock ownership during reauthentication

[CI for CLI account binding at 60ea0f9](https://github.com/Dandiccf/cirrove/actions/runs/37132139130)
finished with six successful jobs and a failed Linux job: an existing iCloud
reauthentication test could not reacquire the settings lock during desired-state
restoration. The workspace step stopped before the new CLI integration target
and later window/kernel groups. This is not evidence of a CLI-binding assertion
failure or proof of the unique remote cause.

The [controlled lock record](benchmarks/config-lock-inherited-description-2026-10-03.json)
reproduces one matching mechanism: closing the parent's file alone retains a
`flock` while a child holds the inherited open file description. The settings
lock now has a non-clonable guard that explicitly unlocks only in its acquiring
process, matching the journal owner's existing policy. Failed contenders never
construct an unlocking guard. The same regression fails before the correction
and passes afterward, preserving exclusion and restoring both originally enabled
and disabled account states while the child remains alive. The original
reauthentication test also passes locally. This safe post-exec descriptor fixture
does not execute a forked Rust guard destructor or establish live/installed
reauthentication reliability. No retry or timeout relaxation was added.

The combined full check subsequently executed both settings tests successfully
alongside the desktop/discovery increment; its exact source and disk-temporary
directory pins are recorded in the same artifact. CI run 37140142769 then
explicitly passed both tests on the tree of source 9527ff0, together with all
seven jobs. This leaves the older 60ea0f9 failure and its cause uncertainty
recorded; a passing snapshot does not establish sustained or installed
reauthentication reliability.

### Developer installation refusal under the schema hold

The newer [installation preflight record](benchmarks/icloud-installation-preflight-2026-10-03.json)
checks the actual developer installer against synthetic typed service routing
and fake mutation boundaries. The unchanged installer failed 31 refusal arms;
the correction passed all 39 tests, with eight targeted omission controls
showing sensitivity before restoration. A separate invocation of only the
read-only helper on this host refused retained local state through the real
D-Bus/process reader. It performed no installation, restart, state-content read,
SQLite open or cloud action.

This subsequent protection is not covered by the completed 9527ff0 CI run.
Its complete contributor check passed, including the newly wired 39 installer
tests, all three native FUSE/HTTPS cases and all 55 writable kernel cases.
[CI for installer source 378a96d](https://github.com/Dandiccf/cirrove/actions/runs/37143554956)
then passed all seven jobs. Its tested PR merge has the identical source tree,
and Linux explicitly executed all 39 installer tests, all 22 windows on the
GTK 4.14/libadwaita 1.5 floor, three native FUSE/HTTPS cases and 55 writable
cases. This snapshot does not cover the subsequent format-recovery correction. It guards declared windows and known affected state routes; it does
not reserve a concurrent measurement lease, discover unannotated historical
windows, attest arbitrary artifacts or prove installed readiness. The
schema19/metadata8 installation hold and all six full-iCloud gates remain open.

### Format-bound native Stage recovery

The [registered format recovery check](benchmarks/icloud-native-stage-format-recovery-2026-10-03.json)
reproduces a concrete Numbers/Keynote recovery refusal. The coordinator allocates
its Stage with the captured original format, but final abandonment validation
still required a `.pages` name. The unchanged implementation rejects both formats
only after complete read-only metadata observation and independent original
semantic-v2 verification; original and mixed-case Pages controls pass.

The correction derives the canonical suffix using the coordinator's existing
rule. Complete name and operation equality, parent, identity, PACKAGE, revision
and checkpoint fences remain strict. Stage abandonment retains the archive and
checkpoint and resolves local ownership only after fresh observation; it does
not delete the Stage, upload again, Trash the original or enqueue a replacement.
The corrected source passes all four selected tests, including five new format
arms and 15 exact-name tamper refusals. The complete contributor check also
passed, including the new tests, existing provider/service/journal abandonment
coverage, all three native FUSE/HTTPS scenarios, all 55 writable kernel cases
and 39 installer tests. Exact source `0e48e13` CI37145716828 executed the new
format tests in both workspace and feature runs, existing abandonment recovery,
chooser, all 22 windows, three native FUSE/HTTPS, 55 writable and 39 installer
cases successfully. Six other jobs succeeded, but Linux was cancelled during
final documentation generation after its global 30-minute budget expired. The
GitHub check annotation states that limit explicitly. This is not a seven-job
green conclusion; the tested PR merge has the exact same source tree. All six
live/application/installed release gates remain open.

### Native kernel save through the normal account router

The [registered local HTTPS acceptance](benchmarks/icloud-native-account-router-https-2026-10-03.json)
adds a fourth native FUSE/HTTPS scenario. A real kernel Pages archive save now
also uses a valid root iCloud account, `WriteContext::open`, its shared journal
and the normal `ICloudWriteProvider` constructor and native factory. An unsafe
intermediate staging directory refuses before any provider request. After a
lost registration reply, the worker retains its encrypted checkpoint and local
payload. Hiding the current parent in the scoped metadata index refuses a fresh
begin; an explicit retry with a newly constructed router restores the captured
parent and completes with one allocation, body upload, registration, Trash and
rename. Exact old/new/Trash receipts, semantic-v2 completion, publication, an
old open reader and remount bytes remain checked.

A controlled omission of captured-parent restore makes that same test finish
in `Conflict` rather than `Uploaded`, with no repeated registration or handoff.
The exact factory source was restored and all four native FUSE/HTTPS cases
passed. This is sensitivity of the new coupled coverage, not a new product
fix. The first setup run failed because the synthetic full-root change feed
omitted its parent folder; that failure and fixture correction are retained.

The test transport is bound only after normal factory/restore validation. It
uses a fresh credential-free session and a fixed loopback HTTPS fixture origin.
Metadata, archive resolution, hydration and checkpoint wrapping keys remain
synthetic; sealed-session decryption/renewal, real Apple behavior, Numbers,
Keynote, DATA application saves and installed deployment are not established.
The complete `scripts/check.sh` passed, including all four native FUSE/HTTPS,
55 writable kernel and 39 installer cases. Desktop display scenarios were not
repeated locally; source CI remains pending. No full-iCloud release criterion
is closed.


### Complete CI job budget

[CI37145716828](https://github.com/Dandiccf/cirrove/actions/runs/37145716828)
passed all Linux steps through icon/translation checks, then exhausted the
aggregate 30-minute job budget during `cargo doc`. The terminal cancellation,
explicit GitHub annotation and completed scopes are retained in the
[budget record](benchmarks/icloud-ci-completion-budget-2026-10-03.json). Linux's
aggregate budget is now 40 minutes; every test-group timeout, scenario and
command remains unchanged. Subsequent source `b5136fd` completed all seven jobs
in CI `37148483127`, with matching head/merge trees and the full test scopes
recorded in the account-router artifact. That single candidate run does not
relabel the earlier cancellation or prove repeatable timing.
The final complete `scripts/check.sh` passed after this workflow change, again
executing all four native FUSE/HTTPS, 55 writable and 39 installer cases.
Executable source, including the corrected workflow, was unchanged throughout
that check. The subsequent remote run completed; timing repeatability remains
open and its evidence excludes the later staging correction below.

### Shared native write staging survives cancelled waiters

The [native WRITE staging record](benchmarks/icloud-native-write-staging-budget-2026-10-03.json)
addresses an additional resource boundary beyond the read-artifact cache and journal
quota. Two transfer pumps bounded active futures, but an aborted waiter could leave
an unreserved upload copy or verification archive in a running blocking task.
Actual unbounded host growth was not measured.

One four-file budget belongs to the account's `Engine` and is retained by its
`WriteContext`. Fresh and restored package adapters, native removal adapters,
replacement-admission Original verification and every native handoff session
share it. Explicit restore adapters share their source pool and accept the runtime
budget through their builder. Each anonymous upload or Original/Current/Trash
verification file has a 64 MiB limit; positioned I/O uses at most 64 KiB per chunk.
The file, HTTP body and detached blocking owners retain the same reservation.
Package allocation holds a reservation before dispatch; full admission refuses
without waiting or deleting retained journal bytes.

Seven local omission controls separately remove file-owner retention, package
factory or handoff-session binding, allocation admission, native removal factory
binding, restore-source binding and replacement-admission binding. Each fails its
corresponding regression; exact source restoration is recorded in the artifact.
Additional TLS pressure arms cover native removal Original/Trash proof and restore
preflight/current proof, including one-slot recovery without replay. The normal
account-router fixture exercises exhausted fresh/restore/removal/admission
verification and subsequent lost-registration recovery without repeated mutations.
These fixtures do not establish live Apple quota behavior, native application
fidelity, installed acceptance or a global account/host storage ceiling. Reader
cache, journal, local source capture, filesystem overhead and independently opened
Engines remain separate. All six full-iCloud release criteria remain open.

The previous published source `b5136fd` subsequently passed all seven jobs in
CI run `37148483127`, including four native FUSE/HTTPS, 55 writable kernel,
39 installer and 22 supported-floor window tests. Its tested merge tree equals
the branch tree. That run does not cover this newer staging correction.

The complete `scripts/check.sh` passed on the final corrected source, including
247 default iCloud tests, 362 `write-probe` iCloud tests, all four native
FUSE/HTTPS scenarios, 55 writable kernel cases and 39 installer tests. Executable
source was unchanged throughout. Its first attempt exposed a feature-only owned
import caller still supplying a raw upload file; that caller now uses the same
guarded transport. All direct probe fixtures explicitly declare private staging;
the corrected 18-test owned-import group passes and the full command was rerun
from the beginning. Initial authoring failures remain in the evidence artifact.
Desktop display scenarios were not repeated locally; this source's remote CI is
pending. No full-iCloud release criterion is closed.

The preregistered [fresh Numbers confirmation](benchmarks/icloud-numbers-confirmation-2026-10-03.json)
is now prepared from validated source `04dc9e1`, replacing its earlier `658d8d6`
binary pins before any cloud phase. Three binaries were built in a clean detached
checkout with a separate Cargo target. Its import now explicitly binds the frozen
account UUID as well as its label; the finite controller scope and no-replay
guards are unchanged. Independent review and versioned local-only records pass,
and the initial controller refuses the refreshed plan hash. The owned A/B
archives, account/settings/session hashes and fresh target remain unchanged.
Authorization remains absent: no daemon, provider request, mutation or Apple
Numbers confirmation was performed at that preparation stage. The subsequent
4 October live result below is separate; preparation alone closes no release gate.
The complete `scripts/check.sh` passed again before publishing this refreshed
preparation, including 247 default/362 feature iCloud tests, four native
FUSE/HTTPS, 55 writable and 39 installer cases. Production code remained exact
`04dc9e1`; only the preparation/integration documentation changed.

The staging correction's exact commit `04dc9e1` subsequently passed all seven
jobs in [CI37153730851](https://github.com/Dandiccf/cirrove/actions/runs/37153730851).
Its actual PR merge tree equals the branch tree. The same staging artifact now
records 247 default/362 feature iCloud tests, 13 staging controls in both builds,
four native FUSE/HTTPS, 55 writable, 39 installer, 22 supported-floor windows and
one chooser case. This source validation includes the staging correction but
excludes later documentation changes; it supplies no live Apple, installed or
timing-repeatability acceptance. All six full-iCloud release criteria stay open.


### Fresh owned Numbers confirmation on 4 October 2026

The [preregistered Numbers run](benchmarks/icloud-numbers-confirmation-2026-10-03.json)
received new explicit authorization and completed once from validated source
`04dc9e1`: one new UUID-owned folder, one public CLI PACKAGE import of A and
one replacement with B. Independent receipt-bound readers verified the complete
semantic-v2 content of current B and original A in recoverable Trash, with exact
account, operation, item and revision bindings. The peer audit confirmed one
Applied folder, two Uploaded operations, two completed metadata publications,
zero unfinished queue rows and zero failed upload attempts. Both isolated
daemons were stopped by their recorded PIDs and their mount was released.
No restore, deletion, prior-run replay or repeated mutation dispatch followed.

Before any mutation, the private controller's status reader exposed an EOF
framing error: it required a newline although the daemon closes after one JSON
reply. The original helper failed against the same synthetic JSON/EOF fixture
that the corrected helper passed. An independent review confirmed that socket
ownership, timeout, size, source, authorization and no-replay guards remained
intact. This was a validation-controller correction, not a daemon change.

In an already authenticated Chrome session, Apple Numbers opened the unique
new run title and rendered B's values 7, 3 and 10 with `SUM(A2:B2)`. No document
edits were submitted. This UI observation binds the unique title and expected
content; it does not establish a direct URL-to-CloudDocs item-ID mapping. A
conversation screenshot was viewed, but a filesystem screenshot/export artifact
was not recorded because Chrome content export was unavailable. The independent
content verifier continues to report GUI fidelity as unverified.

This successful single PACKAGE CLI workflow is partial evidence for requirements
486 and 487. Ordinary and atomic editor saves, actual DATA representations,
Pages/Keynote application acceptance, reopen/remount/export fidelity, installed
transitions and broader reliability remain open. All six full-iCloud requirements
remain unchecked; installed schema14 state was not deployed or migrated.

### Preparing a Numbers save through the normal mount on 4 October 2026

The [next preregistered owned Numbers arm](benchmarks/icloud-numbers-fuse-save-2026-10-04.json)
targets one canonical archive save through a normal account mount, held-original
reads, recoverable Trash and a read-only remount with no additional operation.
At registration it had not run against iCloud and needed new authorization; the
previous CLI trial's authorization was consumed. The subsequent standing grant,
stopped first arm and successful fresh arm are recorded below.

Local preparation preserves the Apple-created A/B documents. Only B's ZIP root
is renamed to the fresh document's canonical archive name. The production
semantic-v2 scanner verified all three pinned sources and confirmed that this
rewrite preserves B's content identity. Distinct offline source/capture modes
and a read-only receipt observer bind the run, source, native working association,
typed current/Trash receipts and completed publication. They keep the earlier
CLI receipt observer's source scope unchanged.

Eight new synthetic tests and four unchanged CLI-observer tests passed. Real
local journal fixtures cover import, native hydration/sealing, acknowledgement,
publication and active or retired working streams. A completed CLI replacement
without a native working association is refused by the FUSE observer. Removing
only the registered working-UUID guard caused its exact test to fail; restoring
the source made all eight pass. Fixture-authoring corrections are recorded
separately from that controlled failure. The private writer also passed eleven
offline tests and a separate 30-minute-window omission control. Sixteen private
controller contract tests and three local system-Python/process compatibility
checks passed. Two private one-line omission controls also demonstrated that
the abort checks detect a blocked writer and an orphaned late scanner; their
unchanged controls passed. These are proposal safety checks with simulated
processes. The complete `scripts/check.sh` also passed after a Clippy
correction. Preserved authoring failures and corrections remain in the artifact.
The clean `ac1ee64` runtime and private controller are now pinned and reviewed;
local pin checks pass, and starting without fresh authorization refuses before
dispatch. Its [CI run](https://github.com/Dandiccf/cirrove/actions/runs/37189036734)
passed all seven jobs, including the eight new source-exact FUSE receipt tests.
This CI covers `ac1ee64`, separately from the later read-only corrections. These
are local validation results, not a real FUSE save, editor acceptance or a closed
release requirement.

### Owned Numbers canonical FUSE save completed on 4 October 2026

The user granted ongoing writable iCloud test authorization. Trials continue to
use fresh, personally owned fixtures, finite registrations and no automatic
mutation replay after ambiguity.

The first arm stopped after one folder dispatch when its private controller
refused an asynchronous confirmation frontier. Later read-only inspection found
the folder Applied; no Numbers import or save had started. The exact initial
frontier was not captured, so its unique cause remains unproven. A matching
Pending/Applying timing defect was reproduced against the original controller.
The corrected private copy observes the same bound operation until Applied
without another dispatch; failed, foreign, expired or uncertain states still
refuse. Eight correction controls passed after the original failed, and 38
adapted controller/writer/process controls passed with fixture-authoring failures
retained separately. This changes the private trial controller, not product code.

The [fresh registered arm](benchmarks/icloud-numbers-fuse-save-confirmation-2026-10-04.json)
completed one folder, one public CLI Numbers A import and one B canonical
archive-copy save through the normal account FUSE mount. Its actual first folder
poll returned confirmation-pending; the same operation later reached Applied.
The writer, public watch and read-only receipt observer verified completed
publication, held-original A, current B and original A in recoverable Trash,
including exact account/collection/item/revision and native working ownership.
Clean fsync added no operation. A genuine read-only remount returned B with an
unchanged journal frontier and no replay. Independent review checked the saved
proofs and SQLite receipts. All 18 recorded processes are gone, the mount is
absent, and the isolated stopped settings are restored to their original RW
bytes. The installed daemon and all 19 packaged artifacts remain unchanged.

In a separate [registered Apple-open observation](benchmarks/icloud-numbers-fuse-apple-open-2026-10-04.json),
Numbers opened the unique new title and rendered 7, 3 and 10, with SUM(A2:B2)
visible after selecting C2. No edits were submitted. The visible application URL
uses an opaque identifier; direct mapping to the typed CloudDocs item ID remains
unproven, and no exported screenshot artifact is claimed.

This is one bounded canonical archive-copy save, not ordinary or atomic saves
from a real editor, genuine DATA acceptance, Keynote fidelity, long-session
reliability or installed access-transition acceptance. All six full-iCloud
requirements remain unchecked; the schema19/metadata8 deployment hold remains.

### Genuine Keynote source and import observer on 4 October 2026

The [registered source preparation](benchmarks/icloud-keynote-source-2026-10-04.json)
created one personally owned Basic White presentation in Apple Keynote, with a
unique run title and synthetic slide title/subtitle. Its single native export
completed; the embedded export preview independently shows the registered text.
The import archive preserves all 54 native ZIP members and their metadata under
`Source.key/`. Its semantic-v2 identity has 58 entries, including implied
directories. Neither the extension nor the ZIP layout proves CloudDocs DATA
or PACKAGE classification.

A separate feature-gated, read-only
[Keynote import observer](benchmarks/icloud-keynote-import-observer-2026-10-04.json)
binds the source, exact account/collection/item, one completed import, its owned
parent and completed metadata publication. It requires exactly two queue rows,
one parent namespace and no working/native association, and fences source,
settings, journal and remote revisions around independent semantic readback.
Nine focused tests passed. Omitting either the sole-upload guard or total queue
guard made the exact registered control fail; restored source passed all nine.
The genuine Apple export also passed the production offline source scanner.
Independent asset and static source reviews found no blocker. The complete
`scripts/check.sh` passed from the beginning with unchanged executable source,
including all nine registered Keynote controls. The first full-check arm stopped
in the script smoke test because the root-selected temporary directory made its
Unix socket path exceed `SUN_LEN`; a shorter private ext4 path corrected only
the test setup. Both runs are preserved. Desktop display scenarios were not
repeated locally. Source CI and the subsequent live import remain separate
endpoints.

At this preparation endpoint, no Keynote import through Cirrove or imported-
document Apple open had run. These source and observer checks establish neither
editor saves, replacement,
Trash, DATA acceptance nor installed transitions. All six full-iCloud
requirements remain unchecked.

### Owned Keynote PACKAGE import completed on 4 October 2026

The [fresh registered Keynote arm](benchmarks/icloud-keynote-import-2026-10-04.json)
completed one owned UUID folder, one account-bound public CLI import, public watch
and archive access on the original mount. After exact-PID shutdown and unmount,
the distinct receipt observer independently verified current content, source-v2
identity, exact item/revision and completed metadata publication. The journal has
one Applied folder, one Uploaded import, two completed queue rows, one parent
namespace, no working files and no native-save association. Read-only peer review
recomputed the same 58-entry/54-file/516253-byte semantic identity from source,
mounted archive and independent download. Raw ZIP hashes differ; semantic content
is identical. Read-only inspection preserved DB/WAL/SHM bytes and inodes. All
11 recorded process identities are gone and the mount is absent. The installed
daemon and all 19 packaged artifacts remain unchanged.

Before this cloud run, independent controller review found a default-field
serialization defect: Rust omits `package=false` on ordinary folders, while the
private controller indexed it directly. The actual serialized fixture first
failed with `KeyError`, then passed all nine controls after narrow absent-false
normalization. The registered authorization-scope omission failed its exact
control, and restored source passed all nine. An unapplied omission's canonical
pass is retained as a fixture-authoring correction, not negative proof. These
are private trial changes; the product source was unchanged. Initial daemon
readiness briefly reported updating/offline, then became ready before any folder
dispatch. The first folder poll was confirmation-pending; the same operation
later reached Applied without resubmission.

In the [separate registered Apple-open observation](benchmarks/icloud-keynote-apple-open-2026-10-04.json),
Keynote opened the unique imported title and displayed one slide with the exact
registered title/subtitle. The template's unfilled Author and Date placeholder
was also visible. No edits were submitted and the two own browser tabs were
closed. A conversation screenshot was observed; no filesystem screenshot
artifact is claimed. Direct opaque UI-ID-to-CloudDocs mapping remains unproven.

This single PACKAGE import/read/application-open arm is partial evidence for
requirement487. It does not establish genuine DATA acceptance, ordinary/atomic
editor saves, Keynote replacement/Trash, export fidelity, timing repeatability
or installed transitions. All six full-iCloud requirements remain unchecked.

The observer source `8f101af` subsequently passed all seven jobs in
[CI37217220504](https://github.com/Dandiccf/cirrove/actions/runs/37217220504).
That source snapshot is separate from the later live-evidence documentation and
does not establish installed migration or timing repeatability.

A final complete `scripts/check.sh` after the README/live-evidence updates also
passed, with all 449 Rust/Cargo source files unchanged and all nine Keynote
controls executed. Desktop display scenarios were not repeated locally.

### Ordinary Numbers DATA validation on 4 October 2026

The separate [raw Numbers DATA arm](benchmarks/icloud-numbers-data-fuse-2026-10-04.json)
uses unchanged owned Apple exports. Its source scanner verifies raw hashes and
sizes offline; neither a filename extension nor an archive layout establishes
the provider's representation. The feature-only receipt observer requires an
independent actual-DATA/raw-A proof before B, then binds current raw B and the
exact original A in Trash. Ten service controls and four Trash-reader controls
passed in the complete contributor check at source `4f40cd2`; targeted guard
omissions failed their expected assertions before byte-exact restoration.

The first live arm stopped after one folder dispatch, before any Numbers file
create or save. The private controller incorrectly expected no operation-to-owner
association for a folder, although genuine namespace creation retains exactly
that association. The stopped journal contains one Applied folder mutation, one
completed queue entry, one folder owner association and no uploads or working
files. The exact first refusal snapshot was not retained, so its individual
fields are not reconstructed as evidence. Product source commits Applied state,
attempt cleanup, queue completion and namespace confirmation together; no partial
Applied/queue transaction is claimed.

The isolated daemon and all known phase owners stopped, the mount disappeared,
and the installed daemon identity and all 19 packaged artifacts remained
unchanged. The run remains unsuccessful with replay disabled. A
[fresh corrective arm](benchmarks/icloud-numbers-data-confirmation-2026-10-04.json)
will bind the exact folder association and exact upload associations, refusing
foreign or extra owners. It uses a new UUID, folder and journal; no prior cloud
operation is resubmitted. This ordinary raw copy/overwrite arm does not establish
native editor saves, immutable held-A snapshots or full iCloud acceptance.

The correction now passes 18 private controller controls, including seven
read-only SQL frontiers and eight malformed-association arms. Its regression
first failed against the previous controller. Independent source review also
found that the new DATA service observer omitted the complete association table
from its otherwise receipt-bound snapshot. A real journal corruption test first
showed all three malformed pair cases being accepted; the bounded exact pair
fence then passed all 11 DATA observer controls. It includes the folder pair and
all upload pairs in the before/after snapshot. The complete `scripts/check.sh`
passed with these 11 controls and all four DATA Trash-reader controls. These
are observer corrections; the fresh live DATA arm remains pending at this
source-validation endpoint, and all six full-iCloud gates remain open.

That fresh arm subsequently completed one folder and one ordinary FileBytes A
upload, including a hash-exact original-mount capture. After the exact daemon
shutdown, its local preflight refused before launching the independent provider
observer: normal metadata handoff retained `content_version=None`, whereas the
immutable upload receipt contained `content_version=Some(its ETag)`. Every other
Node field and the remote sequence remained identical. This follows the existing
create-receipt and directory-metadata projections, not a changed content revision.
The stopped journal retained one Applied mutation, one Uploaded record, two
completed queue entries, two exact owner associations and no working files.

The arm remains unsuccessful: no B was saved and no independent actual-DATA
representation was observed. All known owners stopped, the mount disappeared,
and the installed daemon and all 19 artifacts remained unchanged. A separate
[revision-confirmation arm](benchmarks/icloud-numbers-data-revision-confirmation-2026-10-04.json)
preregisters a narrowly bounded correction for this one-way ETag-alias omission
on fully retired clean objects. Active owners, unrelated tokens, all other Node
fields and hidden backups remain exact. The old cloud operation is not replayed.

The retired-token correction first reproduced both A/B refusals against the
unchanged observer, then passed all 12 service controls including eight strict
negative arms. Its private controller likewise first refused both genuine
read-only SQL frontiers, then passed all 20 controls, including 12 metadata and
lifecycle refusals. The complete `scripts/check.sh` passed with all 12 DATA
service controls and four DATA Trash-reader controls, with all 452 source hashes
verified. This source-validation endpoint supplies no new live DATA acceptance;
the separately registered fresh arm remains unexecuted at this endpoint.


### Owned Numbers DATA create and in-place save completed on 4 October 2026

The [fresh revision-confirmation arm](benchmarks/icloud-numbers-data-revision-confirmation-2026-10-04.json)
subsequently completed its three registered mutations: one fresh owned folder,
one ordinary FUSE create of raw Apple Numbers export A, and one in-place save
of raw export B. After stopping the first isolated daemon, an independent reader
confirmed actual CloudDocs DATA representation and the exact 138,881-byte A
SHA-256 before B was permitted. The save opened without truncation, checked the
actual descriptor identity and size, then truncated and wrote B once.

After stopping the second isolated daemon, independent read-only checks verified
current DATA B (138,943 bytes) and the typed original A in Trash, with exact
provider identities, revisions and raw SHA-256 values. Current identity changed,
the stable local owner stayed the same, and a clean fsync added no operation.
The final journal contained one Applied folder mutation, two Uploaded FileBytes
operations and three completed queue/namespace associations; there were no
native package publications or atomic replacements. No earlier arm was replayed.
All owned processes were gone and the mount absent. The installed daemon kept
its exact process identity; all 19 packaged files and the absent home shadow
were unchanged.

This one successful run establishes bounded ordinary DATA copy/overwrite for
this Numbers fixture. It does not exercise a real editor, atomic save, Apple
GUI/reopen or export fidelity, a read-only remount, uncertain-outcome recovery,
expired sessions, repeatability or installed transitions. The independent raw
proofs deliberately carry no package semantics or GUI claim. Requirements
485–490 all remain open, and the schema19/metadata8 deployment hold remains.

### Owned Numbers final-confirmation loss and recovery on 5 October 2026

The [registered final-confirmation arm](benchmarks/icloud-native-final-confirmation-2026-10-04.json)
completed one fresh owned folder, one public CLI Numbers PACKAGE import A and
one public CLI replacement B. Independent typed metadata and semantic-v2 checks
verified A before replacement. A feature-only guard then durably recorded the
actual final B/Trash-A receipt and exited with code86 before worker acknowledgement.
The CLI disconnected with exit1; that disconnect alone was not accepted as proof.

Before reopening the journal, read-only inspection found the same operation
Uploading, with its attempt, encrypted checkpoint and sealed B retained, and
without a completed receipt or publication. Recovery performed one terminal
inspection, attempted zero upload or namespace mutation callbacks, and published
the same operation through the normal metadata path. A separate read-only
postflight independently verified current B and original A in Trash, with their
exact identities, revisions and semantic-v2 content.

All 18 owned process records were rechecked as gone and the mount was absent.
The installed daemon retained its process identity; all 19 packaged files and
the absent home shadow were unchanged. The source passed complete local
`scripts/check.sh` and [all seven CI jobs at 5a89e04](https://github.com/Dandiccf/cirrove/actions/runs/37240005542).

This proves one controlled loss of a genuine final receipt and inspection-only
recovery for this Numbers PACKAGE fixture. It does not prove real session
expiration, editor fidelity, repeated reliability or installed transitions.
The three logical mutations were bounded; total HTTP mutations were not
instrumented. All six full-iCloud requirements and the deployment hold remain open.

### Owned Numbers browser edit and native export on 5 October 2026

A [fresh registered browser trial](benchmarks/icloud-numbers-browser-editor-2026-10-05.json)
completed one owned folder creation and one public Numbers PACKAGE import.
After stopping and unmounting the writable producer, read-only metadata and an
independent native reader verified the imported document. Apple Numbers then
displayed 7, 3 and 10 with `SUM(A2:B2)`. One browser edit changed A2 to 11;
after the Saving indicator disappeared, closing and reopening the document
preserved 11, 3 and 14 and the formula. One native Numbers export was retained
without modifying its bytes.

The actual export is a flat ZIP containing Index, Metadata and previews, with
no enclosing `.numbers` directory. The wrapped-source proof could not accept
that layout, so the trial stopped without repeating a cloud operation or
repackaging the export. Independent current-B and normal remount-B proofs were
not executed. All owned processes and the mount were gone; the journal frontier
and installed daemon, unit and 19 packaged files remained unchanged. The
supervisor reported settlement, but its launcher lost its terminal report when
it inspected an already-reaped process; no retained watcher exit code is claimed.

The [separate offline correction record](benchmarks/icloud-flat-native-source-proof-2026-10-05.json)
keeps synthetic controls apart from the actual export scan. The corrected
feature-only scanner accepted an unchanged copy of the genuine export with an
explicit `root: null` and independently computed semantic-v2 for all 42 files.
It permits only redundant local size fields that exactly match bounded classic
headers; strict wrapped-package checks retain their previous rules. This is an
offline source proof, not readback of the edited provider version. Browser editing is
not an ordinary or atomic save from a Linux editor through FUSE, and this partial
trial does not close the native application acceptance gate.

### Compiled storage-format declaration on 5 October 2026

The daemon now exposes `--storage-format-json` before opening local state or
starting the service. It reports the compiled journal and metadata writer
constants. The package-switch preflight queries the exact selected executable
through a held descriptor, caps time and output, requires the supported schema
pair, and rechecks service routes and retained-state markers after both queries.

The [registered format validation](benchmarks/storage-format-attestation-2026-10-05.json)
first demonstrated the missing report on the original executable. The corrected
daemon tests, strict format and route-change controls, isolated package-switch
scenarios and complete `scripts/check.sh` passed. Two queries of a derived,
uninstalled copy of the actual new daemon also passed; that copy is not evidence
of package signing or an installed transition.

This supplies a concrete prerequisite for requirement488. Every retained state
still refuses package switching, including matching formats under released
policy. Existing held-policy restrictions remain. No installed account was
migrated, no service restarted, and no full-iCloud requirement closed.


### Derived compatibility and fresh Numbers preparation on 5 October 2026

The [ordinary derived-state compatibility trial](benchmarks/icloud-derived-state-compatibility-2026-10-05.json)
completed all eight child commands and the final preservation checks. An exact
older build produced genuine journal14/metadata7 state with an interrupted
cursor, two sealed ordinary file versions and a dirty successor. Independent
copies retained those contents through read-only inspection, migration to19/8,
a second opaque snapshot and completion of the interrupted cursor. The unchanged
old writer refused the newer formats. Both source inventories and both opaque
images remained unchanged; the entire old-refusal copy also remained unchanged.
No native-document checkpoint, installed upgrade or full requirement488 closure
is claimed.

A [new bounded Numbers trial](benchmarks/icloud-numbers-browser-editor-fresh-2026-10-05.json)
passed40 source-bound local controls and the actual local readiness check,
created one fresh owned folder and publicly imported one test document. The
writable producer then stopped and unmounted. Switching the isolated settings
to read-only refused because the private preparer had created the immutable
settings snapshot with0600 while its controller required0400. Active settings
were not changed. The trial aborted without replay; the watcher settled all
recorded owners and the mount, and its parent retained exit0 after reaping it.
No independent A read, browser edit, B verification or remount was performed in
this successor. This is a local test-preparation mismatch, not a provider write
failure. The existing document and evidence remain retained. All six full-iCloud
requirements and the installed deployment hold remain open.

A separate local regression check reproduced that exact preparation failure:
the actual settings-producing code reached the unchanged operational RO reader
and failed its desired-success assertion after confirming that source snapshots
and active settings were preserved. The same endpoint passed with the narrow
producer correction: immutable RW/RO snapshots0400, mutable account settings0600.
The consumer guard was retained. This corrected local contract does not reopen
the closed trial or supply the missing live A/B/remount evidence.

A [fresh successor](benchmarks/icloud-numbers-browser-editor-successor-2026-10-05.json)
then passed38 selected source-bound controls and the actual immutable-settings
reader. Its live arm created a fresh owned folder, publicly imported one Numbers
document, stopped the writer, switched the isolated settings to read-only, and
independently verified the imported content. The initial browser screenshot
showed7/3/10. After an intended read-only selection of C2, the browser instead
showed text `us` in that cell with Undo enabled, before the planned A2 edit or
browser-edit permit. The cause is unknown. The arm stopped without a corrective
write, native export or automatic retry. The controller and watcher settled
the exact owned processes and mount; the watcher was externally reaped with
exit0. This establishes imported A content, not browser fidelity, current B,
read-only remount B or full requirement487 closure.

The [current-format native restoration trial](benchmarks/icloud-native-derived-restoration-2026-10-05.json)
adds a real synthetic HTTPS producer to the stopped-account copy workflow.
It generated and encrypted a native19/8 checkpoint, closed its writers and was
reaped before collection. A separate derived account restored that checkpoint
using its separately retained, account/operation-bound synthetic wrapping key.
The normal account router and worker completed exactly one inspection, verified
Current and Trash, and published the acknowledged metadata without increasing
any of the five mutation counters. Original, stopped source, snapshot and key
were preserved. Wrong-key, ciphertext-tamper and wrong-operation controls refused;
substituting only the positive consumer's key made its real authentication
endpoint fail. Byte-exact restoration passed all six native recovery tests.

This is current19/8 native restoration with a reconstructed synthetic service
state. The genuine older14/7 build has no native iCloud checkpoint API, so the
ordinary migration arm above covers that historical source separately. Neither
arm proves desktop keyring availability, an installed transition or downgrade,
an abrupt native Uploading crash, or full requirement488 acceptance.

### Fresh browser save confirmation on 5 October 2026

The [current-source executable bindings](benchmarks/icloud-numbers-editor-revalidation-builds-2026-10-05.json)
validate four uninstalled programs against the committed Rust/Cargo sources.
Cargo reused its validated cached artifacts; no forced recompilation is claimed.
Two [local invocation](benchmarks/icloud-numbers-browser-editor-confirmation-2026-10-05.json)
and [startup ordering](benchmarks/icloud-numbers-browser-editor-confirmation-fresh-2026-10-05.json)
mistakes by the test operator stopped before any folder/import dispatch. Their
closed owners and evidence remain retained. These are test execution failures,
not evidence of an Apple write failure.

A [separate fresh trial](benchmarks/icloud-numbers-browser-editor-confirmation-completion-2026-10-05.json)
passed38 source-bound controls and the actual produced-settings reader, created
one owned folder, imported one Numbers document and independently verified A.
After stopping the writable producer, the browser showed7/3/10 and
`SUM(A2:B2)`. One change to A2 produced11/3/14. The Saving indicator appeared and
disappeared; an actual server reload retained those values and the formula.
The prior successor's unexpected `us` input did not recur in this trial. Its
original cause remains unknown; the user reports no conscious edit but cannot
exclude inadvertent input while the website was open.

One native Numbers export was requested. The browser showed export preparation,
then an export-frame error page, and no matching file appeared in the expected
download directory. The cause was not isolated. The arm stopped without another
export or mutation; its watcher and outer runner were reaped with exit0. No B
archive, independent current-B proof or read-only remount-B proof is claimed.
The saved document remains available for inspection. All six full-integration
requirements and the installed deployment hold remain open.

### Retained Numbers current read and fresh mount on 5 October 2026

A [new read-only observation](benchmarks/icloud-retained-numbers-read-2026-10-05.json)
verified the retained document from the browser trial above. Its fresh local
state copied only the same-account opaque saved session; it did not resume the
closed browser controller or repeat any folder, import, edit or export operation.
The feature-only reader selected the exact account/collection/parent/item,
required a revision different from imported A, and captured its actual current
PACKAGE bytes with pre/post-transfer metadata and representation fences.

A second generic reader independently fetched and verified that revision. A
fresh normal read-only FUSE mount then supplied a distinct archive capture,
guarded by its mount identity, open descriptor and account-scoped source metadata.
After closing the mount, the existing offline scanner verified that capture.
All three acquisitions matched semantic version 2: ten entries, seven files,
138,899 expanded bytes and digest `7ff32522e4eadd67b22c69a0a761c1fe709358259294837ef37b2f51650cb043`.
This differs from A. Both direct and mounted archives were 64,903 bytes, but
their raw hashes differed; raw archive equality is not claimed.

The bounded run completed in 65.6 seconds. The independent closure audit found
all seven recorded process identities absent, the mount and socket absent,
registered inputs and opaque session unchanged, and the installed daemon and
all nineteen package files unchanged. No cloud mutation or automatic retry
occurred in this observation. The complete local `scripts/check.sh` also passed
for the new feature and admission tests before the source commit.

These are separate read acquisitions using Cirrove's existing implementation,
not independent provider implementations. They establish scoped content-tree
consistency, not independently decoded cell values, browser export fidelity,
Linux editor saves or full iWork reliability. The earlier browser arm remains
closed with its unresolved export. All six full-integration requirements and
the installed deployment hold remain open.

### Independent Numbers cell decoding on 5 October 2026

A [bounded local comparison](benchmarks/icloud-numbers-independent-decoder-2026-10-05.json)
used the installed libetonyek `numbers2raw` importer on an unchanged copy of the
actual current-provider archive and a separate flat diagnostic copy. The latter
removed only the exact outer document-name wrapper; every member path, type and
byte was preserved, including the opaque nested `Index.zip`. Production offline
semantic-v2 scans verified equal content-tree identity before decoding.

Both one-shot decoder calls succeeded and produced identical callback output.
An independent extraction audit verified one balanced sheet/table scope and
zero-based coordinates for A2=11, B2=3 and C2=14. This binds the cached values to
the captured provider document independently of Cirrove's content readers.
The wrapped layout itself is readable by this installed importer.

No formula property or SUM token was emitted. The result does not prove the
source lacks its formula; the browser separately showed `SUM(A2:B2)`. It proves
the cell values, not independent formula fidelity. All six recorded local process
identities are absent. No cloud, session, daemon, installed-profile or old
controller operation occurred. The derived copy is retained as a diagnostic
artifact, not a browser export or normal mount serialization. Visible Linux
editor saves, export fidelity and the complete iWork format matrix remain open.

### Actual local Calc import, save and reopen on 5 October 2026

The [registered Calc trial](benchmarks/icloud-calc-editor-2026-10-05.json)
ran the installed LibreOffice application with visible windows through its UNO
API, a fresh private profile and a finite original deadline. Initial startup
failures are retained separately: one worker identity acquisition failed before
document actions, and a later direct Office startup exited with code81. The
successful fresh trial handled exactly one normal startup restart before any
document attempt. All three recorded child processes were reaped and are absent.

The unchanged actual Numbers archive opened read-only with values 11, 3 and 14.
Calc reported C2 as VALUE with formula text `14`, not the SUM formula seen in
Apple Numbers. No save, repair or conversion of the Numbers source occurred;
its bytes and filesystem metadata remained unchanged. This is a confirmed
application-import fidelity limit for this fixture, not evidence that the source
lost its formula or that Cirrove changed it.

Calc then created its own XLSX with 7/3/SUM=10, closed and reopened it, changed
only A2 to11, called `store()` once, and reopened 11/3/SUM=14. Both reopened
versions reported FORMULA with zero error. Independent inspection of the two
immutable OOXML snapshots confirmed the formula and values. Visible-window API
checks do not constitute a human GUI witness.

The narrow XLSX DATA fixture verifier failed at its intended unsupported-format
endpoint before the correction and passed both targeted tests afterwards,
including six refused alternatives. Five metadata-helper controls also passed.
Those controls do not establish the saved-session facade end to end. No cloud
request, mounted save, normal LibreOffice profile change or installed Cirrove
action occurred during the local trial. The next endpoint is one actual editor
save through a fresh isolated writable mount, receipt-selected current content
verification and a fresh read-only remount. All full-iCloud criteria remain open.

### Partial actual Calc mounted save on 5 October 2026

The [registered mounted trial](benchmarks/icloud-calc-mounted-editor-2026-10-05.json)
created one new owned folder through a normal isolated writable iCloud mount.
Actual Calc then saved its first XLSX and reopened 7/3/SUM=10. Its 5281-byte
source and both sealed content generations have the same digest. The trial
stopped before independent cloud verification, so the second editor save and
read-only remount were never admitted. All four recorded children were reaped;
the controller, test mount and socket are absent. The installed daemon and all
19 recorded packaged files are unchanged.

The actual journal records a lock-file create, an acknowledged empty temporary
file, its content successor, a separate final-document create and two pending
removals. The acknowledged empty item is the temporary file, not the XLSX.
No destination-victim replacement record was emitted. This first save therefore
does not establish atomic replacement of an existing document.

Three uploads were unconfirmed after shutdown. Their first deferrals occurred
in the shutdown second; cancellation itself maps to that state. The controller
also refuses `Verifying`, which can represent an inspection in progress.
Without a pre-cleanup refusal snapshot, these observations do not distinguish
an initiating provider error from a conservative test refusal and subsequent
cancellation. No provider fix or reliability claim follows from them. The
fixture remains preserved, with no repeated save or mutation replay. Current
cloud contents and the initiating refusal still need independent observation;
all full-iCloud criteria remain open.

### Fresh read-only observation of the retained Calc trial on 5 October 2026

The [registered observation](benchmarks/icloud-calc-retained-read-2026-10-05.json)
used separate read-only state and cache with the same account credentials; it
never reopened the original upload journal or repeated the editor save. Initial
observer versions stopped before file inspection: first at an empty initial
account list, then at the first published non-ready account state. The retained
health database recorded a completed refresh. Source inspection showed that the
manager can publish a previously copied indexing status after further awaits.
These observations do not establish a provider failure or explain the original
writable trial's initiating refusal.

A fresh corrected observer retained the original finite deadline and waited for
published readiness while checking account, collection, root, daemon, read-only
mount and socket identity. Its actual status witness changed from indexing to
ready. It verified the original owned folder, but the bounded folder listing did
not include the final XLSX. This establishes neither current file bytes nor
definite provider noncommitment. The own daemon was reaped, mount and socket were
absent, and the installed daemon and all 19 packaged-file digests were unchanged.
The original sealed edits remain preserved. Cloud confirmation, a second editor
save, native-format editing and all full-iCloud release criteria remain open.

### Calc cleanup diagnostics and the local successor correction

The [registered diagnostics](benchmarks/icloud-calc-mounted-editor-diagnostic-2026-10-05.json)
retain two fresh writable trials. The first stopped at Office process cleanup;
its stopping error does not establish an upload failure. Actual local-process
controls then demonstrated a terminal-handle cleanup correction, including
refusal for a live identity mismatch and preservation of supervised cancellation.
The next fresh trial reaped all four owned children cleanly and captured a
different refusal before shutdown: the temporary file's sealed content successor
was VerifyRequired, with no session or transferred bytes, while its dependent
Remove was pending. Neither trial attempted the second editor save or remount.

Source inspection identified a local journal guard that rejected an already
unlinked owner whose head was its dependent Remove. A real-journal synthetic
worker regression first failed on the original code before the successor's
provider begin call, with both files' bytes preserved. The correction checks
the exact ordinary upload-to-Remove lineage at reservation and confirmation.
The original positive then passed. Six focused tests also passed, including
post-unlink writes/truncation and exclusive reopening after deletion, saved
reservation recovery across restart, and three matrices of 40 hostile bindings.
One earlier control run failed at fixture creation because its existing directory
was 0755; correcting the private fixture layout changed no production behavior.
The failed fixtures and results remain retained.

The original cloud trials remain unconfirmed. These tests prove the local
sequence and its guarded correction; they do not prove any retained trial's
exact first transfer exception, a successful real-provider Calc save, native iWork editing or
full iCloud acceptance. The fresh arm using newly built binaries is recorded
below. All six full-iCloud release criteria remain open.

### Fresh Calc uploads and the metadata-index cleanup boundary

The [fresh follow-up arm](benchmarks/icloud-calc-after-unlink-2026-10-05.json)
used the successor correction and new frozen binaries. Actual Calc saved and
reopened its own XLSX with 7/3/SUM=10. All four ordinary upload records reached
Uploaded, including the previously refused temporary file's staged identity
handoff. The lock-file removal completed; the temporary file's resolved Remove
remained Pending. The bounded arm stopped and reaped all four owned children,
unmounted cleanly and preserved the fixture. The installed daemon and its 19
artifacts remained unchanged. No old writer or pending mutation was replayed.

This confirms the upload endpoint, not a complete cloud editor save: independent
generic DATA readback, the second editor save and fresh read-only remount were
not admitted. The first temporary-removal exception was not historically logged.
The stopped state showed its new item ID absent from all metadata presence and
absence tables while its parent was indexed; source planning required the
missing item-to-root chain before independent cloud verification could start.

The [controlled source regression](benchmarks/icloud-receipt-remove-source-2026-10-05.json)
then reproduced this refusal using real journal handoff and resolution APIs,
before any network call. The same positive passed after adding exact
operation-bound receipt authority for a completely unindexed leaf. Six service
tests also passed, covering 38 hostile admission arms, indexed metadata and
ancestry conflicts, ordinary indexed relocation routing, and eight real
preparation deferrals with newer unlinked bytes preserved through exclusive
journal reopening. The independent content/revision checks remain mandatory.
The subsequent real-provider cleanup result is recorded below. A complete editor
trial remains open; this correction closes none of the six full-integration
acceptance criteria.

### Fresh Calc cleanup and metadata registration refusal

The [fresh receipt-source arm](benchmarks/icloud-calc-after-receipt-source-2026-10-05.json)
used the exact corrected source and four frozen test binaries. Actual Calc
stored and reopened its own 5285-byte XLSX with 7/3/SUM=10. All four ordinary
uploads reached Uploaded without a recorded failed attempt; both the temporary
file and lock-file removals reached Applied. All seven journal queue entries
completed. This is real evidence that the previously blocked cleanup progressed.

The independent metadata helper then stopped before saved-session or provider
access: the controller emitted fractional wall-clock seconds, while the Rust
registration schema requires integer seconds. No metadata attempt or independent
DATA proof was produced. The second save and read-only remount were therefore
not admitted. The stopped writer was not reopened or replayed, all five original
child handles were reaped, and the mount and control socket were absent. The
installed daemon and all 19 packaged artifacts remained unchanged.

Journal acknowledgement is distinct from independent current-content verification.
This result does not prove exact cloud bytes, a complete two-save editor workflow,
native iWork editing, installed upgrade acceptance or general provider reliability.
All six full-iCloud criteria remain open. The next separately registered fresh
arm will project only the metadata registration's original wall-clock values to
integer seconds, preserving the controller's original deadline and cleanup reserve.

The [next fresh arm](benchmarks/icloud-calc-after-metadata-clock-2026-10-05.json)
used that narrow controller correction. Before cloud dispatch, the actual old
boundary's registration failed a privately extracted, byte-exact Rust parser;
the same parser accepted both corrected phase registrations and ten refusal
guards. All five existing Rust metadata tests also passed. The initial parser
slice-hash guard refused a trailing-newline scope mismatch before compilation;
the corrected source map and unchanged consumer remain retained. These controls
prove local registration compatibility, not cloud content.

Actual Calc again stored and reopened its own XLSX with 7/3/SUM=10. This arm
stopped earlier: its restarted Office process failed the exact argv/executable
identity check while the original Child was not yet terminal. The controller
sent no signal after that refusal and did not reach metadata, a second save or
read-only remount. The later stopped journal retained uncertain first-upload
checkpoints; these shutdown states do not establish a provider failure. Three
original child handles were recorded as reaped, while the restarted Office
handle remained unresolved in the original closure. Its later PID absence does
not retrospectively prove that handle was reaped. All recorded PIDs, mount and
socket were subsequently absent; the installed baseline remained unchanged.

A separate real-child control then reproduced the original controller's refusal
before its desired natural-termination endpoint. A corrected fallback passed that
same endpoint and three guards for a still-live child, an expired cutoff and
supervisory cancellation. It waits only on the retained original handle, with a
timeout allowance of up to one second within the original cutoff, and never
signals an unconfirmed process. All five control children were reaped through their original handles,
with zero signals. This does not prove which identity component differed in the
cloud arm or that its historical failure was a natural-exit race. Complete cloud
editor and full native-iWork acceptance remain open.

### First independently verified Calc save and the second-save source boundary

The [fresh A/B arm](benchmarks/icloud-calc-after-natural-exit-2026-10-06.json)
used the unchanged corrected service binaries and the locally tested controller.
Actual Calc stored and reopened its own 5283-byte XLSX with 7/3/SUM=10. All seven
first-save journal entries completed, including both temporary and lock-file
removals. The independent generic verifier read the exact current DATA item from
iCloud; its size and SHA-256 matched the actual saved file. This is the first
complete independent first-save readback in this Calc sequence.

Only after that verification, Calc saved and reopened a second 5610-byte version
with 11/3/SUM=14. Its sealed temporary-file upload reached VerifyRequired before
the second cloud-content check or read-only remount could start. The stopped
journal shows that the queued atomic replacement had already renamed the same
source owner and become its latest operation while its prerequisite content
upload still awaited completion. Both reservation guards in that arm's source
reject that ordering before provider delegation. The exact runtime exception was
not retained; the source-derived boundary therefore needed the controlled
synthetic reproduction recorded below.

The bounded arm stopped without repeating either save. All ten original child
handles were reaped; the mount and control socket were absent. The installed
daemon and all 19 packaged artifacts remained unchanged. The fixture and uncertain
operations stay retained. The first version's verified bytes do not prove the
second version, native iWork editing, installed upgrades or general reliability;
all six full-iCloud acceptance criteria remain open.

The [controlled source regression](benchmarks/icloud-atomic-source-handoff-2026-10-06.json)
then reproduced the source-prerequisite refusal in two actual worker sequences,
within one directory and across two directories. Both first failed on the original
guard before the source's provider begin callback, with original and sealed bytes
preserved. The unchanged tests passed after binding the earlier source upload to
its exact pending atomic replacement, prerequisite and cleanup. A private optional
reservation field retains that target across restart; existing reservation bodies
deserialize without a schema migration. The captured original source parent is
verified separately from the final destination, and the upload's new temporary
receipt supplies the subsequent cleanup identity.

Nine focused tests passed, including 47 hostile authority arms across fresh
reservation, saved reservation and acknowledgement, recovery of the same saved
reservation after exclusive journal reopening, and ordinary newer-save parity.
Later unsealed descriptor writes also survived both handoffs, cleanup and reopening,
while the original sealed bytes stayed unchanged. An initial control run had six
passing tests and three fixture-inspection failures: its snapshot reader correctly
rejected deliberately corrupted digest metadata before the intended authority
check. Reading the owned fixture's raw sealed bytes independently fixed that
inspection; production remained unchanged, and the subsequent nine tests passed.
These are local synthetic results. The retained cloud writer is not replayed,
and a fresh real-provider two-save/remount trial remained necessary at that point.
The subsequent successful ordinary XLSX arm is recorded below.

The complete `scripts/check.sh` then passed in 674.461 seconds, including every
Clippy variant, workspace and feature tests, kernel FUSE scenarios, script checks,
documentation and the acceptance ledger. All 477 recorded Rust/Cargo source pins
remained unchanged. Desktop display scenarios were not run. The installed daemon,
unit and all 19 packaged artifacts remained unchanged; this is source validation,
not installed delivery or closure of a full-iCloud release criterion.

### Delayed temporary-file creation and atomic takeover on 6 October 2026

The [fresh Calc trial](benchmarks/icloud-calc-after-atomic-source-handoff-2026-10-06.json)
on `abc6d73` again completed its first save and independent DATA readback: all
5280 bytes matched the source digest, with 7/3/SUM=10 after local reopening.
Its second 5612-byte version reopened locally with 11/3/SUM=14, but source upload
12 reached VerifyRequired before independent second-save readback or remount.
All ten original children were reaped, the owned mount and socket were closed,
and the installed daemon and 19 packaged files remained unchanged.

The stopped source audit found the missed ordering. Atomic acceptance clones its
source into a reserved cleanup object before the empty temporary Create is
acknowledged. The later acknowledgement advances the source receipt, while the
cleanup retains `remote=None`, sequence 0 and revision 0. The previous guard
required that shadow to hold the already acknowledged receipt. No exact original
transfer exception was retained; this is a source-derived mismatch, not proof of
a provider transport failure.

The [controlled correction](benchmarks/icloud-atomic-source-delayed-create-2026-10-06.json)
first reproduced two actual same-directory/cross-directory failures on the old
code. The same byte-exact tests then passed with a creation-origin UUID captured
inside the original replacement transaction. An empty cleanup shadow requires
that marker and the exact later zero-Create receipt; missing historical evidence
is not promoted. All 14 module tests passed, including 30 new hostile saved
reservation/confirmation arms and 47 existing authority arms. Both original
Relocate-based parity arms also passed. A private initial marker draft failed
that same parity test at its first arm with Storage; optional upload lookups
corrected it without granting creation authority to mutation-based topology.
No SQL schema migration or installed change was made. A new actual two-save
cloud trial remains required; no full-iCloud acceptance criterion closes here.

The [separate public Pages chooser preflight](benchmarks/icloud-pages-public-chooser-preflight-2026-10-06.json)
stopped before its first UI action because the Python accessibility client
aborted in libatspi after the existing accessibility-bus socket refused its
connection. The coredump confirms SIGABRT and the GLib/libatspi/GI stack. No
import RPC was recorded. The outer original Python handle was reaped; the
crashed harness never wrote its original Desktop-child reap receipt. Root
revalidated that exact Desktop PID, birth ticks, executable digest and argv,
observed terminal state after a pidfd signal, and confirmed later process
absence. This does not recreate the missing original handle proof. Its stale
owned socket and fixture remain retained. No global accessibility repair or
installed activation was performed. The next isolated accessibility session
must be verified before another fresh actual public chooser trial.

On the subsequent fresh 8e60 Calc arm at `3179522`, both the sealed source and
final target uploads reached their durable staged acknowledgements and
transferred all 5,611 bytes. This verifies the corrected source handoff in that
owned ordinary-file arm. The run still failed at its original active cutoff:
only temporary-file cleanup sequence 14 remained pending after seven preparation
backoffs. Its exact confirmed temporary receipt was absent from the evictable
metadata index; the existing receipt fallback excludes atomic cleanup. This is
a concrete source-derived reproduction candidate, not a reconstruction of the
historical exception. Independent B DATA readback and read-only remount were not
reached. All ten original children were reaped, the mount/socket disappeared,
and the installed daemon and 19 packaged files remained unchanged; see the
[fresh delayed-Create trial](benchmarks/icloud-calc-after-delayed-create-2026-10-06.json).

An isolated accessibility health diagnostic separately passed with its own two
foreground buses, exact registry PID, empty Atspi desktop and four original child
reaps. It makes no public chooser or provider acceptance claim, and the earlier
first-bus identity refusal remains unexplained; see the
[health diagnostic](benchmarks/icloud-pages-accessibility-birth-diagnostic-2026-10-06.json).

The completed-atomic-cleanup receipt correction now passes the same two
synthetic endpoints that failed with `Conflict` on unchanged production code.
Four focused tests pass with 30 journal authority and six metadata refusal arms.
The strict index-first route and independent provider digest/revision checks
remain in force; no synthetic result confirms the unresolved real cleanup.
The initial fixture construction failures are recorded separately and are not
counted as failure-before-fix evidence; see the
[cleanup regression](benchmarks/icloud-completed-atomic-cleanup-routing-2026-10-06.json).


### Two confirmed Calc saves and a remaining remount boundary

The [fresh cleanup-corrected trial](benchmarks/icloud-calc-after-completed-cleanup-2026-10-06.json)
on `333a0a4` independently read back both ordinary XLSX versions: A was 5,281
bytes and B was 5,611 bytes, each matching the saved source SHA-256. All 15
journal entries completed; all nine uploads and six mutations had zero failed
attempts. Cleanup sequence 14 reached Applied. This is actual owned-account
evidence for the completed cleanup correction, not a native iWork matrix result.

The same trial stopped at the subsequent read-only remount's regular-file/size
assertion. The actual descriptor attributes were not retained, so its failing
clause cannot be reconstructed. The stopped metadata contains both old A and
new B under the same parent and name, with neither identity marked absent.
An independent read-only audit of the retained cache and canonical name query
selects A at 5,281 bytes, while the exact B receipt expects 5,611 bytes. Ordinary
handoff acknowledgement commits the journal; its maintenance can retire B
without publishing the old identity's confirmed Trash relocation to the index.
This deterministic cache/source reproduction led to the controlled regression and
durable exact-identity publication correction described below. That arm did not
establish a fresh live remount. No save was repeated. All 13 original children
were reaped, the owned mount and socket were closed, and the installed daemon and 19 packaged artifacts remained unchanged.

A separate [bounded public chooser trial](benchmarks/icloud-pages-bounded-public-chooser-2026-10-06.json)
passed isolated accessibility health and the exact chooser/Desktop ownership
acknowledgements, then refused the first accessible-tree traversal at its
16-level/512-node bound. Neither the failing depth nor count was retained.
There were no UI actions or import requests. Its exact Desktop and five original
children were reaped and its sockets closed. The next diagnostic preserves the
same bound and records the actual values before refusal; no public Pages import
or full-iCloud criterion closes here. The [observer controls](benchmarks/icloud-pages-birth-observation-controls-2026-10-06.json)
record two desired pure decision failures and ten corrected passes, without
claiming actual kernel timing or UI acceptance from mocks.

### Durable ordinary replacement metadata repair

The [metadata regression](benchmarks/icloud-ordinary-handoff-metadata-publication-2026-10-06.json)
now reproduces the duplicate old/new listing after successful handoff on unchanged
production code. The same test passes after the correction: only B remains at the
original name, A has its exact validated backup location, the completed cursor is
unchanged, and a delayed old observation cannot resurrect A.

Schema20 enqueues immutable publication jobs in the handoff acknowledgement
transaction. Nine additional tests pass with the original regression, covering
later sealed/dirty saves, completed rename and unlink, normal read-only Engine
startup with an unchanged Uploading successor, restart before/after Store commit,
newer observations, scoped receipt refusal and corrupt-job backoff. These are
synthetic checks. Two initial compile errors are retained separately from the
single genuine failure-before-fix endpoint. The subsequent owned cloud remount
result is recorded below; all six full-iCloud requirements remain open. The
current deployment hold is schema20/metadata8; the installed schema14 Canary daemon remains untouched.

A further supported-API startup regression reproduced the same retained metadata
problem when `Manager::start_with_provider` supplies no write factory, despite a
recorded ReadWrite grant. The unchanged fixture fails at the exact backup lookup;
the corrected fixture passes while preserving Uploading rows, working bytes,
sealed payloads and settings. It cancels and joins the real Manager before its
assertions. The nonempty mount sentinel deliberately refuses before FUSE, so this
is startup coverage, not a successful kernel mount or remount. Initial launch and
remount now select the same metadata-only task from the effective connection mode.
The complete project check now passes this final correction in 644.87 seconds,
with all 534 code/script/policy pins unchanged and kernel FUSE checks included.
Desktop display scenarios remain separate; the actual owned cloud remount result
is recorded next.

### Actual Calc two-save DATA readback and read-only remount on 6 October 2026

The [fresh metadata-publication arm](benchmarks/icloud-calc-after-metadata-publication-2026-10-06.json)
on `b078a414` completed once in 479.19 seconds within the original 600-second
window and 45-second cleanup reserve. Calc stored and reopened A with 7/3/SUM=10,
then the independent iCloud DATA verifier matched its 5,281-byte source and
SHA-256. Only after that proof, one B save reopened 11/3/SUM=14. Its independent
DATA readback matched the new 5,612-byte source. A distinct normal read-only
remount then captured 5,612 bytes with the same B SHA-256,
`309095700a7965e6e377c06d826766f0cacb54c6be6c06e32d69c2b5af8b4ca0`.

All 15 queue entries completed: nine Uploaded operations and six Applied
mutations, with zero failed attempts. The sealed temporary source, final target
and temporary cleanup all completed. Three durable ordinary metadata jobs were
done with no failures or backoff. The stopped cache held original A as the exact
target receipt's 5,281-byte Trash backup; B was the sole identity at the original
folder/name. That backup is A, distinct from the source handoff's prior empty
temporary file. This is actual DATA and remount evidence for the correction,
not a claim derived from synthetic tests.

The original controller and all 13 owned children were reaped, the outer runner
was gone, and mount/socket closure was independently checked. All 477 runtime
source pins, 534 checked code/script/policy pins and four frozen roles remained
unchanged. The installed daemon's unit, PID/birth and executable, plus all 19
packaged files, stayed unchanged. No old writer was reopened or save repeated.

The read-only audit used WAL-aware SQLite access for the stopped journal. Its
DB, WAL and SHM byte hashes stayed unchanged, but SHM coordination advanced
mtime/ctime; the audit therefore does not claim unchanged filesystem timestamps.
Metadata used immutable read-only access only after confirming no WAL existed.
The exact preregistration bytes were preserved and matched to the actual launch
manifest before outcomes were appended.

This is one bounded ordinary XLSX arm. Visible-window UNO checks record no human
GUI witness. That writer arm did not independently read the original in Trash;
the following observer now confirms its bytes. Restoration remains unverified;
native iWork editing, public Desktop import, all six full-iCloud
gates and the schema20/metadata8 installed-migration hold remain open.

The [unchanged-bound desktop diagnostic](benchmarks/icloud-pages-accessibility-walk-diagnostic-2026-10-06.json)
recorded an owned GTK node at depth17 with only 20 output nodes. After a controlled
desired failure and seven corrected pure traversal checks, the
[fresh bounded-walk trial](benchmarks/icloud-pages-bounded-owned-walk-2026-10-06.json)
completed that walk but refused ambiguous name-only account selection. No UI
action or import request occurred; all original owned processes were reaped and
sockets closed. A fresh selector will require an owned, visible expandable row
and preserve ambiguity refusal. Public Desktop import remains unverified.

### Independent original Calc Trash read on 6 October 2026

A [new saved-session read-only observer](benchmarks/icloud-calc-trash-original-2026-10-06.json)
bound the completed target replacement to the exact original A, independently
of the distinct empty temporary-file shadow. The fresh reader confirmed its
receipt-bound Trash identity and revision, streamed all 5,281 original bytes,
and matched SHA-256
`3645854724d003f2a18ce55f2fc6f1b5a9e761598f5e9d8d93ee23b1c77b1dba`.
This was one read, with no restore, mutation, old writer reopen or retry.

The CLI exited 0 and its original handle was reaped. The test controller retained
an overall success result, then caught its own `SystemExit(0)` and reported exit 1.
That instrumentation false-failure remains in the artifact. Offline controls
using the actual old and corrected main code reproduce exit 1 versus exit 0,
with one stubbed run each and no provider calls; an initial missing-stderr harness
setup failure is retained separately. The future full controller was not rerun.

An independent stopped audit confirmed the proof/fixture/target receipt bindings,
original child and controller reaps, absent processes/mount/socket, all 480 runtime
and 537 checked source pins, and the unchanged installed daemon and 19 packaged
files. The registered 90-second window and 20-second cleanup reserve completed in
3.644 seconds. README and result documentation were updated after the full check;
no source/script/policy change followed it. This proves original raw bytes for
this owned XLSX arm, not restoration, native application fidelity, a complete
Trash inventory, installed migration or any full-iCloud gate closure.

### Public Desktop account expansion diagnosis on 6 October 2026

The [fresh account-row chooser trial](benchmarks/icloud-pages-account-row-chooser-2026-10-06.json)
selected the exact owned expandable row, then refused before its first action.
A [new state observation](benchmarks/icloud-pages-account-row-readiness-diagnostic-2026-10-06.json)
retained `ENABLED=false`, `SENSITIVE=true` and no actions on that row.
The [subsequent bounded subtree observation](benchmarks/icloud-pages-account-row-subtree-diagnostic-2026-10-06.json)
captured all 15 owned nodes: even visible buttons offering `click` reported
`ENABLED=false`. No expansion action was exposed in the observed subtree.

These runs completed in 1.471, 1.270 and 1.421 seconds, respectively. Each used
the same frozen default Desktop and isolated synthetic account. All original
process handles were reaped, owned sockets closed, inputs remained unchanged,
and no import or cloud request occurred. This identifies limits of the test's
AT-SPI action admission and expansion mechanism; it does not establish a broken
user interface. A fresh GTK-specific controller must prove its state guards and
own-window keyboard activation before testing the real chooser. Public Desktop
import, original-mount publication and independent native readback remain open.

The subsequent GTK-specific work now distinguishes two further controller
assumptions from product behavior. GTK application toolkit getters returned
null, while its public attributes correctly identify GTK. The pinned GTK ELF
then reported the same inode and exact pathname through `stat` and process
mappings, but different device numbers on this Btrfs host. A known-file
calibration retains the hashed source descriptor across loading that exact ELF
without calling `gtk_init`, then compares its physical mapping tuple and mount
namespace with the owned Desktop. The
[actual calibrated trial](benchmarks/icloud-pages-gtk-calibrated-library-chooser-2026-10-06.json)
passed that admission and all three read-only compositor queries.

It then stopped at GTK's unsupported AT-SPI `Component.GrabFocus` call, before
sending any key or opening the import dialog. All original children and utility
handles were reaped, sockets closed and inputs unchanged. This result proves
the library calibration on this host, not public import completion. The
[separate controls](benchmarks/icloud-pages-gtk-calibrated-library-controls-2026-10-06.json)
first fail under the original device comparison and pass eleven cases under
the correction; they do not replace the live chooser test. The successor must
use ordinary keyboard navigation addressed to the owned window and observe
the exact account row focused before activating it. No full release criterion
or installed deployment hold has closed.

### Actual public Pages chooser and confirmation Cancel completed on 6 October 2026

The [fresh visible-tree arm](benchmarks/icloud-pages-chooser-visible-tree-2026-10-06.json)
completed once in 4.025 seconds on the frozen default Desktop from `65a25139`,
with all 480 runtime and 537 checked source pins unchanged. Four addressed TABs
focused the exact owned account row; one SPACE expanded it. The public Import
control opened the actual GTK chooser. One chooser-addressed Ctrl+l exposed
its focused editable TEXT field with the `Location` placeholder. Explicit
AT-SPI interface calls set and read back the exact sealed local archive path.
The real Open action reached confirmation, whose archive filename and four
field labels were checked before one Cancel. The visible confirmation was then
absent. This proves the local public selection/confirmation/Cancel route, not
cloud submission or destruction of every accessible object.

The earlier [explicit-interface arm](benchmarks/icloud-pages-chooser-explicit-text-2026-10-06.json)
already reached Cancel but failed its final tree observation. A
[cache-clear successor](benchmarks/icloud-pages-chooser-post-cancel-cache-2026-10-06.json)
failed earlier, before its new post-Cancel step could execute. The
[event-dispatch arm](benchmarks/icloud-pages-chooser-context-dispatch-2026-10-06.json)
retained an exact owned child count of -1 during confirmation polling; its
underlying cause remains unproved. The successful successor observes live
visible owned descendants, pruning hidden or defunct non-root branches before
child queries. It preserves the original depth/node/child bounds and still
refuses invalid child counts on traversed visible nodes. The application root
remains observable even though it has no visible surface itself.

All 24 compositor utility handles, five outer child handles, the Desktop and
original controller were reaped. Owned sockets closed and all inputs remained
unchanged. Status, capabilities and recent activity were requested twice each;
there were exactly zero import requests and no provider access. These controller
corrections did not change Cirrove behavior or the installed service. Full Pages
import still needs one fresh actual Desktop submission, public completion and
publication on its original mount, independent semantic readback and Apple Pages
open of the exact allocated document. All six full-iCloud criteria remain open.

The [fresh real-provider Desktop arm](benchmarks/icloud-pages-live-desktop-import-2026-10-06.json)
subsequently created and confirmed one owned iCloud folder and warmed it on the
original mount. It stopped before launching Desktop: the reused accessibility
health child required the old local-chooser bus path instead of the newly
registered Pages Desktop path. Its initial address guard refused before GI
initialization. No import was submitted and no document was uploaded. The
original controller and all five children were reaped, and the isolated mount,
control socket and both private bus sockets closed. The installed service and
all runtime source pins remained unchanged. This is a test-controller path
mismatch, not evidence of a failed Pages provider import; the closed cloud arm
will not be replayed. The corrected exact Pages-path guard subsequently passed
five source-selected controls after the old guard failed the positive assertion.
A [fresh local health arm](benchmarks/icloud-pages-desktop-health-prefix-2026-10-06.json)
also passed the actual registry-owner and empty-desktop handshake. All four
original children and the parent were reaped; both private sockets closed and
inputs remained unchanged. This local check made zero provider calls and started
no Desktop or cloud daemon. It clears that test-infrastructure prerequisite for
a new owned Desktop import trial, with no full-integration gate credit.

That [new owned arm](benchmarks/icloud-pages-live-desktop-import-health-corrected-2026-10-06.json)
passed health admission, launched the normal Desktop and opened its public
chooser after the exact account row was expanded. It stopped at the chooser's
active-window PID/address check before Ctrl+l, pathname entry, confirmation or
submission. The operands were not retained, so neither focus-transition timing
nor external focus changes are established as the cause. The stopped journal
contains one applied folder creation and no uploads or native imports. Seven
original children, the controller and 21 compositor utilities were reaped;
mounts and private sockets closed. All 537 checked source pins, four frozen roles
and 19 installed files remained unchanged. A separate Root reporting exception
occurred while hashing the already-written result; its exact metadata mismatch
is unproved and does not change the retained child or journal results. The arm
will not be replayed. A fresh local chooser/three-field/Cancel check must now
validate bounded readiness observation before another cloud submission trial.

The [fresh local three-field arm](benchmarks/icloud-pages-local-chooser-focus-fields-2026-10-06.json)
then passed in 4.480 seconds. It retained exact owned-window checks, allowed at
most five initial read-only focus observations and kept the 40-utility bound.
The actual chooser selected the owned archive, and each uniquely title-labelled
Document folder, Destination folder and New document name field accepted and
returned its registered value through explicit AT-SPI interfaces. All three
were checked again before one Cancel; the visible confirmation disappeared.
All 13 recorded actions completed, with zero import requests or provider calls,
unchanged inputs and complete original-handle/socket closure. This verifies the
local field route, not the cause of the earlier focus refusal or a cloud import.
The next owned live arm must carry these validated controls through actual
submission, public completion, original-mount publication and independent reads.

The [subsequent real-account focus observation](benchmarks/icloud-pages-live-desktop-import-focus-fields-2026-10-06.json)
retained all five active-window samples: another PID and address stayed active
while the owned chooser remained mapped. It refused before Ctrl+l or submission,
with one confirmed test folder and no uploads. All seven original children,
the controller and 25 utilities were reaped; mounts and private sockets closed.
The Root result writer also completed its corrected immutable identity check.

A [fresh local successor](benchmarks/icloud-pages-local-chooser-defunct-transition-2026-10-06.json)
then passed all 14 actions in 4.680 seconds: one focus action for the exact
revalidated owned chooser, strict active-window checks, selection, three field
setter/readbacks and Cancel. Fresh owned accessibility-cache observations handle
chooser removal without repeating an action. A descendant returning child
count -1 is skipped only after rechecking its own PID, DEFUNCT state and absence
of SHOWING; the existing root, foreign-node and tree bounds remain enforced.
There were zero import requests or provider calls, with unchanged inputs and
complete original-handle/socket closure.

The [focus-only local arm](benchmarks/icloud-pages-local-chooser-owned-focus-2026-10-06.json),
[cache observation arm](benchmarks/icloud-pages-local-chooser-owned-cache-2026-10-06.json)
and [transition observation arm](benchmarks/icloud-pages-local-chooser-transition-2026-10-06.json)
remain recorded separately as failed prerequisites. The first refused a joint
time/liveness guard whose operands were not retained; its specific cause is
unproved. The second encountered an accessibility error during traversal.
The third retained the exact removed owned-node state before its count guard
refused. The successful local successor proves this bounded route, not general
Desktop reliability or an iCloud import. All six release criteria remain open.

The [fresh actual Desktop arm](benchmarks/icloud-pages-live-desktop-import-owned-focus-2026-10-06.json)
then submitted exactly one owned Pages import through the normal public UI.
Its public job succeeded, and the original warm mount yielded a 55,695-byte
canonical archive before shutdown. An independent provider reader returned zero
and matched the source semantic V2 tree: ten entries, seven files and 98,835
expanded bytes. The separately fetched archive differs in raw ZIP bytes, so
this is a content-tree comparison. [Apple Pages opened the exact newly allocated
document](benchmarks/icloud-pages-desktop-apple-open-2026-10-06.json) and showed
the expected text, without typing, saving or exporting.

The historical controller still exited one: its proof reader required mode
0400 while the successful observer writes private evidence at mode 0600.
An offline control reproduced the refusal and accepted those same unchanged
files with the documented mode. A stopped audit verified all 20 bound proof
clauses, exact journal receipt/publication and complete process/mount/socket
closure; all 537 checked sources and 19 installed files remained unchanged.
The read-only audit preserved journal bytes but changed SHM timestamps; full
stat equality is not claimed. No import was replayed and no historical result
was rewritten. This completes the bounded Desktop import branch. The first
release row still needs one fresh CLI arm with all endpoints for the same
document, including original-mount access and Apple Pages open. Native editing
and the other full-iCloud criteria remain open.

A [fresh public CLI arm](benchmarks/icloud-pages-fresh-cli-import-2026-10-06.json)
then completed its own entire import sequence in 138.802 seconds: the normal
account-bound CLI's initial job, durable operation and completion identifiers
matched the sole succeeded public job and exact uploaded journal receipt.
Before shutting down the original daemon, the original warm mount yielded a
55,695-byte canonical archive. The independent reader matched the source V2
content tree and exact new identity/revision. Its concrete `pages_desktop` fixture
discriminator binds source/root/label names; CLI provenance is recorded explicitly
and no Desktop submission is claimed for this arm. The producer's three private
JSON outputs are checked at their documented 0600 mode; other evidence remains
0400. The CLI, daemon, observer and Root controller exited zero and were reaped.

[Apple Pages opened that same CLI-created document](benchmarks/icloud-pages-cli-apple-open-2026-10-06.json)
with the expected source text, after the listing data ID and editor document UUID
were matched to the independent receipt. No typing, saving or export occurred.
The stopped audit passed 26 explicit checks, including all 537 source pins,
19 installed files and mount/socket closure. Journal bytes stayed unchanged;
SHM timestamps changed. Together with Desktop378, this closes the first full-iCloud
criterion using two separately complete fresh arms. The other five criteria
and installed schema-transition HOLD remain open. At that checkpoint, the
wrapped-package input could not express genuine flat Numbers exports. The
subsequent explicit source-contract work is recorded below; no synthetic success
or archive repack closes live application acceptance.

### Explicit flat Numbers admission on 6 October 2026

The [registered source-contract validation](benchmarks/icloud-flat-numbers-normal-source-2026-10-06.json)
first ran a desired admission test against a compilable wiring baseline. Exactly
one test executed and failed at the typed `Archive` refusal, before implementation.
This is a new format capability, not a regression of previously supported writes.

The implementation selects `flat_numbers` explicitly, requires an absent/null
source root, seals the unchanged archive and recomputes semantic V2. Legacy
wrapped requests keep their existing wire shape and required nonnull root.
The CLI accepts `--source-layout flat-numbers` instead of `--source-root`; the
Desktop form exposes the same explicit choice. Import and selected-revision
replacement persist distinct source kinds through payload verification,
completion, metadata publication and retained recovery inspection. Provider
downloads and original/Trash proofs keep strict actual-name wrappers. Ordinary
DATA items remain ordinary DATA.

The same desired source test passed after implementation. Offline admission of
the unchanged 138,945-byte Apple export also passed against the normal service
library with no optional features. Its source identity, bytes and 0400 mode stayed
unchanged. The actual GTK form passed its separate synthetic dialog scenario.
These checks do not upload that export or reopen a new copy in Apple Numbers.

The source and expanded archive limits remain 64 MiB with at most 10,000 canonical
entries. Flat layout is supported only for explicit Numbers input. The bounded
parser's narrow redundant local ZIP64 size mirror is supported; genuine ZIP64
archives and unsupported central/global ZIP extensions remain refused. Linux
editor fidelity and other flat iWork formats are not established by this change.

Two additional corruption controls failed before their respective fixes and
passed with unchanged tests afterward. Inner checkpoint layout tamper now refuses
before original verification HTTP. Matching invalid V1 flat completion proofs
now refuse publication discovery, status and completion, retaining the upload row,
source, payload and publication state. Pending flat bytes survive read-only
restart/export; completed uploads keep the existing rescue-export refusal.

The candidate journal writer advances to schema21, under the
[held deployment policy](development.md#flat-numbers-source-journal-schema21-held-prerelease-policy).
The installed packaged daemon and account state are not migrated. A future fresh
owned live arm must still establish unchanged-source upload, actual remote
representation and exact identity/revision, independent content, Apple Numbers
open and the required recovery endpoints. Local admission and synthetic transport
results do not close those criteria.

The complete `bash scripts/check.sh` run passed in 550.16 seconds, including
format, Clippy, workspace and optional iCloud tests, actual kernel/FUSE controls,
script checks, translations, ledger and documentation. All 538 pinned source and
translation files were unchanged during that run. Earlier failures and their
corrections remain in the same validation record. The running packaged daemon
retained its exact PID/start identity and binary/unit hashes. These local and
synthetic checks leave the live and installed acceptance endpoints open.


### Fresh flat Numbers import stopped on 6 October 2026

The [fresh public CLI arm](benchmarks/icloud-flat-numbers-fresh-public-cli-import-2026-10-06.json)
submitted the unchanged actual 138,945-byte flat Apple export exactly once into
one new owned folder. All source bytes transferred, but the durable operation
ended in `Conflict`. No uploaded completion receipt, successful public job or
metadata publication was established. The original daemon exited zero; the
controller terminated and reaped its original CLI. No import was resubmitted.

After shutdown, a separately registered read-only diagnostic found the exact
new document and downloaded its actual PACKAGE representation with parent,
item revision and representation fences before and after transfer. The complete
57,041-byte ZIP passed parsing but failed strict semantic V2 equality. Its 39
retained files are byte-identical to corresponding source files at changed paths:
`Index/` and `Metadata/` prefixes are absent, and the three source preview JPEGs
are missing. The expanded tree is 74,963 bytes instead of 133,153 bytes. This is
an actual content-tree mismatch, not optional directory-entry spelling.

The provider's requirement for a wrapped transport envelope is a working
hypothesis. The historical failing verification fence was not persisted, so the
current readback is not claimed as proof of that historical fence. The new
document was not opened in Apple Numbers. Strict source equality stays required;
this result closes no additional release criterion. The journal bytes, original
source and all 19 installed packaged files remained unchanged during the stopped
read-only audit, and the installed daemon retained its original process identity.

The metadata helper itself exited zero, but its private Root wrapper then failed
to parse a merged stdout/stderr log as one JSON document. Its existing typed
producer artifact was preserved; no metadata request was repeated. The subsequent
content observer used separate stdout/stderr files. A private launcher settings-key
error was also corrected before any provider dispatch. Both corrections are
recorded beside the failed arm rather than hidden in a later success claim.

The complete `bash scripts/check.sh` command passed again before publication in
530.89 seconds, with all 538 source/translation pins unchanged. The implementation
head's [GitHub CI](https://github.com/Dandiccf/cirrove/actions/runs/37425658643)
also completed successfully. These checks preserve the failed live result. A
private model-based regression draft had not been compiled or executed at that
publication. Its subsequent controlled results are recorded below.


### Separate flat Numbers transport envelope on 6 October 2026

The [registered transport control](benchmarks/icloud-flat-numbers-wire-envelope-2026-10-06.json)
models the observed loss of the outer path component. This is an inference from
the failed owned upload, not an Apple protocol guarantee. The desired complete
roundtrip first failed with typed `Conflict`; the same test source then passed
with a separate destination-name transport wrapper. Index, Metadata, Tables and
all previews must return unchanged, with exactly one allocation, body,
registration and independent verification. Strict semantic V2 was not relaxed.

The caller's sealed flat source, raw receipt and request remain unchanged.
Fresh preparation derives a bounded deterministic ZIP and records its separate
size, digest and exact root in a version-2 package checkpoint before allocation.
Streaming rederives and compares that receipt before sending a body. Existing
wrapped version-1 transport is unchanged. Legacy flat version-1 checkpoints
support read-only inspection; they cannot allocate, stream or register the old
raw source. Recovery never prepares or allocates again. Structural receipt
corruption refuses before HTTP; valid-shape size/digest tampering refuses before
stream HTTP when the source is rederived.

A separately registered offline control used the actual unchanged 138,945-byte
Apple export. Two derivations produced identical 141,475-byte transport archives,
each with exactly the source's semantic V2 content. The original bytes, mode,
identity and modification metadata stayed unchanged. This control made no cloud
calls and did not open Numbers. The inferred-model regression and offline source
check establish local transport preservation, not live provider acceptance.

The candidate journal stays at schema21 under the existing held deployment
policy. The installed packaged daemon remains unchanged. Fresh live upload,
Apple Numbers reopening, replacement/recovery endpoints and the five remaining
release criteria stay open. Earlier compile/setup failures and fixture callback
updates are retained in the same transport artifact.

The entire `bash scripts/check.sh` passed in 670.61 seconds: formatting, all
Clippy variants, workspace and optional iCloud tests, actual kernel/FUSE groups,
scripts, translations, ledger and docs. All 546 registered crate/script/Cargo/
translation/policy pins stayed unchanged. Display scenarios were not repeated.
An earlier full run missed the existing retained-status test's initial deadline;
the isolated unchanged test and subsequent entire run passed. This deadline
failure remains recorded without changing the timeout or claiming a proven cause.


### Fresh flat Numbers import and Apple open on 6 October 2026

The [separately registered fresh trial](benchmarks/icloud-flat-numbers-wire-fresh-public-cli-import-2026-10-06.json)
used the unchanged actual Apple flat export through the normal public CLI, with
`--source-layout flat-numbers` and no source root. One new owned folder and one
import were dispatched. The public CLI exited zero, its exact job succeeded,
and journal21 contained the uploaded operation and completed metadata publication.
The original warm mount and stopped independent provider read both matched the
source's complete semantic V2: 46 canonical entries, 42 files and 133,153 expanded
bytes. Index, Metadata, Tables and all three preview images were preserved.

The exact rendered Drive row's item identity matched the completion receipt
before opening it. Apple Numbers displayed the new document with A2=11, B2=3
and C2=14; selecting C2 without editing showed `SUM(A2:B2)`. The original
180-second GUI arm did not retain an individual formula-completion timestamp,
and its later screenshot file timestamp cannot establish that deadline. Its
record was preserved. A separately registered 120-second read-only observation
of the already-open document completed in 9.703 seconds with timestamped formula
and screenshot evidence, without editing, exporting, reloading or reimporting.

The original controller completed in 106.856 seconds with exit zero; all five
children exited zero and were reaped. The owned mount, control socket and buses
were absent. A subsequent read-only journal audit confirmed the same single
upload and completed publication. All 546 runtime source pins, four binary roles,
sealed source and immutable inputs remained unchanged. All 19 packaged files,
unit configuration and installed daemon process identity also stayed unchanged.

This closes no additional release criterion: it establishes one actual flat
Numbers import and Apple open. Genuine edited B replacement with current-B and
Trash-A comparison, flat checkpoint-v2 recovery after lost final confirmation,
and recoverable removal still need their own controlled acceptance arms. The
historical failed import remains failed. Candidate21/8 installation stays on
HOLD against the packaged14/7 baseline. Full iCloud acceptance remains open.

The complete `bash scripts/check.sh` passed again before publication in 526.23
seconds, with all 546 runtime source/translation/policy pins unchanged. It included
formatting, Clippy, workspace and optional iCloud tests, actual kernel/FUSE groups,
scripts, translations, ledger and documentation. Display scenarios were not
repeated. These checks support the bounded result above and leave full acceptance
open.


### Selected flat Numbers replacement read observer on 6 October 2026

The [registered observer control](benchmarks/icloud-flat-numbers-replacement-observer-2026-10-06.json)
removes a validation gap for the next genuine Numbers edit/replacement trial.
The existing feature-only observer now has explicit flat preflight/postflight
arms for an already imported owned document. A fresh observation run binds the
previous subject run, account, exact parent/item/revision and unchanged flat
source archives. Source roots must be explicitly null; downloaded current and
Trash packages retain exact provider-name wrappers and strict semantic V2.
Postflight requires distinct original/current identities, the original's Trash
revision and complete A/B contents. Legacy wrapped/Pages scopes are preserved.
The caller separately proves the stopped writer and durable receipt.

The two desired JSON-admission and valid-fixture tests first compiled and failed
against the original consumer. The same regression bodies then passed; all 14
observer tests passed, including the original eight and four additional controls
for scope, missing roots, original/Trash substitution and flat source tampering.
Three new guard signatures needed workspace formatting before the complete
check; that correction is retained in the artifact. These controls made no
provider calls. Genuine B export, actual public replacement, independent B/Trash-A
proof and recovery acceptance remain open.

The complete `bash scripts/check.sh` passed in 555.56 seconds, including all 14
observer tests again after workspace formatting, full workspace and optional
iCloud controls, real kernel/FUSE groups, script tests, translations, ledger and
documentation. All 546 pinned source/script/Cargo/translation/policy files stayed
unchanged. No cloud call, installed change or additional release criterion is
claimed by this local observer correction.


### Refused flat Numbers replacement preflights on 6 October 2026

The [registered genuine-edit trial](benchmarks/icloud-flat-numbers-genuine-editor-replacement-2026-10-06.json)
stopped before authoring B or dispatching a replacement. The first exact-original
read observer exited one after 32.546 seconds. A separate fresh attempt, explicitly
requested by the owner, exited one after 1.221 seconds. Both retained their own
attempt records, reached the document-folder query and produced no typed content
fixture. Their original processes were reaped. No GUI edit, duplicate, export or
cloud mutation occurred, and neither failed attempt was replayed automatically.

The initial local launch also exposed missing executable permissions on the
frozen test binaries. It failed before child creation or provider access; the
permissions were corrected from 0400 to 0500 with unchanged bytes before the
first actual observer dispatch. That correction is distinct from the subsequent
provider-observation refusals. The existing public error discarded the inner
failure phase, so these observations do not identify an expired session, changed
revision or network failure. The original successful import remains separate;
genuine B replacement and the five open release criteria remain unproved.


The feature-only observer now preserves a fixed failure-stage label while
immediately discarding underlying errors and context. A synthetic refusal through
the actual public observer first failed its desired stage assertion without the
change; the unchanged assertion then passed. All 17 observer tests passed,
including sensitive-context stripping across every stage and an actual stale-ETag
refusal. This improves the next diagnosis; it does not determine the cause of the
two historical refusals or establish a successful live replacement.


The entire `bash scripts/check.sh` subsequently passed in 572.77 seconds with
`RUST_TEST_THREADS=4`, including all 17 observer controls again, kernel/FUSE
scenarios, scripts, translations, ledger and documentation. All 546 registered
runtime pins stayed unchanged. An initial Clippy finding was corrected; two
subsequent default-concurrency runs missed the existing initial status-publication
wait in different retained-status tests. The unchanged isolated control and full
four-thread run passed without extending any timeout or skipping additional tests.
Those failures remain in the same artifact; default-concurrency stability is not
claimed. Desktop display scenarios were not repeated. All 19 installed files,
unit configuration and daemon process identity remain unchanged; deployment stays
on HOLD and no additional full-iCloud criterion closes.

### Current Numbers reference and flat recovery controls on 6 October 2026

A [separate read-only diagnosis](benchmarks/icloud-flat-numbers-current-reference-diagnostic-2026-10-06.json)
refused the historical parent ETag before checking document metadata or content.
It therefore establishes neither a document change nor the cause of the earlier
replacement preflight refusals. Its conditional content-acquisition arm was not
dispatched, and the closed diagnosis was not repeated.

The retained reader now has an explicit `current_snapshot` mode; omission retains
the previous changed-revision requirement. The two admission assertions first
failed against the unchanged consumer and subsequently passed without changing
their test bodies. All nine local reader controls passed, including exact expected
revision, foreign identity/account routes and expired-window refusal. A
[fresh current-reference read](benchmarks/icloud-flat-numbers-current-snapshot-2026-10-06.json)
completed once in 6.43 seconds after the full check and pinned reader build. Its
current parent and item revisions differ from the retained original, as does its
complete semantic V2 tree: 10 entries / 7 files / 138,812 expanded bytes versus
46 entries / 42 files / 133,153 bytes. Exact read fences passed; the original child
was reaped, all input/cipher/installed-baseline guards remained unchanged and no
cloud mutation occurred. This does not attribute the change to browser input or
retrospectively identify either historical preflight failure. A separate
[independent offline decode](benchmarks/icloud-current-numbers-cached-cells-2026-10-06.json)
then verified cached A2=11, B2=3 and C2=14 on the unchanged captured archive. The
importer emitted no formula; SUM and editor/export fidelity remain unproved. A
different content tree does not by itself establish changes to those cell values.
This current reference cannot stand in for a genuine edited B or browser export,
and the old original revision cannot be silently substituted as the current
precondition.

A [local flat recovery control](benchmarks/icloud-flat-numbers-final-recovery-controls-2026-10-06.json)
also first failed the intended admission assertion after actual TLS handoff,
encrypted checkpoint persistence and exclusive journal reopen. With the correction,
the unchanged test passed: the same operation completed by one inspection, with
no additional allocation, body transfer, registration, Trash or rename callback.
Both source archives stayed unchanged. This withholds a local acknowledgement
after the backend returned its final receipt; it does not simulate provider reply
loss or process death. Metadata publication and a read-only remount are outside
this control. The complete `bash scripts/check.sh` passed in 619.95 seconds with
`RUST_TEST_THREADS=4`, all 549 runtime pins unchanged, both new parser/receipt
controls, all nine reader controls and all seven native recovery runtime controls.
The initial standalone parser filter executed zero tests because it omitted the
actual module namespace; this is recorded separately and supplies no passing
evidence. Desktop display scenarios were not repeated. That control did not
acquire NativeFinal metadata from Apple; subsequent local producer admission
coverage is recorded below. Apple fault acceptance, genuine edited replacement and the five open
release criteria remain separate; installed delivery stays on HOLD.

### NativeFinal flat metadata producer admission on 6 October 2026

The [registered producer control](benchmarks/icloud-flat-native-final-metadata-acquisition-2026-10-06.json)
first failed both desired null-root admission assertions against unchanged
production code. The unchanged tests passed after the producer accepted an
explicit null source root within the existing NativeFinal namespace and required
matching A/B roots in postflight. They exercise local flat V2 proof, fixture
production and the generic consumer, including byte and mode preservation.

All 19 module tests then passed. The first whole-module run retained one obsolete
expectation that NativeFinal must reject null roots; its correction preserves
wrong-root, missing-root, foreign-subject, Pages-null and mixed-layout refusals.
The two original desired test bodies and their complete helper region are unchanged.
These are local controls, with no provider calls or mutations. Genuine editor
export, fresh Apple metadata acquisition, uncertain-operation recovery and the
five open full-integration criteria remain separate.

The complete `bash scripts/check.sh` passed in 564.20 seconds with
`RUST_TEST_THREADS=4`, all 549 runtime source pins unchanged, kernel/FUSE and
script coverage included. Desktop display scenarios were not repeated. The
previous head's CI run separately failed an existing adaptive OneDrive window-count
assertion (18 expected, 19 observed); its other six jobs passed. This local full
check does not erase that CI result or close an iCloud release criterion.

### Genuine edited flat Numbers replacement and process recovery on 6 October 2026

A [fresh donor](benchmarks/icloud-numbers-edited-b-donor-2026-10-06.json)
completed normal CLI import, original warm readback and independent full semantic
V2 verification. A single browser edit changed A2 from 11 to 17; B2 remained 3
and C2 retained `SUM(A2:B2)` with result 20. The original 300-second edit/export
arm closed partial before confirmed export. A separately registered 600-second
read-only export arm acquired genuine Numbers bytes with zero additional edits.
The unchanged 139,044-byte export passed full flat V2 proof; an independent local
importer decoded cached 17/3/20 but emitted no formula. Formula evidence comes
from Apple Numbers after reload, not from the cached-value importer.

The [separate fresh NativeFinal arm](benchmarks/icloud-flat-numbers-native-final-2026-10-06.json)
completed once in 382.98 seconds within its original 600/45-second window.
It imported a distinct protected original through the normal public CLI, acquired
actual typed Apple metadata and independently verified original A before starting
one genuine B replacement. The feature-only test process exited 86 after the
backend returned its final receipt, withholding the local acknowledgement. The
public replacement CLI exited one as expected when its socket owner disappeared;
this is not an ordinary successful CLI return or a simulated provider reply loss.

Before reopening, the stopped journal retained the exact Uploading operation,
encrypted checkpoint and sealed B, without a published terminal receipt.
Recovery reopened that same operation and completed through one provider inspection:
zero reconciliation, upload or namespace callbacks and no mutation replay.
Independent postflight verified new B and original A in Trash against their complete
46-entry/42-file semantic V2 identities and exact item/revision bindings. This
proves content-tree preservation; provider-generated ZIP bytes differ from the
flat input, and raw ZIP equality or restoration is not claimed. Ten original
children were reaped, the owned mount/socket disappeared and all runtime source
and source-archive pins remained unchanged. The independent retained audit passed
32 checks.

Apple Numbers subsequently opened the exact new item, showed 17/3/20 and
`SUM(A2:B2)`, and retained them after one reload without any cell edits or repair
dialog. Read-only remount acceptance is a separate next endpoint. This bounded
fault case closes no additional full-iCloud release row: native editing/removal,
broader application/format fidelity, installed transitions, Strata preservation
and sustained reliability remain open. Installed delivery stays on HOLD.

The [separate read-only remount](benchmarks/icloud-flat-numbers-native-final-ro-remount-2026-10-06.json)
subsequently started from a coherent copy of the closed recovered state but stopped
at mounted capture with `FileNotFoundError`, before creating an archive or running
the semantic scanner. It closed in 33.49 seconds, all original children were
reaped, and original state, completed transfer rows and installed files remained
unchanged. No automatic retry occurred; semantic remount acceptance is unproved.
An independent stopped-catalog query proves that effective name lookup selects
old A at the original parent/name while recovered B is also cached there. The
typed receipt places that same old A in Trash. This supports a metadata-handoff
routing defect; the exact failing open was not retained and is not reconstructed.
The next correction must publish the exact receipt-backed backup identity, rather
than discard items by name or reorder ambiguous lookup results.


### Native replacement metadata correction on 6 October 2026

The [registered local correction](benchmarks/icloud-native-package-backup-publication-2026-10-06.json)
first reproduced three intended failures against unchanged production: normal
package publication and journal reopen still selected old A, and actual read-only
Engine startup did not repair already completed native history. The same three
assertions now pass without changing the protected transfer row, source or sealed
payload. Eight Store controls preserve newer/negative identities, completed
cursors and observation tickets, and three journal controls validate genuine
receipt/owner authority, ordinary wire compatibility and bounded history repair.

Separate negative controls establish that an older complete directory cannot
authorize unknown B, that the namespace body's owner ID must match the mapped
SQL UUID, and that the startup history range cannot reopen at a newer sequence.
An initial cursor control stopped at an invalid ordinary fixture before its
desired assertion; that failure remains retained and is not regression proof.
The corrected fixture uses the existing ordinary working-file write/seal flow.

Native completion now publishes matching B and the exact receipt-bound A backup
in one metadata commit. Schema-21 read-only startup can admit validated historical
receipts even when their prior package publication is done. Its retained volatile
cursor processes at most 16 indexed package rows per pass up to a fixed startup
high-water mark; it does not rescan all history on each status poll. Ordinary
legacy receipts without captured originals remain excluded, and schema-20
metadata-only access does not migrate or query the schema-21 package index.

The complete `bash scripts/check.sh` command passed in 691.17 seconds with all
549 Rust/Cargo source pins unchanged. The earlier full check remains recorded:
it failed an existing ordinary corruption control when native startup high-water
selection encountered a negative SQL sequence before the bad job's normal
backoff. The same control failed before the correction and passed afterward.
Native history selection now excludes negative sequences; ordinary corruption
is still refused and deferred, and no stored sequence is normalized.

The [fresh read-only provider acceptance](benchmarks/icloud-native-package-readonly-backfill-2026-10-06.json)
ran once in its own 600/45-second window and closed after 18.51 seconds. Its
normal read-only startup repaired the copied completed history: original A matched
its receipt-bound Trash backup, and the canonical original-name lookup selected
only exact B. This adds live evidence for metadata repair, not semantic remount
acceptance.

The mounted capture then refused a changed revision. Stopped current B and its
native archive artifact bind a newer ETag and logical size (139,003 rather than
133,252 bytes). An independent offline scan of the retained 65,019-byte archive
found a different complete semantic V2 tree: 10 entries, 7 files and 139,003
expanded bytes, rather than the expected 46/42/133,252. The exact first failing
capture guard was not retained. The reference was not substituted, the live
scanner was not dispatched and this arm remains failed. Apple Numbers had been
opened between earlier postflight and remount, but these records do not establish
who or what changed the revision.

All original test handles were reaped, the mount/socket disappeared, and original
state, completed transfer rows, all 19 installed files, unit and daemon identity
remained unchanged. No cloud write or automatic remount retry occurred. The next
controlled arm will place strict remount verification before opening Apple Numbers;
it must use a new owned subject and a fresh preregistered window. Five remaining
full-integration criteria and installed delivery stay open.

### Fresh replacement and full read-only remount before Apple opening

The [first new subject](benchmarks/icloud-native-final-remount-before-apple-2026-10-06.json)
imported and independently verified its own original, then stopped before B
replacement. The private harness supplied a full UUID temporary path where the
production feature adapter requires the first eight characters. Static review
had missed that contract mismatch. The failed arm is retained; its remount was
never dispatched and no automatic replay occurred.

A [separately registered corrected subject](benchmarks/icloud-native-final-remount-before-apple-corrected-2026-10-06.json)
used the proven harness with only its fresh run identity changed. It closed in
351.39 seconds: genuine edited B replaced its owned A, the controlled process
exit occurred before the final local confirmation, and the same operation
recovered through one inspection without upload, reconciliation or namespace
replay. Independent postflight verified the full new B and original A in Trash.
The original handles were reaped and the owned mount/socket closed.

Only after that successful subject closed, a
[separate normal read-only mount](benchmarks/icloud-native-package-readonly-remount-before-apple-2026-10-06.json)
started from a derived copy of its state and closed successfully in 12.897 seconds.
Canonical lookup selected only the exact receipt-bound B at the original name;
the exact A backup metadata remained in Trash. Mounted capture and independent
offline scanning matched B's full semantic V2 identity: 46 entries, 42 files and
133,252 expanded bytes, including all three previews. Independent retained audits
passed both arms; the read-only audit passed all 16 checks.

No Apple application opening preceded this endpoint. The original closed state,
completed transfer rows, runtime source pins, all 19 installed files, unit and
daemon identity remained unchanged. This arm made no provider write and no new
Trash-content read; full original content preservation comes from the separate
native postflight. The earlier revision-drift failure remains failed and no
reference was relaxed. This closes the bounded semantic remount question, not
native Linux editor saves, broad format fidelity, installed delivery or any
additional full-integration release row.

The [separate Apple Numbers check](benchmarks/icloud-native-final-apple-after-remount-2026-10-06.json)
then opened that exact fresh B using its identity-bound route. It displayed
17/3/20 and `SUM(A2:B2)` and retained both after one reload, without cell edits or
a repair dialog. The owned test tab closed within its own original window.
Installed file hashes, unit and daemon identity were checked again afterward
and remained unchanged. Application fidelity for this fixture is confirmed;
provider serialization after Apple opening and broader native editing are not
claimed by this arm.

### Standalone native Trash recovery metadata in read-only mode

The [registered local correction](benchmarks/icloud-native-trash-readonly-publication-2026-10-06.json)
reproduced three intended failures: actual read-only Manager startup and effective
read-only startup with a recorded write grant did not finish an already Applied
native removal's pending metadata absence; observing completed history required
a writer. The same unchanged assertions pass after the correction. They preserve
an unrelated Uploading save, newer dirty working bytes, source bytes, the
synthetic opaque checkpoint marker, settings, schema and completed feed cursor.
Marker preservation is not a real encrypted provider-checkpoint proof.

Read-only maintenance now handles one handoff and one standalone-removal job per
pass. It releases the metadata-only journal owner before the configured-drive
exact-ID read and finishes only ordered absence. Newer restored observations
remain visible; invalid jobs receive bounded cooldown so valid siblings progress.
Historical observers use a read-only recovery lease, with no admission, writer
construction or provider mutation. Completion records the old receipt and absence
observation, not the current state after restoration.

Four mapped SQL/body authority controls and two impossible Applied acknowledgement
controls first failed, then passed with protected transfer data unchanged. Two
follow-on scope controls exposed issues in the draft helper and then passed:
foreign provider/collection jobs now cool down without provider reads. The owner
release/restoration race control passed before those scope changes and is not
claimed as a new regression. The extended native-removal group passed all 47
tests, including schema20/21 existing-job repair, missing-table compatibility,
older-schema refusal and immutable completion checks.

This is controlled synthetic evidence. Live standalone native removal interrupted
before its local acknowledgement still needs its own exact-operation,
inspection-only recovery and full Trash-content arm. These changes close no
additional full-iCloud release row and do not authorize installed migration.

The complete `bash scripts/check.sh` passed in 736.34 seconds, including
formatting, Clippy, workspace and required feature tests, actual kernel/FUSE
groups, script checks, the ledger and documentation. All 552 registered runtime,
source, fixture and script pins remained unchanged; the original process handle
was reaped and absent. Display scenarios are outside that command. Independent
code and evidence reviews found no remaining concrete inconsistency. This is
local development validation, with no real-account mutation or installed action.


### Standalone native Trash receipt-loss guard

The [registered local trial](benchmarks/icloud-native-trash-receipt-loss-guard-2026-10-06.json)
now exercises the actual MutationWorker in a child process against a local TLS
provider fixture. The normal native Trash adapter stores a real encrypted
MayHaveSent checkpoint, sends one Trash request and returns its genuine Removed
receipt. A feature-only guard then exits the process with code 86 before the
worker acknowledges that receipt locally. The parent reaps that exact child and
checks the raw journal body, indexed columns and incomplete queue.

Reopening the same operation changes only its interrupted state and attempt.
Recovery performs one reconciliation and no prepare, mutation or upload call;
the TLS server still records exactly one Trash request. The original source and
checkpoint ciphertext remain intact. All nineteen guard, registration, observer, metadata-collector
and actual-child controls passed. This models process loss after a received
provider response; it does not model a lost provider response or establish
real-account reliability.

The finite live driver binds a fresh Numbers import, its original provider
identity and revision, the immutable flat source and complete semantic V2 proof.
An explicit read-only preflight bridge accepts this new test namespace while
keeping existing fixture validation strict. Its three account/scope/content
controls passed. A separate saved-session-only collector now acquires fresh
typed parent/document metadata, verifies the exact PACKAGE content and fences
metadata, settings, source and the original deadline again before exporting
its proof. All three collector admission controls passed. Actual fresh typed
provider metadata remains a live endpoint; new executable bindings and a
complete overall check were required before real-account dispatch. The
first overall check that reached script smoke tests stopped because its disk
temporary path exceeded the Linux UNIX-socket length limit; the unchanged-source
failed result remains recorded. This work closes no additional release gate and
does not change the installed daemon.

The complete successor `bash scripts/check.sh` passed in 653.92
seconds under the shorter ext4 temporary path, with all 561 registered
runtime/source/fixture/script pins unchanged. Its original process handle was
reaped and absent. Formatting, Clippy, workspace and required feature tests,
actual kernel/FUSE scenarios, scripts, the ledger and documentation passed;
display scenarios remain separate.

The [first once-only real-account attempt](benchmarks/icloud-native-trash-numbers-receipt-loss-2026-10-06.json)
then stopped at the private controller's startup status guard before folder
creation, import or native Trash admission. No cloud-file mutation was
dispatched; provider reads may have occurred and were not counted. The actual
status operands were not retained, so an initially unpublished account vector
is a source-supported explanation, not an observed cause. The original failed
cleanup record is preserved: the daemon was killed after its short shutdown
wait and left an owned mount. All original handles were reaped; Root separately
verified and unmounted that exact dead mount, then verified all source and
executable pins and the unchanged nineteen-file installed baseline. The
[closed window](benchmarks/icloud-native-trash-numbers-receipt-loss-window-2026-10-06.json)
records both outcomes. No receipt-loss, recovery, Trash-content or read-only
remount endpoint was exercised, and the closed trial will not be replayed.

The [source-selected startup controls](benchmarks/icloud-native-trash-controller-startup-2026-10-06.json)
reproduced two controller defects on the old source: refusing the legitimate
empty initial account list, and failing to capture an already-present owned
mount/socket before a status refusal. Both controls failed on V4 and passed on
V5; all seven V5 controls passed, including refusal of foreign scope, multiple
accounts, a changed binary and a wrong protocol. These are pure Python controls
with mocked process, socket and filesystem facts. They neither identify the
unretained status operands from the live failure nor prove real daemon shutdown;
late mount/socket publication during shutdown remains a separate controller
frontier. Its subsequent desired V5 control failed, while the V6 successor
passed that same control, all six closure controls and all seven startup
controls. The first V6 fixture omitted a mocked `RUN` global and raised a
NameError; that setup failure remains recorded alongside the corrected
controls. No controller guard was relaxed and no daemon runtime proof is
claimed from these pure controls.

The [fresh successor real-account arm](benchmarks/icloud-native-trash-numbers-receipt-loss-attempt2-2026-10-06.json)
ran for 43.39 seconds. Startup passed and one owned folder creation completed.
It then stopped at the public import-job composite guard before any upload was
enqueued. The raw journal retains exactly that Applied folder mutation and its
completed queue entry, with zero uploads; no native Trash admission occurred.
The normal daemon exited zero, the original import observer was stopped, all
original handles were reaped, and the owned mount/socket were absent. All 561
source pins, compiled and fresh role hashes and the nineteen-file installed
baseline matched. The [closed successor window](benchmarks/icloud-native-trash-numbers-receipt-loss-attempt2-window-2026-10-06.json)
preserves the actual failure and closure. Its owned test folder is retained.

The status operands at refusal were not retained. Source review confirms that
the guard accepts the valid initial Running job with the exact requested name,
no issue and no durable operation yet. Pre-enqueue admission can fail without
an upload row; the evidence does not establish which guard operand failed or
which admission stage caused it. The diagnostic controller successor records only
sanitized guard facts and fixed issue categories, preserving refusal and
no-retry behavior. This arm closes no additional release criterion and is not
automatically repeated.

The diagnostic successor passed all three source-selected controls: the valid
initial Running job is accepted, a failed admission remains refused after its
sanitized snapshot, and untrusted issue/name text is omitted. Independent
reviews also found a concrete difference from the previously successful parent
admission: an Applied folder receipt can precede the namespace handoff.
The successor waits for the exact owned operation/receipt/namespace binding,
`follows_remote=true` and `latest=None` before the sole import submission. Its
desired predecessor control failed; the same successor control and all five
readiness controls passed. This justifies the added readiness fence without
establishing the cause of the unobserved live refusal.
The same selected readiness method also passed against the actual closed
successor journal, returning true with its strict SQL/body/owner bindings.
Database, WAL and SHM bytes remained unchanged. This later read-only snapshot
does not establish readiness at the time of the original refusal.

The complete documentation-successor `bash scripts/check.sh` passed in 556.16
seconds, with all 561 runtime/source pins unchanged and its original handle
reaped and absent. Implementation commit `670cb88` also passed all seven
[GitHub CI jobs](https://github.com/Dandiccf/cirrove/actions/runs/37497027629).
Neither result closes standalone real-iCloud removal acceptance or permits
installed migration.

The [next once-only real-account arm](benchmarks/icloud-native-trash-numbers-receipt-loss-attempt3-2026-10-06.json)
stopped after 28.40 seconds, before import or native Trash admission. Local mkdir
returned, but the sole folder mutation reached `VerifyRequired` without a recorded
attempt or receipt; its queue entry is incomplete. This is an uncertain cloud
folder outcome, not a confirmed creation or confirmed failure. The raw frontier
contains zero uploads, and no import or Trash command was started.
The normal daemon exited zero; all original handles were reaped, the owned
mount/socket were absent, and the source, executable and nineteen-file installed
baselines matched. The [closed window](benchmarks/icloud-native-trash-numbers-receipt-loss-attempt3-window-2026-10-06.json)
preserves that unresolved outcome. Its cause and cloud presence or absence are
unproved; provider reads were not counted. The exact operation remains retained
without requeue, retry or replay. No removal, recovery, Trash-content or read-only
remount endpoint was exercised, and no additional release criterion closes.


The separately registered [local checkpoint inspection](benchmarks/icloud-folder-create-checkpoint-inspection-2026-10-06.json)
then authenticated the two exact retained ciphertexts. Its accepted second local
read classified the outer plan as Sent and the inner checkpoint as Created, with
the operation-bound folder ID. The helper exited zero in 0.124 seconds, was
reaped, and left all thirteen state files unchanged. It made no HTTP/provider
request and opened no writer journal. The folder mutation still has no recorded
typed Upsert receipt and remains `VerifyRequired`; current cloud presence and
the cause of its uncertainty are unproved. No acknowledgement, replay, import
or Trash request followed, and no acceptance gate closes.

The first local read also exited zero in the helper, but its receiver rejected
the output and did not retain the classification. Actual helper-codec fixtures
subsequently showed that the old receiver rejected valid uppercase UUIDs; the
corrected receiver passed the same desired case and thirty scoped controls.
That compatibility defect is proven locally, while the first real rejection
operand remains unretained. Both local reads have closed without extending the
original cloud window.


A separately registered [exact-ID read-only inspection](benchmarks/icloud-folder-exact-id-read-2026-10-06.json)
then ended in 0.950 seconds: the helper exited zero, but the outer stream/result
guard raised AssertionError before retaining a classification. The original
helper handle was reaped and absent, all thirteen state files and all 561 current
source pins were unchanged, and no timeout or cleanup signal occurred. The
observation was stopped without a second read, journal acknowledgement or replay;
current cloud presence remains unverified.

Static review found a concrete stream-contract mismatch: the pinned iCloud
dependency has `write-probe` enabled and its direct root listing emits a fixed
phase-timing line to stderr, while the outer guard demands empty stderr. The
actual streams were not retained, so this source finding does not establish the
actual failed operand or recover the cloud classification. Local stream controls
and real-cloud acceptance remain distinct; this observation closes no release
criterion.

The current successor passed the complete `bash scripts/check.sh` in 554.65
seconds, including workspace and actual FUSE groups, with all 561 current source
pins unchanged and the original handle reaped and absent. The separately
controlled Strata companion GUI arm passed in 9.427 seconds with exactly its
three synthetic actions and no network access. Neither result establishes
current presence for the unclassified folder inspection or closes an additional
full-iCloud release criterion.

The local stream correction was then verified with the actual linked iCloud
dependency: an unconfigured session refused its root listing before HTTP while
emitting the fixed timing line. The source-selected V4 success branch failed
the desired control; V5 passed that same control and all 26 stream controls,
including the four actual synthetic Rust codec forms and hostile outputs.
The receipt validator is unchanged; only one bounded fixed-format timing line
is accepted. All six local children were reaped and all 561 source pins and
thirteen dependency pins matched. V5 has not performed a real read, and the
earlier cloud classification and failed operand remain unretained.

A [fresh current-source exact-ID read](benchmarks/icloud-folder-exact-id-current-read-2026-10-07-810056f7-4d38-4d0f-8c38-a4131b428e82.json)
then observed the exact owned folder currently present with its registered parent
and name, returning `Applied`. Its independent audit passed all 20 checks;
all thirteen stopped state files and 568 current source pins stayed unchanged,
and the original handles closed. No mutation or journal acknowledgement occurred;
HTTP request counts were not instrumented. This verifies current presence only:
the original `VerifyRequired` journal and first failed read retain their historical
outcomes, without acknowledgement, replay or retrospective completion. Row490
remains open.


### Fresh standalone native Trash recovery on 6 October

The separately registered [fresh removal arm](benchmarks/icloud-native-trash-fresh-receipt-loss-2026-10-06.json)
passed in 237.67 seconds within its original 600-second window and 45-second
cleanup reserve. It created a new owned folder and imported one flat Numbers
archive through the normal public CLI before submitting one public native Trash
request. A genuine Removed result was withheld by process exit 86 before local
acknowledgement. The retained operation was still incomplete at that boundary;
the public Trash CLI exited one and did not claim completion.

Inspection-only recovery used the same operation UUID, with one inspection and
one reconciliation, zero preparations and zero mutation callbacks. Independent
verification then matched the complete semantic V2 tree in Trash: 46 entries,
42 files and 133,252 expanded bytes. The original item was absent before and
after verification. This compares complete content, including the previews;
it does not claim raw Trash ZIP equality with the source archive.

Normal read-only public listing and watch, followed by mounted original-path
absence, passed without changing the completed journal frontier. All thirteen
original managed handles were reaped and their processes were absent; the owned
mount and socket were gone. Source pins, nineteen installed artifacts, the
installed unit and daemon identity remained unchanged. An independent terminal
audit passed all sixteen checks.

This is one bounded removal and recovery arm, with controlled local
acknowledgement loss rather than an unobserved provider reply. HTTP request counts
were not instrumented. It neither resolves nor replays the earlier 175 folder
operation, and it does not establish broader native editing, conflict refusal,
sustained reliability or installed acceptance. Full-iCloud row 486 remains open;
one of the six release criteria is closed and five remain open.


### Mismatched-selection arm stopped before replacement

The [fresh mismatched-selection arm](benchmarks/icloud-native-replacement-mismatched-selection-2026-10-06.json)
stopped with exit one after 105.24 seconds. The owned folder, normal public flat
Numbers import and typed independent metadata acquisition completed. Before any
replacement request, the controller raised OSError while moving a local proof
file into its retained readback directory. The errno was not retained, so the
specific filesystem cause is unproved.

No replacement request or mismatched-selection refusal endpoint was exercised;
the planned refusal preservation comparison and final independent postflight
therefore remain unvalidated. All six original managed handles were reaped and
absent, the owned mount/socket were gone, source pins matched, and the nineteen
installed artifacts, unit and daemon identity were preserved. The first local
check's setup refusal and the corrected second local check's pass remain separate
pre-dispatch outcomes. This stopped arm is retained without automatic retry.

The preceding complete `bash scripts/check.sh` passed in 548.12 seconds with
all 561 source pins unchanged and 87 test groups reporting no failures. It was
not a display rerun. That repository validation does not turn the partial live
arm into conflict-refusal or installed acceptance: row 486 remains open, with one
of six full-iCloud release criteria closed and five open.

### Fresh mismatched-selection refusal

The separately registered [fresh successor](benchmarks/icloud-native-replacement-mismatched-selection-fresh-2026-10-06.json)
passed in 120.74 seconds within its original 600/45-second window. After new
owned-folder setup, normal public flat Numbers import and independent reference
acquisition, one public replacement request used the actual item ID with a
deliberately wrong non-wildcard ETag. The CLI exited one as expected; its unique
job failed at selected-document resolution, with no native replacement progress
or durable replacement operation admitted.

Exact journal rows and the fixed private payload inventories remained unchanged.
Stopped independent postflight preserved the complete original Node, actual ETag
and full semantic V2 content. All nine original managed handles were reaped and
absent, the owned mount/socket were closed, and source pins plus the nineteen
installed artifacts, unit and daemon identity matched. The independent terminal
audit passed all sixteen checks.

The same genuine B archive supplied both the initial import and the refused
replacement; this adds no editor-fidelity result. It tests deliberately mismatched
selection, not a naturally stale previously valid revision or a concurrent
same-name occupant. HTTP counts were not instrumented. The earlier 8d650 partial
arm and its unknown errno remain separately retained without replay. Row 486
stays open, with one full-iCloud release criterion closed and five open;
installed delivery remains held.

The reserved recovery-name guard also has a new [controlled TLS regression](benchmarks/icloud-native-recovery-name-occupant-controls-2026-10-06.json). Omitting only that name from the foreign-occupant guard made the exact test fail before its expected refusal; restoring the unchanged guard made it pass, with original, staged and occupant metadata preserved and zero Trash/rename calls. These are synthetic responses, not a live competing-name or atomic-race result.

The complete `bash scripts/check.sh` subsequently passed in 621.01 seconds, with 87 completed test groups and no failures, including formatting, Clippy, workspace/feature tests, kernel/FUSE groups, scripts, ledger and documentation. The formatter only expanded the new test's final equality assertion; all 561 after-format source pins were preserved through closure. The original process was reaped and absent. Display scenarios were not repeated, and this source validation does not close live conflict or installed acceptance.

### Controlled preparation for a competing recovery name

The isolated write probe now has an optional pause at the persisted
`handoff-move-old-armed` checkpoint, before committing the original to Trash.
It is disabled by default. An explicit registration can request up to 120
seconds within the original operation deadline. Releasing it requires the same
run, operation, attempt and checkpoint digest in a complete private regular
file; cancellation, expiration or a changed journal frontier refuses release.
The adapter performs its fresh preflight after release.

The [actual controlled test](benchmarks/icloud-native-pre-trash-pause-controls-watchdog-2026-10-06.json)
first omitted only the nonblocking-open flag. Exactly one FIFO test failed at
the intended assertion after an independent OS-thread watchdog released and
joined the worker. Restoring the flag passed that test and all seven TLS
worker/checkpoint controls. Five additional local tests passed for the
read-only observer that binds original, stage and competitor identities,
revisions, completed import authority and complete journal queue ownership.
The independent terminal audit passed 54 checks; all seven original managed
handles closed, and the registered production source was restored.

Three [offline controller controls](benchmarks/icloud-native-pre-trash-controller-controls-2026-10-06.json)
also passed for completed competitor-import binding, atomic release-file
publication and expiration refusal. These controls do not execute the full
cloud controller. Earlier compilation, fixture-calibration and timeout failures
remain in their own artifacts and are not treated as the intended failing arm.

These synthetic results alone establish no live competing-name preservation.
The pause tests a competitor present before the fresh preflight; it cannot
establish atomic protection from a change after that final preflight. Row 486
and the five remaining release criteria stay open; installed delivery remains
held.

The observer's final synchronous disk publication now checks the original
monotonic deadline before and after recording its result. An actual
[disk control](benchmarks/icloud-native-pre-trash-observer-deadline-controls-2026-10-07.json)
showed that omitting only the final check accepted a late publication and failed
the intended assertion. Restoring it passed the same test, both publication
controls and all seven observer controls. All 563 registered inputs were
restored and the seven managed handles closed. A late private result file can
remain as evidence, but the observer returns refusal rather than success.

The [controller correction controls](benchmarks/icloud-native-pre-trash-controller-corrections-fresh-2026-10-07.json)
also established two intended failing endpoints followed by nine passing
controls: incomplete marker observation and enforcing the original pause bound
at actual process dispatch. The preceding fixture setup error is retained in
its separate artifact. A [lifecycle control](benchmarks/icloud-native-pre-trash-lifecycle-ownership-controls-2026-10-07.json)
used actual small local children to show the missing-pidfd cleanup failure in
both dispatch paths, then passed both corrected controls, including natural
exit without signals. Individual fixture PID records were not captured; the
tests retained and waited on their original process objects. These are isolated
test-controller checks, with no iCloud request or installed service change.

The final [complete project check](benchmarks/icloud-native-pre-trash-full11-check-2026-10-07.json)
passed in 550.83 seconds with 87 Rust test groups and no failures. Formatting,
Clippy, workspace/feature suites, kernel/FUSE tests, script tests, ledger and
documentation completed; all 563 registered inputs stayed unchanged and the
original check process was reaped and absent. It includes all seven pause and
seven observer tests on the final source. Desktop display scenarios were not
repeated. This supersedes the earlier full10 source validation for the observer
deadline fix; neither check establishes a new live or installed result.


### Real competing-name refusal and three-item preservation

A [fresh owned Numbers trial](benchmarks/icloud-native-pre-trash-recovery-name-occupant-fresh-2026-10-07.json)
passed in 299.64 seconds. A normal public import established original A. The
public B replacement reached its durable pre-Trash pause; a distinct normal
Cirrove instance then imported B once at the exact reserved recovery name.
Read-only capture bound all three typed IDs, revisions and locations before
one release. The same replacement became Conflict, with no current/recovery
or package-completion receipt; the public replacement CLI returned the
expected failure rather than reporting successful replacement.

After both writers stopped, the independent observer downloaded all three
packages and verified full canonical V2 A/B/B content against the immutable
source exports. All captured IDs, revisions and locations remained equal.
Remote transport archives have different raw bytes; the result is full
semantic identity, not raw ZIP equality. All ten inner process handles and
both outer command handles closed, both mounts and sockets were absent, all
563 source hashes stayed unchanged, and installed artifacts remained intact.
HTTP counts are uninstrumented. This proves this exact competitor present
before final preflight; it does not establish a server-side atomic race guard.

The [preceding partial arm](benchmarks/icloud-native-pre-trash-recovery-name-occupant-2026-10-07.json)
confirmed original A but stopped at a local test-controller file-mode guard
before replacement. Inspection also found a latent wrong metadata-store path.
Both failures and the byte-exact closed-database RED/GREEN controls remain
recorded. The fresh arm uses a new folder and operation identities, the
account-scoped store, and umask077 from its first process launch; it does not
replay the earlier arm. Original, stage and competitor evidence is retained;
no abandoned-stage cleanup or GUI fidelity claim follows. Row 486 and all five
remaining full-iCloud criteria remain open; installed delivery stays on HOLD.


After recording the live and partial-arm evidence, the [complete precommit
check](benchmarks/icloud-native-pre-trash-full12-check-2026-10-07.json) passed
in 539.56 seconds: formatting, Clippy, workspace/feature tests, kernel/FUSE
scenarios, scripts, ledger and documentation, with all 87 Rust test groups
passing and all 563 source hashes unchanged. Both independent live-case audits
also passed: sixteen bounded provider-proof checks and 96 terminal/closure
checks. Display scenarios, installed migration and full-iCloud acceptance
remain separate.


The [owner-readiness test correction](benchmarks/icloud-reauth-owner-readiness-ci-2026-10-07.json)
requires the actual account lease to release, alongside Manager withdrawal,
before inspecting retained edits or completing simulated sign-in. A controlled
held lease showed the intended old-algorithm failure; the restored control,
three coupled tests and complete project check passed. This changes test
synchronization, with no new live reauthentication or installed claim.

The explicit [Pages PACKAGE replacement observer](benchmarks/icloud-pages-replacement-observer-controls-2026-10-07.json)
now checks completed journal, queue and publication authority before independently
reading current B and original A in Trash. Eight synthetic Pages controls and all
eleven existing Keynote controls passed. A Keynote-only admission counterfactual
failed the positive Pages test; omitting the SQL tuple guard failed the test that
rejects a borrowed completed receipt. Both exact tests passed after byte-exact
restoration. The first counterfactual demonstrates the newly added Pages contract,
not a previously shipped Pages defect. The complete project check is recorded in
the same artifact. This prepares a fresh genuine Pages replacement workflow;
no new live Pages result, DATA coverage or full-iCloud criterion is claimed here.

The complete Pages-observer `scripts/check.sh` passed in 555.04 seconds with
87 Rust test groups, kernel/FUSE scenarios, scripts, ledger and docs, preserving
all 568 source hashes. An earlier complete command stopped at the smoke test
because the chosen temporary path exceeded the Unix socket limit. Its failure
and closed owners remain recorded; a shorter disk-backed temporary directory
allowed the unchanged source to pass the complete command from the beginning.
No new live Pages or installed acceptance follows from this local check.

### Genuine Pages PACKAGE replacement, remount and Apple reopen on 7 October 2026

A [fresh native Apple Pages donor](benchmarks/icloud-pages-edited-source-2026-10-07-80e1cb43.json)
produced genuine A and edited B exports. The originals stayed unchanged. Separate
transport archives removed only two validated redundant local size-mirror fields
per source; production flat-original and wrapped-transport semantic V2 identities
matched. This explicit transformation does not establish direct import of an
unmodified native Pages export.

The separately registered [public PACKAGE replacement](benchmarks/icloud-pages-genuine-replacement-2026-10-07-96aa715d.json)
imported A once, replaced it with B once, and independently verified the exact new
current identity/content and the same original A in Trash. The original run closed
in 289.13 seconds without automatic replay. Its stopped journal, completed queue,
typed receipts and namespace publication agreed. A [fresh normal read-only
remount](benchmarks/icloud-pages-readonly-remount-2026-10-07-7f9c92b1.json) then
returned the complete B content tree, preserving the original state and transfer
frontier. These are semantic content comparisons; differing ZIP encodings do not
establish raw archive-byte preservation.

[Apple Pages opened the exact receipt-bound replacement](benchmarks/icloud-pages-apple-reopen-2026-10-07-60aa8a2c.json)
from its verified Drive item and parent. Its genuine 93-byte B paragraph matched
before and after one actual reload. Both owned tabs closed inside the original
window, and independent source, replacement, remount and UI audits passed. No new
Trash read was inferred from the remount, and no post-Apple unchanged revision or
archive claim follows from the UI check.

This completes one genuine Pages text/PACKAGE replacement workflow using wrapped
input. That arm alone does not establish direct native export input. The wider
DATA/editor/formatting/media matrix, installed upgrades and remaining full-iCloud
criteria stay open. The installed service was not changed.

### Direct native Pages source and completed bounded workflow on 7 October 2026

The development source now adds explicit `flat-pages` input for the normal CLI
import and replacement paths. Use `--source-layout flat-pages` instead of
`--source-root` for an original Apple Pages export with no enclosing document
folder. The destination must be a `.pages` document; replacement still requires
the exact selected identity and revision of an Apple-confirmed PACKAGE. Wrapped
Pages archives and explicit flat Numbers input retain their existing contracts.

Cirrove captures the original source bytes unchanged, checks their complete V2
content identity, and generates a separate bounded upload envelope with the same
content identity. It persists both receipts for restart validation; changing the
source, wire, format or checkpoint does not authorize a replacement. This is an
explicit import/replacement path, not general editing of generated archive
children or an ordinary Pages editor save on Linux.

The [registered local controls](benchmarks/icloud-direct-flat-pages-local-controls-2026-10-07.json)
retain their actual outcomes and earlier fixture/compile failures. A subsequent
[fresh normal CLI trial](benchmarks/icloud-pages-direct-flat-replacement-2026-10-07-ab8555d8.json)
imported genuine A and replaced it with genuine B directly from the unchanged
flat Apple exports. Independent complete semantic V2 reads verified all twelve
files in current B and original A in Trash. No local size-mirror normalization
was applied to these source exports; the separately derived transport envelope
remains distinct from the protected raw source.

A [fresh normal read-only remount](benchmarks/icloud-pages-direct-flat-readonly-remount-2026-10-07-805f4b1e.json)
returned the complete B content tree before Apple opening.
[Apple Pages then opened the exact receipt-bound replacement](benchmarks/icloud-pages-direct-flat-apple-reopen-2026-10-07-3a054fad.json)
and retained the genuine 93-byte paragraph before and after one actual reload.
All three bounded arms closed and passed independent audits. Semantic content
identity is established; identical remote ZIP encoding, a post-Apple unchanged
revision, general formatting/media fidelity and native Linux Pages saves are not.
Installed delivery remains on HOLD and the complete native-document matrix stays open.

### Saved-session lifecycle observation on 7 October 2026

The [first isolated trial](benchmarks/icloud-saved-session-real-lifecycle-2026-10-07-ebbd9f71.json)
completed the genuine saved-session connection but refused during default read-only
startup before Ready. Its exact failing startup operand was not retained; all
original processes closed and the remaining five phases were not run. Pure
source-selected controls subsequently demonstrated initial-empty-status waiting,
strict scoped Ready and the original startup deadline, without attributing the
historical failure to a particular operand.

A [separately registered existing-account follow-up](benchmarks/icloud-saved-session-existing-account-2026-10-07-b8483086.json)
closed successfully in 21.04 seconds under its own window. It did not reconnect or
repeat the prior trial. Its five new helper phases passed disabled write opt-in,
creation of unattempted sealed/dirty ordinary-file generations, same-account
saved-session reauthentication, read-only downgrade and exact offline export.
Both normal read-only daemon starts reached Ready; retained journal and payload
proofs stayed equal. The 5,658-byte sealed and 5,701-byte dirty exports matched their
registered hashes. No writable daemon/upload worker was started. Original child
handles closed, and the nineteen installed artifacts, unit and daemon birth stayed
unchanged. This does not prove fresh password/2FA, expiry recovery, installed
upgrade/downgrade, or general repeatability; HTTP counts remain uninstrumented and
row488 remains open.

### Offline Pages-to-Writer loading and text conversion on 7 October 2026

The [separate diagnostic trial](benchmarks/icloud-pages-writer-source-load-2026-10-07-30098796.json)
used the protected genuine Pages B export with the installed Writer import filter.
The loader returned no document; text-service and read-only property getters were
therefore not reached, and no DOCX export occurred. Its independent audit passed
27 checks, with all owned processes closed and the original source unchanged.
This specimen could not be imported in this environment; the underlying null-load
cause remains unproved, and this is not a claim that every Pages format is
unsupported. The import/export workflow did not complete; fidelity was not measured,
and row487 remains open.

A [separately registered direct parser trial](benchmarks/icloud-pages-direct-parser-2026-10-07-61778150.json)
then read the same unchanged source with installed `pages2text`. It exited
successfully and returned exactly the registered 93-byte paragraph plus one
newline, with empty stderr. The source and executable/library pins were unchanged,
and the original isolated child closed. This narrows the investigation to the
Writer loading path; direct text extraction supplies no DOCX, native save or
formatting/media fidelity proof.

A [fresh UNO service probe](benchmarks/icloud-pages-uno-filter-probe-2026-10-07-697c1f07-ba06-40c4-a420-e3b7340c3177.json)
confirmed the registered Pages filter properties but stopped when constructing
its service. Dependency inspection identified the host's missing `libwpg` library;
that probe performed no document load or export. A
[separately registered private-library probe](benchmarks/icloud-pages-uno-private-library-2026-10-07-8e73eb0d-fe0e-497b-9a81-ad266a96d4d5.json)
used an exact library capsule verified from a signed official package. It passed
service construction, source stream length/seekability and Pages type detection
for the same unchanged specimen. Its independent audit passed 21 checks. Service
and detection success alone did not establish document load or conversion.

The [fresh conversion trial](benchmarks/icloud-pages-writer-private-library-2026-10-07-2f2fec37-6b7c-40aa-b8db-49fa98d5192c.json)
then used that private capsule to import genuine Pages B read-only, export exactly
one DOCX, close it and reopen the DOCX read-only. Both actual Writer observations
matched the registered 93-byte text as one paragraph; independent ZIP CRC and
OOXML inspection agreed. The independent terminal audit passed all 24 checks.
The original source and executable/library pins stayed
unchanged, all owned processes closed without signals, and the run completed in
0.805 seconds. The earlier failed arms retain their outcomes; the exact historical
null-load operand is not retroactively supplied by this successful arm.

This establishes one offline text-conversion endpoint, not fonts, layout, media,
native Pages saving, general format compatibility or GUI acceptance. The host
`libwpg` dependency remains absent; no system package, installed Cirrove service
or provider state was changed. Row487 and installed delivery remain open/on HOLD.

### Offline Keynote-to-Impress text conversion on 7 October 2026

A [fresh offline Impress trial](benchmarks/icloud-keynote-impress-pptx-2026-10-07-20d21b4e-a918-41db-b7f5-e1a0fb3c593c.json)
loaded the unchanged genuine Keynote B export read-only, exported exactly one
PPTX and reopened it read-only. The actual import and reopen retained the exact
registered title and subtitle on one slide. Independent inspection checked all
14 ZIP member CRCs and the exact slide XML paragraphs; the terminal audit passed
all 28 checks. The outer run completed in 0.905 seconds, with all owned handles
closed, no signals, and source bytes, protected stamps and tool pins unchanged.

This is a headless, network-isolated text-conversion result. It does not establish
layout, fonts, media, transitions, animations, native Keynote saving, GUI or
installed acceptance. Row487 remains open; no provider or installed state was
changed.

Development commit `56a2496a` passed all seven CI jobs. That source/CI
result does not deploy the candidate or change the installed HOLD.


### Bounded native replacement and removal criterion closed on 7 October

Full-iCloud row486 is closed by genuine edited Pages, Numbers and Keynote
archive replacements with exact typed old/new/recovery identity and revision
bindings, independent complete source-B/current and source-A/Trash semantic V2
verification, real mismatched-selection and reserved-name Conflict refusal,
and actual process exit86 followed by same-operation inspection-only recovery
without mutation replay. The separate standalone native Trash arm also passed
complete original-content verification and normal read-only list/watch/mounted
absence. The linked registered arms above retain their original outcomes,
failed predecessors and independent audits.

This closes the bounded explicit native archive replacement/removal workflow.
It preserves complete original file contents, not raw transport ZIP encoding,
and proves local acknowledgement loss rather than an unobserved provider reply
loss. Arbitrary generated-child writes, after-final-preflight atomic races,
natural stale-revision acceptance and sustained reliability are not inferred.
Rows487–490 remain open for the application/representation matrix, installed
lifecycle, full Strata delivery and remaining reliability boundaries. The current
count is two of six criteria closed and four open; installed delivery stays on HOLD.

### Actual mounted Writer DOCX create/save on 7 October 2026

The [separately registered Writer trial](benchmarks/icloud-writer-mounted-docx-2026-10-07-f015bd4d-44a1-4676-88ca-a0a0445ed9ab.json)
used the completed genuine Pages-to-DOCX conversion as an unchanged seed. Writer
loaded a private writable copy, called `storeAsURL` once into the fresh isolated
iCloud mount, and reopened A read-only with the exact 93-byte paragraph. It then
loaded mounted A, appended the registered own marker, called `store` once and
reopened B read-only with the exact 158-byte paragraph. Both actual ZIP archives
passed all member CRCs and independent OOXML paragraph checks.

Independent iCloud DATA readers matched the complete 5,829-byte A and 5,906-byte B
sources. A normal read-only remount returned exact B bytes and the selected current
Node. A separate typed Trash observer verified the completed replacement's original
A identity, revision, size and raw hash. The complete stopped journal frontier
stayed unchanged across the read-only remount and Trash observation; no mutation
was retried. The observer did not retain another downloadable ZIP for separate CRC
inspection, and provider HTTP counts were not instrumented.

The controller completed in 472.41 seconds within its original 600-second window
and 45-second cleanup reserve. All twelve controller-owned children and both outer
original owners closed; mounts and sockets were absent, with no cleanup error.
The independent retained-evidence audit passed 29/29 checks. All bound sources,
role binaries, Office modules and protected seed stamps remained exact, and the
nineteen installed artifacts, service unit and daemon birth stayed unchanged.

The feature proof binaries were freshly built from the formatter-final568 source
map, before commit `19f604d`, from `97bee0e` plus its exact tested pending changes.
The normal read-only daemon remains the distinct frozen `56a2496a` role. The Writer
application ran headless with network access disabled; its authorized mounted
saves used the host daemon's provider operations. This is one bounded ordinary
DOCX create/save workflow, not native Pages saving, fonts/layout/media fidelity,
GUI, sustained reliability, installed delivery or full row487 closure. The four
remaining acceptance criteria and installed HOLD remain open.

### Explicit recovery-only startup controls on 7 October 2026

The [registered recovery-only controls](benchmarks/icloud-recovery-only-startup-2026-10-07.json)
exercise the actual Manager with a saved writable synthetic iCloud account, an
uncertain sealed generation, a newer dirty working generation and a genuine
completed handoff whose ordinary metadata publication remains due. An explicit
`cirroved --recovery-only` run keeps desired settings and credentials intact,
restricts the effective mount policy to read-only, and exposes local recovery
without starting writers or repairing retained journal publications. The same
restriction applies to remount retries, manual disable/enable and interrupted
sign-in healing.

The actual Manager and CLI controls passed, together with sixteen existing
recovery, five retained-status and three reauthentication controls. Omitting the
run-wide selection caused the intended WriteFactory assertion to fail; omitting
only the remount publication guard caused the intended pending-publication
preservation assertion to fail. Each executed exactly one test and passed again
when its exact source bytes were restored. All nine original test-process owners
closed, and all569 source paths, including the new fixture module, were restored.

The Manager fixture exports exact sealed and dirty bytes and compares every
journal table/DDL and retained file against its baseline. It deliberately uses
DELETE journaling; this proves neither installed WAL coordination nor migration,
real credentials, a live provider, FUSE readiness or row488 acceptance. Recovery
mode still permits provider reads and ordinary index/cache work. It preserves
saved write grants, so a later normal start without the flag can start writers.
The [complete composed check](benchmarks/icloud-impress-recovery-complete-check-2026-10-07.json)
records whole-project validation separately; installed HOLD remains in force.

### Actual mounted Impress PPTX create/save on 7 October 2026

The [separately registered Impress trial](benchmarks/icloud-impress-mounted-pptx-2026-10-07-85ee1211-763d-46b7-b7a7-2886e6867e0d.json)
used the genuine Keynote-to-PPTX conversion as an unchanged seed. Impress called
`storeAsURL` once through a fresh isolated iCloud mount, then loaded mounted A,
edited its title and subtitle and called `store` once. Both versions reopened
read-only with their exact registered text on one slide. Independent checks
verified every ZIP member's CRC and the slide XML.

Independent iCloud DATA readers matched all 8,304 A and 8,416 B bytes. A normal
read-only remount returned exact B and its selected current identity. The genuine
completed atomic replacement retained original A as its Trash backup; a separate
typed observer verified its exact identity, revision, size and raw hash. The
complete stopped journal stayed unchanged across the remount and Trash observation,
with nine Uploaded rows, six Applied mutations and fifteen completed queue entries.
These are journal counts, not provider HTTP counts. No mutation was retried.

The trial completed in 465.65 seconds within its original 600-second window and
45-second cleanup reserve. All twelve controller children, both outer owners and
both private Office owners closed. Mounts and sockets were absent; the independent
retained-evidence audit passed 24/24 checks. All 569 bound sources, role binaries,
Office modules, protected seed and donor pins remained exact. The nineteen
installed artifacts, service unit and daemon birth remained unchanged.

All four binaries were freshly built in W39 from `19f604d` plus the exact tested
pending source, identical to committed Root `a28dd0f`; they were not built from a
clean `a28dd0f` checkout. Normal daemon/CLI binaries were frozen before the feature
CLI/probe build. Impress ran headless with network access disabled; authorized
mounted saves used the host daemon. Trash verification retained a typed raw-byte
receipt, without a second archive CRC parse or whole Trash inventory. This is one
ordinary PPTX workflow with exact slide text, not native Keynote saving, broader
presentation fidelity, GUI, repeatability, installed acceptance or full row487
closure. The four remaining criteria and installed HOLD remain open.

### Explicit native Pages DATA observer controls on 7 October 2026

The [scoped Pages DATA observer](benchmarks/icloud-pages-data-observer-2026-10-07.json)
adds a feature-only route for genuine raw Pages files created and overwritten
through an ordinary FUSE mount. It requires actual provider DATA with no package
source root or semantic claim. The saved-version observer binds the prior
independently verified A, exact current B and the completed original-A Trash
receipt. The Pages route carries one original 600-second deadline and 45-second
cleanup reserve through nested reads and final publication. Existing Numbers
wire shapes, its default 900-second observer and native PACKAGE contracts remain
separate.

Twelve Pages and twelve Numbers controls passed. Five guard removals each failed
the intended one-test assertion, and the same tests passed after exact restoration;
eight wrapped Pages and eleven Keynote regressions also passed. The initial
harness rejected Cargo's standard expected test-failure trailer. That completed
negative result was retained, the classifier was corrected, and only the eleven
remaining arms ran. The combined retained-evidence audit passed 17/17 checks.
All compiler owners closed and all 570 sources were restored. The subsequent
[complete project check](benchmarks/icloud-pages-data-complete-check-2026-10-07.json)
passed all ten sections, 87 Rust result groups and all twelve new test names.

These are synthetic observer controls, not real Pages DATA acceptance. A fresh
owned-cloud create/save, independent readbacks, normal read-only remount and exact
Apple reopening remain required. No provider representation override, native
Linux Pages save, installed change or full row487 closure is claimed.

### Actual native Pages DATA create/overwrite on 7 October 2026

The [fresh registered Pages DATA trial](benchmarks/icloud-pages-data-create-replace-2026-10-07-e5ed45e4-02cd-45e3-8ea1-20568c3e6a3a.json)
created one genuine Apple-exported A through an ordinary writable FUSE mount.
A stopped independent reader proved actual DATA and matched all 100,824 bytes
before the sole B write became eligible. The writer then verified mounted A,
opened the same file without following symlinks, checked its descriptor identity
and truncated/overwrote it once with the genuine edited 101,769-byte B export.
Stopped independent readbacks verified complete current B and the exact original
A identity, revision, size and raw SHA-256 in Trash. No package representation or
semantic normalization was accepted as DATA proof.

The run completed in 205.52 seconds within its original 600-second window and
45-second cleanup reserve. All nine inner and two outer process owners closed;
mount and socket were absent. Two ordinary daemon stops used orderly TERM.
The stopped journal contained two Uploaded rows, one Applied folder mutation and
three completed queue entries, with no native association, package publication
or file-replacement rows. These are journal counts, not provider HTTP counts.
The independent retained-evidence audit passed 53/53 checks. All 570 bound sources,
frozen roles, genuine original exports, donor and nineteen installed artifacts
remained exact; the installed service unit and daemon birth stayed unchanged.

Before cloud dispatch, a local check exposed a private-driver footer catching
its own successful SystemExit. The controller and writer successors changed only
that final catch from BaseException to Exception. Eight actual-footer controls
reproduced the old failure and verified corrected success and retained refusal;
the corrected real local check passed. Original failed logs were preserved, and
that failure performed no cloud access.

This is raw content copying through ordinary FUSE create and truncate-overwrite,
not a native Linux Pages editor save. The separate normal read-only remount below
has passed. The later exact-ID Apple open failed, as recorded below; its cause is
unresolved. Typed raw Trash verification
does not establish a second retained archive CRC check, whole Trash inventory,
general fidelity or repeatability. Row487 and the other three remaining criteria
stay open; installation remains on HOLD.

### Normal read-only remount of Pages DATA B on 7 October 2026

A [separate registered read-only trial](benchmarks/icloud-pages-data-readonly-remount-2026-10-07-e751e06a-51e0-48be-b964-f510b3e78f1b.json)
started the normal daemon with fresh read-only settings and an empty account
state, copying only the audited saved-session ciphertext from the closed donor.
No prior journal, index or cache was copied. Normal path lookup and the typed
cached-path reply selected the exact completed B identity. A descriptor-bound
read from the actual read-only FUSE mount matched all 101,769 genuine B bytes.
The complete scoped Node matched the saved receipt, allowing only omission of
its redundant content_version equal to the same ETag; every other field stayed
exact. No new journal or cloud mutation occurred.

The trial completed in 12.22 seconds within its separate original 600/45 window.
All three inner and one explicit outer original owners closed, along with the
unified launcher handle; mount and socket were absent. An attempted later
launcher PID snapshot found that it had already completed, so no reconstructed
PID/ownership receipt is claimed. The independent terminal audit passed 37/37.
Complete stopped donor and writer-state inventories, 570 Root sources, frozen
normal binaries and nineteen installed artifacts remained exact; installed unit
and daemon birth were preserved.

Source review first caught a proof-body-versus-pin comparison in the private
supervisor. Its original local prepare refused before fixture creation; the
narrow correction then passed actual local preparation and checks before the
clock/provider phase. Both outcomes remain retained. This completes the separate
normal remount endpoint for this DATA fixture.

The [separately registered Apple trial](benchmarks/icloud-pages-data-apple-reopen-2026-10-07-df50f6ee-f23c-4229-860d-0c209fe78821.json)
verified the exact owned parent and current FILE identity, then opened that row
once. Apple displayed “This document can’t be opened right now.” There was no
content verification, reload, typing, export or retry. Both owned tabs closed
within the original 600/45 window; independent audit passed 28/28. The cause is
unresolved, and no post-open provider revision or representation was retained.
The successful PACKAGE reopen is a separate representation case. Raw DATA and
normal remount byte parity establish neither Apple editor admission nor native
Linux Pages saving. Broader fidelity and full row487 acceptance remain unproved;
the four remaining criteria and installed HOLD stay unchanged.

A [separate normal Numbers DATA remount](benchmarks/icloud-numbers-data-readonly-remount-2026-10-07-8d791c0c-5902-44e6-b184-d39a2ad5935e.json)
completed in 41.56 seconds, matching the exact current B item, revision
`i1bm::i1bl`, all 138,943 bytes and SHA-256
`223e42672ba3735b6464719ed6e2be70a107b437f0b11d267fbef0552e965724`.
Its independent audit passed 49/49. The [subsequent exact-ID Apple Numbers
open](benchmarks/icloud-numbers-data-apple-reopen-2026-10-07-d6466c5a-d307-4552-90a1-887a91ff9300.json)
failed with “This spreadsheet can’t be opened right now.” Its exact parent and
current FILE row, and the opened editor document UUID, were verified. Both
owned tabs closed after 106.59 seconds within the original 600/45 window.
Independent audit 22/22 verified the retained refusal and owned-tab closure.
No values, formula, reload, typing or retry were observed. Pages and Numbers DATA
therefore have exact raw storage and
normal RO remount evidence but failed Apple editor opens in these two cases.
Genuine PACKAGE reopen successes remain separate; neither the cause nor a
provider representation change after these DATA opens is established.

A [subsequent exact Pages metadata read](benchmarks/icloud-pages-data-exact-metadata-read-2026-10-07-c912d2f6-428e-4b86-bb30-70d7bf19abef.json)
reported revision `i22n::i22l`, changed from the pre-open `i22m::i22l`, and an
absent `shortGUID` field. It stopped after one call because the revision changed.
Only fixed metadata categories were retained; no GUID values or provider bodies
were logged. Neither observation proves why Apple refused the document, its
current representation or its current bytes. The earlier raw proofs remain
evidence for the revisions they actually observed.

### Keynote DATA validation controls on 7 October 2026

The [registered focused controls](benchmarks/icloud-keynote-data-observer-2026-10-07.json)
passed all fifteen test stages. Twelve new Keynote DATA tests and the existing
twelve Pages and twelve Numbers DATA tests passed. Five individual guard removals
each caused the intended single assertion failure; restoring each guard made the
same test pass. The existing eight Pages and eleven Keynote PACKAGE controls also
passed. All original child owners closed and all 571 candidate source hashes were
restored. The independent retained-result audit passed 112/112 checks.

The [complete project check](benchmarks/icloud-keynote-data-complete-check-2026-10-07.json)
then passed all ten sections, including 87 Rust result groups, actual kernel/FUSE
tests, script checks and documentation generation. All twelve new named Keynote
tests executed. Only the nine proposed source paths were integrated into the main
development checkout after the original check finished; its final source hashes
match the compile checkout. These are synthetic validation and source checks.
A [fresh genuine Keynote DATA trial](benchmarks/icloud-keynote-data-ordinary-save-2026-10-07-672e98c3-39b6-45a1-85bd-16cc77a6404b.json)
subsequently completed one owned folder, one ordinary FUSE create of raw A and one
same-file overwrite/fsync of raw B. The independent reader confirmed actual DATA
A before B could be dispatched. Further independent reads verified all 527,694 B
bytes and the exact 525,068-byte original A in Trash, with distinct current and
backup identities and their typed revisions. A clean additional fsync added no
operation. Nine inner and two outer original processes closed; the test mount
and socket were removed. The case completed in 206.28 seconds within its original
600-second window and 45-second cleanup reserve, without an automatic retry.

The current feature build reused the already verified normal CLI/daemon and
produced an exact three-role source binding; its independent audit passed 33/33.
Cargo reported the probe artifact as cached, with unchanged bytes. A mistyped
Root input-hash preflight stopped before any compiler launch and remains recorded.
The four-script live packet's final source and local-preparation audit passed
22/22. The real cloud outcome has separate retained receipts.

The [separate normal read-only remount](benchmarks/icloud-keynote-data-ro-remount-2026-10-07-3d114776-4344-4869-b628-502e45fd1b06.json)
stopped at its exact metadata guard: the index retained B's internal staging name
and earlier revision although the completed publication receipt held the final
name and revision. All owners closed without mutation; raw mount capture was not
completed. The [ordinary publication investigation](benchmarks/icloud-ordinary-completed-current-convergence-2026-10-07.json)
reproduced this completed-history gap in a synthetic normal read-only regression.
A [local transport regression](benchmarks/icloud-ordinary-replacement-binary-mime-2026-10-07.json)
also confirmed that ordinary replacements stage binary iWork content under a
`.txt` name and declare `text/plain` in the allocation request and upload header.
The correction preserves the original suffix for binary staging and recovery,
retains legacy checkpoint reservations, and makes completed ordinary history
refresh the exact current identity before publishing its metadata. Local checks
passed 19 metadata cases, eight MIME/checkpoint cases, five router cases and
38 shared DATA-reader cases. The corrected working-file router fixture also
failed at its intended suffix assertion when only the old hint was restored;
the DATA refusal control failed when its exact suffix allowlist was omitted.
Initial compilation and fixture failures remain recorded in the same artifacts.
The [complete project check](benchmarks/icloud-ordinary-replacement-complete-check-2026-10-07.json)
then passed all ten sections and 87 Rust result groups, including actual
FUSE scenarios, script checks and documentation generation. The original
Clippy refusal and its test-only correction remain recorded. Root and compile
checkout contain the same 571 runtime source hashes. Fresh normal and feature
roles subsequently built from those checked sources; the real-provider endpoints
are recorded below. The correction is not installed. This separate transport
defect does not establish the cause of Apple's refusal.
The [separate exact-ID Apple Keynote open](benchmarks/icloud-keynote-data-exact-apple-reopen-2026-10-07-672e98c3.json)
was refused with "This presentation can’t be opened right now." The visible row
and editor URL both matched the confirmed current B identity, and both own tabs
closed with no edits or uploads. This DATA result does not inherit the successful
PACKAGE reopen result. This is a bounded raw archive-copy create/save, not a native Linux editor
workflow or broad export-fidelity result. No installed service changed and no
full-iCloud acceptance criterion closed.

A [fresh normal read-only followup](benchmarks/icloud-keynote-data-ro-remount-2026-10-07-8dec719b-e163-4cad-a31e-901318c12759.json)
using newly built corrected normal roles observed the canonical Keynote filename,
parent and size. It refused the newer metadata ETag `i2to::i2tm` against the
registered historical `i2tn::i2tm`; no raw capture ran. The independent closure
and diagnosis audit passed 24/24, without treating that audit as observation
success. Original state and journal frontier remained unchanged; the revision
change's cause is unproved. This closed arm is not replayed.
A [new owned Keynote DATA writer](benchmarks/icloud-keynote-data-mime-followup-2026-10-07-2b4c7986-df74-4332-b24f-9d6bfd435fcd.json)
then completed a fresh A create, independent A preflight and one B overwrite
using the corrected candidate. Independent reads confirmed all 527,694 B bytes
and the exact 525,068-byte A original in Trash. Its nine inner and two outer
original processes closed in 208.24 seconds within the original 600/45 window;
mount and socket were absent. Its independent writer audit passed 36/36.
A [separate normal read-only remount](benchmarks/icloud-keynote-data-ro-remount-2026-10-07-edba5896-a07e-4cf8-98a7-d52735c5f3d1.json)
then returned the exact current B identity, final filename and all 527,694 B bytes
in 8.62 seconds. The independent audit passed 29/29; all original owners closed,
mount and socket were absent, and source state and journal frontier were preserved.
A [separate Apple Keynote observation](benchmarks/icloud-keynote-data-exact-apple-reopen-2026-10-07-2b4-8653574f-c244-47b2-9284-99828cb9ecbf.json)
opened the exact selected current B row. The genuine B title and subtitle remained
visible on its single slide before and after exactly one reload. Undo and Redo
were disabled, no content was edited or uploaded, and both own tabs closed within
the original observation window. Its independent audit passed 26/26; identity
is linked through the selected row and sole new editor, without claiming literal
UUID equality in Apple's short editor URL. This completes one bounded DATA
archive-copy create/replace, original-Trash verification, normal read-only capture
and Apple reopen chain. It does not establish native Linux Keynote saving, broader
fidelity, repeatability or Apple's earlier refusal cause. No installed service
changed, no old writer was replayed and no full-iCloud criterion closed.

### Numbers PACKAGE atomic save on 7 October 2026

The [once-only atomic-save trial](benchmarks/icloud-numbers-package-atomic-save-2026-10-07-33711731-75c5-4d9e-bc26-aa385cab9748.json)
created one owned folder, imported A and replaced it with B through a native
temporary-file write, fsync and rename on the normal FUSE mount. The public save
completed, a held reader retained A, the canonical stream read B, and a clean
fsync produced no additional operation. The controller nevertheless exited with
a verification failure: it incorrectly required the current B metadata index to
equal the immutable original A hydration binding. The failed overall trial is
retained; its writer has not been replayed.

The [validation-only reader correction](benchmarks/icloud-numbers-atomic-detached-reader-controls-2026-10-07.json)
admits the clean detached original stream only after checking its exact atomic
transfer, original receipt, scope, owner, revision and complete archive digest.
The canonical inventory predicate remains strict. Removing this handling caused
the intended one-test assertion failure; restoring it passed the same test and
all ten receipt-bound FUSE tests, including changed-provenance and byte-tamper
refusals. The isolated build audit passed 36/36 checks. No mutation, schema or
normal filesystem behavior changed.

A [fresh separate read-only observation](benchmarks/icloud-numbers-atomic-postflight-read-2026-10-07-60fc4036-f746-4f74-a796-052cd70d276a.json)
then verified full semantic V2 current B and the exact original A in Trash.
Its independent audit passed 28/28, including receipt and revision bindings,
original process closure, source provenance and preservation of the installed
daemon and all nineteen installed artifacts. The original 600-second window and
45-second cleanup reserve were retained. No provider mutation or writer replay
occurred. Provider ZIP encodings differ from local source ZIPs; full content-tree
identity establishes the A/B comparisons.

The normal CLI/daemon and strict offline archive scanner then built from the
same fully checked sources. The build packet stopped before its first scanner
control because the scanner binary exceeded the verification tool's default
64 MiB read bound. That failed packet remains recorded. A separate
[five-control observation](benchmarks/icloud-numbers-atomic-readonly-scanner-independent-controls-2026-10-07.json)
reused the successful builds with an allowance restricted to that exact scanner
binary. Genuine A and B passed; wrong raw digest, wrong root and a changed IWA
member refused. Its independent audit passed 59/59. No compiler or provider
operation was repeated.

A [fresh normal read-only remount](benchmarks/icloud-numbers-atomic-normal-readonly-remount-2026-10-07-b6ce3c1e-f8e7-4dfd-8a2f-b2d46b66150a.json)
then reached ready on an isolated, genuinely read-only mount and returned the
complete semantic V2 B archive. The observation completed in 6.59 seconds within
its original 600-second window and 45-second cleanup reserve. All five child
processes were reaped, the mount and socket were removed, and the stopped source
state and installed daemon remained preserved. The temporary test daemon received
the expected termination signal for normal shutdown; the other children did not.
Its independent audit passed 34/34, including preserved source bytes and protected
file stamps, the derived journal frontier and all nineteen installed artifacts.

[Apple Numbers reopening](benchmarks/icloud-numbers-atomic-exact-apple-reopen-2026-10-07.json)
matched the selected visible row's exact CloudDocs item ID to the confirmed B
receipt. The editor displayed A2=7, B2=3, C2=10 and `SUM(A2:B2)`, with no typed cell
input. Its retained screenshot, accessibility text and DOM identity audit passed
24/24. Apple uses a shortGUID editor route rather than the CloudDocs UUID expected
in the preregistration; that correction is retained. The list opened a separate
editor tab, and a second open created a duplicate which was closed without edits.

These separate observations support one bounded archive-copy atomic replacement,
independent current/Trash readback, normal read-only remount and exact-document
Apple reopen. The original writer trial remains failed. They do not establish
native Linux Numbers editing, repeatability, installed acceptance or the complete
application matrix. Acceptance row 487 remains open and installed delivery remains
on HOLD.


### Fresh Pages DATA chain on 8 October 2026 (Europe/Vienna)

The [new Pages DATA trial](benchmarks/icloud-pages-data-corrected-candidate-2026-10-07-9fa551d8-c617-4ae7-ac5c-d4d5360bafb9.json)
used the fully checked metadata/MIME candidate and unchanged genuine Pages80
exports, rather than replaying the earlier e5ed writer or df50 Apple refusal.
Its independent source peer passed 16 checks. One fresh ordinary FUSE A create
was independently confirmed as actual DATA before a single B overwrite was
allowed. Independent provider reads then matched all 101,769 raw B bytes and all
100,824 original A bytes in Trash. The same working owner retained the correct
new current identity and a clean fsync created no additional operation. Nine
inner and two outer original processes closed in 233.64 seconds within the
original 600-second window and 45-second cleanup reserve, with mount and socket
absent. The independent actual audit passed 36/36. No source archive was
normalized or changed; no installed action occurred.

The [separate normal read-only remount](benchmarks/icloud-pages-data-normal-readonly-remount-2026-10-08-6231f854-e2dd-4a83-a018-38526ed315ca.json)
used normal feature-free CLI/daemon roles from the same checked 571-source
candidate. The bound supervisor source peer passed 17 checks and its local-input
materializer passed 13. The once-only preparation and local check passed before
any clock or provider dispatch. The fresh observation returned the exact B
identity, final filename, registered revision and all 101,769 B bytes in 8.62
seconds. All original owners closed and the mount/socket disappeared; complete
original source membership, bytes, protected stamps and journal frontier were
preserved. The independent actual audit passed 29/29.

The [separate exact-row Apple Pages observation](benchmarks/icloud-pages-data-exact-apple-reopen-2026-10-08-9fa-4e7c87d1-0bba-4983-8362-0a61f8269683.json)
opened the selected row's exact current B identity in its sole new editor tab.
The genuine 93-byte paragraph remained identical before and after exactly one
reload, as confirmed by the native accessibility selected-text value and its
SHA-256, matching the original Pages80 readback. Selecting text did not edit it;
Undo/Redo stayed disabled. Screenshots and DOM snapshots were retained, both own
tabs closed, and the observation completed in 249.49 seconds within its original
600/45 window. A read-only DOM textbox lookup timed out before evaluation; a
subsequent uninitialized local expected-text guard stopped before any action.
Both observation failures and the corrected exact-text accessibility proof remain
in the same artifact. Neither caused another open, reload, upload or content edit.
The independent audit passed 28/28. The retained screenshots show the opened
Pages document and selection, but their small text is not an independent visual
transcription; the complete native accessibility value supplies the exact text
proof.

This completes one bounded Pages DATA archive-copy create/replace, original-Trash
verification, normal read-only readback and exact-text Apple reopen chain. The
historical Pages refusal remains failed and unreplayed; this new case does not
establish its cause or MIME causality. Native Linux Pages saves remain unsupported
by the tested import-only filter. Layout, fonts, media, repeatability and installed
acceptance remain unproved; full-iCloud rows 487–490 stay open and delivery stays
on HOLD.

### Fresh Numbers DATA replacement and read-only remount on 8 October 2026

The [new Numbers DATA trial](benchmarks/icloud-numbers-data-corrected-candidate-2026-10-08-8bbfa670-42ac-4f04-91d1-a6cbd6afc018.json)
used the same fully checked 571-source metadata/MIME candidate as the fresh
Pages and Keynote chains, with an unchanged genuine Apple A/B export pair.
The independent source review passed 18 checks. One ordinary FUSE create was
independently verified as actual DATA before exactly one descriptor-bound
overwrite. Provider readback matched all 138,943 B bytes and all 138,881 original
A bytes in Trash. The confirmed replacement has a new item identity; its
revision, parent and working-generation lineage are bound to the saved receipt.
A clean fsync added no operation. Nine inner and two outer original processes
closed in 206.58 seconds, with mount and socket absent. Independent actual audit
36/36 passed; no installed action or automatic retry occurred.

The retained Numbers validation API requires its registration to omit
`original_window` and keeps an internal 900-second timeout. Every observer child
was nevertheless owned and capped by the same original 600-second wall/monotonic
parent window with a 45-second cleanup reserve. Audit V2 corrects V1's overly broad
nested-deadline label; both remain retained in the registered result. The legacy
generated fixture names `source-a.numbers` or `source-b.numbers` as its source
root, while its semantic field remains null. This is raw DATA verification,
with no PACKAGE or expanded-content substitution.

The [separate normal read-only remount](benchmarks/icloud-numbers-data-normal-readonly-remount-2026-10-08-68e37925-ca6f-483b-b62c-1db7450bf98a.json)
used feature-free CLI/daemon roles from the same candidate, a complete copy of
the closed test state and a fresh read-only mount. Its source review passed
16 checks and its local preparation/check both passed before provider dispatch.
The observation returned the exact B item, filename, revision and all 138,943
bytes in 8.52 seconds. All original handles closed; the mount and socket were
absent and the stopped source state and journal frontier remained preserved.
No new Trash read or cloud mutation was submitted in this arm.
Its independent actual audit passed 29/29, including the exact bytes, scoped
revision, process closure and preservation checks.

The [separate real Strata Keep/Stop trial](benchmarks/icloud-strata-numbers-data-pin-2026-10-08-bad238ab-7129-47c4-b7a5-b331219f41d7.json)
used another complete stopped-state copy and the same normal feature-free roles.
The actual pre-action state was unpinned with zero resident bytes. One Keep action
completed all 138,943 bytes and displayed the kept badge, direct Stop menu and
exact-file availability dialog. A transient fetching description was observed
through accessibility, without a retained fetching screenshot. One Stop action
withdrew the badge and restored the Keep menu; the cached bytes remained resident.
All GUI children closed before the exact B byte capture and daemon shutdown.
The complete observation closed successfully in 15.02 seconds. Independent source
review passed 22/22 and the actual audit passed 33/33, including source state,
journal frontier, installed artifacts and default-settings preservation. All five
screenshots were inspected. Inherited folder pins and installed Strata acceptance
remain separate open requirements.

The [separate exact-item Apple Numbers observation](benchmarks/icloud-numbers-data-exact-apple-reopen-2026-10-08-8bb-d9fd412d-02f8-4c91-ac5c-ab9a952af52a.json)
opened the new allocated FILE from its exact Drive DOM identity, then reloaded
the sole Numbers editor once. The labeled table retained A2=7, B2=3 and C2=10,
with the actual `SUM(A2:B2)` formula and genuine source marker before and after.
Native accessibility confirmed the marker, C2 result and formula parts both
times; individual A2/B2 selections additionally confirmed the values after reload.
Undo and Redo remained disabled, no content input or upload was performed, and
both owned tabs closed within the original 600-second window with a 45-second
cleanup reserve. The complete observation took 226.17 seconds and its independent
audit passed 26/26, including inspection of both retained screenshots. This closes
the bounded writer/remount/Strata/Apple chain for this one new DATA document.
No post-editor revision or raw-byte preservation was measured. The old
0803/d646 failure stays failed and unreplayed. This new archive-copy workflow
does not establish its cause, MIME causality, native Linux Numbers editing,
general formula/layout fidelity or repeatability. Full-iCloud rows 487–490 remain
open and installed delivery stays on HOLD.

### Fresh Pages PACKAGE atomic-save trial on 8 October 2026

The [fresh Pages PACKAGE trial](benchmarks/icloud-pages-package-atomic-d34f610e-e05d-4e1c-8341-098c6dd935f2-terminal-audit-2026-10-08.json)
used the fully checked and formatted 572-source candidate and its separately
frozen normal and mounted-probe roles. The complete repository check passed all
ten sections, with 87 Rust result groups and 36 required named tests; its
independent audit passed 21/21. The role-build audit passed 25/25.

One genuine A import was independently verified as PACKAGE before a single
temporary archive write, fsync and canonical rename to B through FUSE. The held
A descriptor and local B capture matched their complete registered semantic V2
identities. Independent provider observations verified the new B and the original
A in Trash, including all member contents. The observed current archive is
59,889 bytes; its semantic identity matches all twelve registered B files and
100,205 expanded bytes. Apple packaging changes the archive's raw byte layout,
so this PACKAGE proof compares the complete semantic identity rather than
claiming raw parity with the locally wrapped source. A clean fsync added no
operation. The independent actual audit passed 34/34. Eight controller children,
three nested scanners and the outer controller closed within the original
1,200-second window and 120-second cleanup reserve. The mount and socket were
absent, with source, installed artifacts and the installed daemon unchanged.

The separate 9b5 preparation failed locally on rewritten ZIP external attributes;
its proposed continuation then refused an incomplete process census before
changing any asset. Both outcomes remain preserved, with no provider dispatch.
The fresh d34f fixture corrected the ZIP metadata preservation and passed the
actual offline source verifier and final local admission before its sole cloud
arm. This success does not replay either failed preparation.

The closed journal now supplies an exact offline typed projection of the
acknowledged current, original and backup Nodes, joined to the native working
association, namespace publication and public receipts. Its independent audit
passed 27/27, with all seventeen stopped-state files unchanged. The fresh normal
read-only remount and exact-item Apple Pages reopen completed successfully, as
recorded in the follow-up below. This is
canonical archive replacement, without a native Linux Pages editor-save claim.
Ordinary PACKAGE saving is a distinct remaining workflow. Full-iCloud rows
487–490 remain open and installed delivery stays on HOLD.

### Pages PACKAGE atomic-save read-only and Apple follow-ups on 8 October 2026

The [fresh normal read-only remount](benchmarks/icloud-pages-package-atomic-normal-readonly-2026-10-08-9f7027c9-afa1-44bf-aefc-c3bb3bd62cac.json)
used a complete copy of the closed d34f state, current normal feature-free
572-source roles and a freshly linked production semantic scanner. The scanner
accepted genuine B and rejected all four hostile controls; its independent
actual build/control audit passed 29/29. The remount returned the exact B Node,
sole canonical archive and complete V2 B contents through a retained FUSE
descriptor. It closed in 6.61 seconds within its original 600/45 window.
Independent remount audit passed 32/32. All seventeen original state files,
their protected stamps and the journal frontier were preserved. The state copy
retained its cache; this does not claim a cold download or a new Trash read.

The [fresh exact-item Apple Pages observation](benchmarks/icloud-pages-package-atomic-exact-apple-reopen-2026-10-08-238dd615-d388-423c-aa16-6ae706335bdf.json)
matched the current FILE identity in the visible Drive DOM before its sole open.
The sole new Pages editor retained the complete genuine 93-byte B paragraph
before and after exactly one reload. Read-only text selections exposed the full
value through native accessibility; Undo and Redo stayed disabled. Both owned
tabs closed, and the complete observation finished in 461.78 seconds within its
original 600/45 window. Independent audit passed 25/25 and inspected both saved
screenshots. The screenshots retain the same static view; the distinct complete
accessibility snapshots supply the before/after text proof.

Two initial clock-adapter setup errors occurred before any browser action. The
corrected adapter used Root's conservative CLOCK_BOOTTIME lower bound, a 20 ms
early margin for `/proc/uptime` rounding and the unchanged wall deadline. The
original window was never renewed; neither setup error caused an extra document
open or reload. Source and installed artifacts remained unchanged. No
post-editor revision or raw bytes were measured.

Combined with this same case's independently audited writer, this completes
one bounded Pages PACKAGE atomic replacement, original-Trash verification,
normal read-only remount and Apple exact-text reopen chain. It does not establish
native Linux editor saves, ordinary PACKAGE saving, broader fidelity or
repeatability. Full-iCloud rows 487–490 remain open and delivery stays on HOLD.

### Fresh Keynote PACKAGE atomic-save trial on 8 October 2026

The [fresh Keynote PACKAGE trial](benchmarks/icloud-keynote-package-atomic-ff5abf15-0264-46a1-9efd-94a70ae724a7-terminal-audit-2026-10-08.json)
used the same checked 572-source candidate and genuine 54-file A/B pair. Local
preparation preserved all member contents and declared ZIP metadata while
changing only B's registered root name. The actual production offline verifier
and local checks passed; final independent local admission passed 21/21.
Two earlier local input guards refused Root's relative path and placeholder
assemble digest before provider dispatch; both outcomes remain recorded.

One A import was independently verified as PACKAGE before exactly one canonical
archive temporary write, fsync and rename to B through the mounted filesystem.
The independent current archive contains 54 files and 519,514 expanded bytes;
its complete semantic V2 identity matches genuine B. The original in Trash
contains the registered 54 A files and 516,888 expanded bytes. Held A and local
B captures also match their full semantic identities. A clean fsync added no
operation. Independent terminal audit passed 34/34, including the exact typed
receipts, original process ownership and closure, original 1,200/120-second
window and preservation of source and installed artifacts. The mount and socket
were absent, and no automatic cloud retry occurred.

The [fresh normal read-only remount](benchmarks/icloud-keynote-package-atomic-normal-readonly-2026-10-08-184da3f0-452f-44df-917f-f442913a048c.json)
returned the exact current B Node and complete V2 contents in 31.645 seconds;
its independent audit passed 32/32. It retained the complete stopped state and
warm cache, preserved the original state and journal frontier, and made no new
Trash read. All owned processes, mount and socket closed within its original
600/45 window.

The [exact-item Apple Keynote observation](benchmarks/icloud-keynote-package-atomic-exact-apple-reopen-2026-10-08-d26b6cbe-d96b-45ab-9c31-ab6d7a5ef611.json)
opened the selected current FILE once and reloaded its sole editor once. The
genuine B title and subtitle were visually verified on the rendered single
slide before and after reload; this does not claim full slide-text extraction
through accessibility. Both owned tabs closed in 200.233 seconds within the
original 600/45 window, and independent audit passed 26/26. No content edit or
post-editor byte/revision-preservation claim is made.

This completes one bounded Keynote PACKAGE atomic writer/Trash/remount/Apple
chain. Seven of the twelve registered document workflows now have complete
bounded endpoints; three DATA atomic saves and two ordinary PACKAGE saves
remain. Native Linux Keynote editor saves, broader slide/font/layout/media
fidelity and repeatability remain unproven. Full-iCloud rows 487–490 remain
open and installed delivery stays on HOLD.

### Pages DATA atomic-save interruption on 8 October 2026

The [fresh Pages DATA atomic trial](benchmarks/icloud-pages-data-atomic-2026-10-08-f3157aef-67b6-4542-a933-2c165e27ab77.json)
created and independently read back genuine A, then created temporary B and
committed the local replacement. Its test controller stopped with `KeyError`
because Rust correctly omitted an optional `source_unconfirmed_create` field
whose value was `None`. The stopped journal retains the replacement as
`verify_required` and the temporary cleanup as pending. The provider outcome
remains unresolved; local rename completion does not prove cloud completion.

All owned processes closed, with mount and socket absent. Independent failed-run
audit passed 35 preservation and closure checks. No automatic replay occurred.
A separate read-only browser observation showed the canonical row with the
original A identity, but proved neither its bytes nor Trash, temporary absence
or finality. The failed run remains failed.

The [narrow caller correction controls](benchmarks/icloud-data-atomic-optional-none-caller-controls-2026-10-08.json)
reproduced the omitted-field failure in the exact selected guard, passed the
same case after correction, and passed five controls including refusal of
foreign values and changed SQL ownership. Independent actual audit passed
16/16. This validates the selected test-controller boundary, without proving
a complete controller execution or cloud handoff. Seven of twelve bounded
iWork workflows remain complete; full-iCloud rows 487–490 remain open and
installed delivery stays on HOLD.

A [separate fresh saved-session read](benchmarks/icloud-pages-data-uncertainty-read-2026-10-08-4b82c198-90cd-4e1b-9112-d1ac738ef8e3.json)
then matched the original A identity, revision and complete raw bytes, and the
temporary B identity, revision and complete raw bytes. It used a private
standalone observer, with no daemon, journal recovery or mount. The SDK's
`write-probe` feature provided an existing read-only verifier; the helper called
no mutation API. Its build audit passed 19/19.

The observer refused or could not complete the global Trash listing and stopped
after 38.09 seconds without repetition. Its final membership comparison was
not reached. The original state, 573 source hashes and nineteen installed files
were preserved; its sole original process and group closed without signals.
These are two revision-bound raw-content observations, without a complete
stable namespace, Trash absence, staging or replacement-finality proof. They
do not turn the original failed Pages test into a success.

### Fresh Numbers DATA atomic writer on 8 October 2026

The [fresh Numbers DATA atomic writer](benchmarks/icloud-numbers-data-atomic-2026-10-08-de0f154b-d2de-4daa-bce3-c734dd311ca2.json)
passed one temporary B create and atomic replacement in a new owned folder.
Independent iCloud reads matched complete current B (138,943 bytes) and original
A in Trash (138,881 bytes). The held A descriptor still returned exact A after
rename and was closed before waiting for cloud acknowledgement. The stopped
journal retains its unlinked working A.

Independent writer audit passed 36/36, including three uploaded records, two
applied mutations, five complete queue associations, five objects and the exact
replacement lineage. It inspected only a guarded private copy of the complete
stopped DB/WAL/SHM trio. The original trio, 573 source hashes and nineteen
installed files remained unchanged. Both outer and all nine inner original
processes closed; only the two owned daemons received their intended TERM. The
mount and socket were absent after the original 281.08-second run, and there
was no automatic retry.

This completes the bounded writer and original-Trash checks. The subsequent
read-only remount and Apple observation below complete this one Numbers DATA
atomic workflow. Full-iCloud rows 487–490 remain open and installed delivery
stays on HOLD.

The [first normal read-only follow-up](benchmarks/icloud-numbers-data-atomic-normal-readonly-remount-2026-10-08-578657e8-ce08-4cbe-ae03-636aed000d44.json)
read the exact current B identity and all 138,943 expected bytes. It nevertheless
closed as a failed test: the final preservation comparison rejected changed
modification and change timestamps on the cloned SQLite SHM file. Its bytes,
inode, mode and size matched the prepared copy; the original writer state was
unchanged. All three inner processes and their outer owner closed, with the
mount and socket absent. This retained failure does not establish a successful
remount or add a completed workflow. Independent inspection confirmed the exact
logical journal frontier and all journal bytes; no automatic retry occurred.

The narrow comparator correction accepts only modification/change timestamps on
the derived `uploads.db-shm`, records those timestamps, and still rejects changed
bytes, stable file identity, other-path timestamps, membership or logical state.
The old selected guard failed on the retained real pair, the new selected guard
passed, and all eight comparator controls passed; an independent audit confirmed
the actual closed controls (16/16).

The [new separately registered normal read-only remount](benchmarks/icloud-numbers-data-atomic-normal-readonly-remount-2026-10-08-b27a5609-cc26-4247-bdae-cc36bd1893c7.json)
passed in 7.07 seconds with normal binaries built without write-probe features and
`--recovery-only`. Independent audit passed 33/33: exact current B identity and
all bytes, unchanged original state and journal byte/logical frontier, recorded
clone SHM timestamps, installed/source preservation and complete process closure.
The copied cache already held all B bytes; this is a warm-cache observation.

The [exact-item Apple Numbers observation](benchmarks/icloud-numbers-data-atomic-exact-apple-reopen-2026-10-08-32f9269d-8713-463f-bf1d-b2834ca17cc9.json)
opened the exact owned Drive row in its own editor once and reloaded once. Before
and after reload, labeled screenshots showed A2=7, B2=3 and C2=10; native
accessibility confirmed `SUM(A2:B2)`, the source marker and disabled Undo/Redo.
Both owned tabs closed within the original 226.11-second observation, without
typing or content edits. Independent audit passed 33/33. Opening in Apple may
advance provider metadata, so no post-editor raw-byte or revision-preservation
claim is made.

At this stage, eight of twelve bounded iWork workflows were complete. Pages and
Keynote DATA atomic saves and their ordinary PACKAGE saves remained open. This evidence does
not establish native Linux iWork editing, general document fidelity or full
iCloud reliability, and does not release installed delivery from HOLD.

### Fresh Keynote DATA atomic workflow on 8 October 2026

The [fresh Keynote DATA atomic writer](benchmarks/icloud-keynote-data-atomic-2026-10-08-01da7cef-1407-4830-9f76-08c498a11a76.json)
created genuine A in a new owned folder, verified its complete bytes independently,
then created temporary B and performed one atomic replacement. The held original
descriptor still returned exact A immediately after rename and closed before
waiting for cloud acknowledgement. Independent reads matched current B
(527,694 bytes) and original A in Trash (525,068 bytes). The original run closed
successfully in 319.36 seconds, without automatic retries or installed changes.

The independent writer audit passed 36/36. A guarded copy of the complete stopped
DB/WAL/SHM trio retained three uploads, two applied mutations, five complete queue
entries, five objects, the unlinked original working file and exact replacement
lineage. All nine inner and two outer original processes closed; the mount and
socket were absent. The original journal, 573 source hashes and nineteen installed
files remained unchanged.

The [separate normal read-only remount](benchmarks/icloud-keynote-data-atomic-normal-readonly-remount-2026-10-08-a4f0ef41-022e-4a13-ab81-14dc54aa4dcf.json)
passed in 7.17 seconds with normal binaries and `--recovery-only`. Its independent
audit passed 33/33, confirming exact B identity and all bytes, original-state
preservation, unchanged journal bytes and logical frontier, recorded clone SHM
timestamps and complete process closure. The copied cache already held all
527,694 bytes; this is warm-cache evidence. Before execution, the controller's
package-root expectation was corrected to the actual Keynote DATA fixture's null
root. No cloud test was repeated for that preparation correction.

The [exact-item Apple Keynote observation](benchmarks/icloud-keynote-data-atomic-exact-apple-reopen-2026-10-08-a55c81d2-bc01-4597-a708-7ca8f05f05ed.json)
opened the observed parent and document identities once and reloaded once. Both
screenshots showed the genuine B title and subtitle on one slide. Accessibility
confirmed the slide and disabled Undo/Redo; it does not expose the full slide text.
There was no typing or content edit. Both owned tabs closed within the original
169.91-second observation; independent audit passed 33/33. Post-editor raw-byte
and revision preservation are not claimed.

At this stage, nine of twelve bounded iWork workflows were complete. Pages DATA atomic saving
and ordinary PACKAGE saves for Pages and Keynote remained open. All four full-iCloud
acceptance rows 487–490 and installed delivery HOLD remain open. These bounded
archive-copy checks do not establish native Linux iWork editing, general format
fidelity or full-provider reliability.

### Fresh Pages DATA atomic workflow on 8 October 2026

The [fresh Pages DATA atomic writer](benchmarks/icloud-pages-data-atomic-2026-10-08-ffe0da39-0a83-42cc-b165-2910defae614.json)
completed in 311.16 seconds. It created genuine A in a new owned folder, then
created temporary B and performed one atomic replacement. The held original
descriptor returned exact A after rename and closed before waiting for cloud
acknowledgement. Independent reads matched current B (101,769 bytes) and original
A in Trash (100,824 bytes). The writer audit passed 36/36, including the stopped
journal's three uploads, two applied mutations and five completed queue entries.
All original process owners closed, and the mount and socket were absent.

The [separate normal read-only remount](benchmarks/icloud-pages-data-atomic-normal-readonly-remount-2026-10-08-f3b7b6d2-927b-4982-9223-4aeb5d878110.json)
completed in 7.06 seconds with normal binaries and `--recovery-only`; its audit
passed 33/33. It verified exact B identity and bytes while preserving the original
state and journal. The copied cache already contained all 101,769 bytes, so this
is warm-cache evidence. The private capture filename inherited a `.key` suffix;
the mounted document, fixture and verified representation were Pages DATA.

The [exact-item Apple Pages observation](benchmarks/icloud-pages-data-atomic-exact-apple-reopen-2026-10-08-18ba4dbb-fe8f-4c98-9ce6-5ded7b17076f.json)
opened the observed parent and document identities once and reloaded once.
Before and after reload, the rendered screenshot and native accessibility
selection matched the complete known 93-byte paragraph. Undo and Redo remained
disabled; there was no typing or content edit. Both owned tabs closed within the
original 408.41-second observation. Independent audit passed 33/33. Post-editor
raw bytes and revision preservation are not claimed.

Ten of twelve bounded iWork workflows are now complete. Ordinary PACKAGE saves
for Pages and Keynote remain open. The earlier interrupted Pages subject remains
unresolved and was neither replayed nor relabeled by this fresh test. Source
hashes, installed artifacts and the installed daemon remained unchanged.
Full-iCloud acceptance rows 487–490 and installed delivery HOLD remain open;
these archive-copy checks do not establish native Linux iWork editing, general
format fidelity or full-provider reliability.

### Fresh Pages PACKAGE ordinary workflow on 8 October 2026

The [ordinary writer](benchmarks/icloud-pages-package-ordinary-8bbbb835-e4ea-460c-b6f5-bbbfba5d79b9-2026-10-08.json) passed one canonical truncate/write/fsync save, independent current B and original A in Trash verification, and complete process/mount closure. Its independent audit passed 39/39, retaining exactly two uploads, one applied namespace mutation and three complete queue entries. The held original remained A while current metadata advanced to B.

The [normal read-only remount](benchmarks/icloud-pages-package-ordinary-normal-readonly-2026-10-08-b6d2dbdb-7691-4113-aa9d-de69668d38b4.json) completed in 6.63 seconds and passed audit 35/35. Semantic V2 verified 15 entries, 12 files and 100,205 expanded bytes; the 59,889-byte captured ZIP is a separate transport representation. Original state and journal were preserved.

The [exact-item Apple Pages observation](benchmarks/icloud-pages-package-ordinary-exact-apple-reopen-2026-10-08-192eaf1f-655e-4cc3-ae8d-e553d6892f81.json) passed audit 33/33. Before and after one reload, rendered screenshots and native accessibility selection matched the complete known 93-byte paragraph. Undo/Redo remained disabled; there was no typing or content edit. Both owned tabs closed within the original 454.86-second observation. Post-editor raw bytes and revision preservation are not claimed.

Eleven of twelve bounded iWork workflows are complete; ordinary Keynote PACKAGE saving remains unexecuted. Source hashes and installed artifacts/daemon stayed unchanged. Full-iCloud rows 487–490 and installed delivery HOLD remain open. Native Linux iWork editing, general fidelity and repeatability are not established. The earlier unresolved Pages subject and Numbers cached-formula import failure remain recorded.


### Fresh Keynote PACKAGE ordinary workflow on 8 October 2026

The [ordinary writer](benchmarks/icloud-keynote-package-ordinary-a0a06783-4280-4a2c-b34c-d94b737df23e-2026-10-08.json) passed one canonical truncate/write/fsync save, independent current B and original A in Trash verification, and complete process/mount closure. Its independent audit passed 39/39, retaining exactly two uploads, one applied namespace mutation and three complete queue entries. The held original remained A while current metadata advanced to B.

The [normal read-only remount](benchmarks/icloud-keynote-package-ordinary-normal-readonly-2026-10-08-4c0b9e76-a9c0-4267-977f-af372b8e2cb8.json) completed in 6.62 seconds and passed audit 35/35. Semantic V2 verified 58 entries, 54 files and 519,514 expanded bytes. The captured ZIP was 464,446 bytes; its encoding hash differed from the writer transport ZIP while full semantic content matched. Original state, journal and completed frontier were preserved. Cache warmth was not measured.

The [exact-item Apple Keynote observation](benchmarks/icloud-keynote-package-ordinary-exact-apple-reopen-2026-10-08-d976ad1d-71ac-4374-8f6e-c63013c409db.json) passed audit 33/33. Actual screenshots showed the complete genuine B title and subtitle on one slide before and after one reload. Native accessibility confirmed Slide 1 and disabled Undo/Redo; it did not extract the full slide text. Both owned tabs closed within the original 472.81-second observation, with no content edits or automatic retries. Apple normalized the exact-item opening URL to an opaque editor route in the same tab; the actual route change is recorded separately from the opening identity reference. Post-editor raw bytes and revision preservation are not claimed.

All twelve registered bounded iWork archive-copy workflows are complete: three formats, two representations and two save patterns. Source hashes, installed artifacts and the installed daemon stayed unchanged. Full-iCloud rows 487–490 and installed delivery HOLD remain open for supported export fidelity, installed account/recovery transitions, inherited pin and event behavior, and reliability boundaries. Native Linux iWork editing, general fidelity and repeatability are not established. The earlier unresolved Pages subject and Numbers cached-formula import failure remain recorded. A separate offline probe of the current Numbers fixture is prepared but has not run.


### Current Numbers DATA import/export formula boundary on 8 October 2026

The [fresh offline Calc observation](benchmarks/icloud-numbers-data-calc-formula-export-2026-10-08-5ef91b93-ac46-40f4-80cd-bf5a07d49d7e.json) used the current immutable 138,943-byte genuine Numbers DATA B whose SUM and values 7/3/10 had been verified in Apple Numbers. One isolated read-only native import, one XLSX export and one read-only reopen completed in 0.96 seconds under the original 60-second work plus 30-second cleanup window. Original Office, sandbox and supervisor owners closed; source bytes, dependency pins, runtime573 and installed19/PID were unchanged, with no provider calls, cell edits, explicit recalculation or application retry. Ownership/preservation audit27 and independent formula audit18 passed their verification scopes.

Formula fidelity failed. C2 was already VALUE10 with formula string `10` immediately after native import, and remained VALUE10 after export/reopen. Independent inspection of all ten XLSX members verified CRCs and the single worksheet relation; A2/B2/C2 held numeric7/3/10 and C2 contained no formula element. This locates the earliest observed loss at the Calc native Numbers import boundary. It does not prove the exact external importer defect or source-byte corruption. The historical 17/3/20 specimen failure remains separate. Two pre-application Root setup errors (source-peer DTO field and access-time comparison) were corrected before the only application execution and are retained.

This completes the observation, not formula acceptance. A separately registered Apple Numbers Excel download tests a distinct supported route. All full487–490 and installed delivery HOLD remain open.


### Apple Excel export and read-only Calc formula preservation on 8 October 2026

The [owned Apple Numbers Excel export](benchmarks/icloud-numbers-apple-excel-export-2026-10-08-4cfbc759-df14-4a24-a84d-9e1614eb3146.json) opened the exact recorded Numbers DATA document once and downloaded one Excel copy, with no content edits, reload or retry. Native values 7/3/10 and SUM were visible before and after download; Undo/Redo stayed disabled. Both owned browser tabs closed within the original 405.46-second observation. Independent UI/identity/closure audit passed 28/28. No post-editor native raw-byte or revision preservation is claimed.

The initial local capture helper mistakenly serialized a cross-realm Buffer as JSON. Its original file and incorrect receipt remain retained. The byte array was recovered without repeating the download; it exactly matches the unchanged 6,964-byte downloaded XLSX, SHA-256 `2eeb79e968e9d40c030ecbf21fcaca52032e1c3118cdac914f33e6db057dbccf`. The corrected raw capture is the application input. An independent raw mapping audit passed 9/9, checking all eleven ZIP member CRCs and the single worksheet relation. Apple adds the `Table 1` title row: the native A1 marker maps to A2, and native A2/B2/C2 map to Excel A3/B3/C3. C3 contains cached 10 and the exact single-cell array formula `SUM(A3:B3)` with `t="array"` and `ref="C3"`.

The [separately preregistered read-only Calc observation](benchmarks/icloud-numbers-apple-xlsx-readonly-calc-2026-10-08-d1a23b21-9996-4c3d-b6de-840ec776b7cc.json) performed exactly one XLSX load in an isolated network/PID namespace. It completed in 0.61 seconds under the original 60-second work plus 30-second cleanup window. Actual Calc A3/B3/C3 values were 7/3/10; C3 was FORMULA with raw spelling `{=SUM(A3:B3)}` and error zero, matching the exact array spellings declared before execution. The source marker matched. No export, edits, explicit recalculation, provider calls, signals or automatic retry occurred. All original Office, sandbox and supervisor owners closed, and source/dependency pins, runtime573 and installed19/PID were preserved. Independent formula and ownership/preservation terminal audits passed 20/20 and 24/24. The formula auditor's original command-indexing check failed and remains retained; its corrected V2 passed without repeating the application.

This is a supported export-copy route for the registered SUM/value/marker specimen. It does not repair either direct native Numbers importer failure. Native iWork saving from Linux applications is unsupported by the tested import-only filters. Pages DOCX acceptance covers one exact 93-byte paragraph; Keynote PPTX covers one slide's exact title/subtitle; Numbers XLSX covers these exact values, SUM dependency and marker. Full layout, fonts, images/media, charts, animations, macros, external links and arbitrary formulas/documents are untested. Saving an exported Office copy does not write back to its native iWork source. Historical uncertain and failed outcomes remain recorded and unreplayed. These independently audited export-copy endpoints, all twelve archive-copy workflows and the explicit limits close the literal [application criterion 487](benchmarks/icloud-iwork-application-acceptance-2026-10-08.json). The installed lifecycle, desktop delivery and reliability criteria 488–490 remain open, and installed delivery stays on HOLD.

### 2026-10-08: inherited-pin prerequisite refuses a changed parent revision

The separately registered [exact freshness read](benchmarks/icloud-strata-inherited-exact-freshness-2026-10-08-a865f43c-f842-4eb3-a718-e14832eb3065.json) ran once with the historical Numbers DATA parent/child revisions. The authenticated parent-metadata check refused an ETag difference before child verification or the content download. The leaf exited 1 after 10.214 seconds; no mutation or automatic repeat occurred. The old tuple is therefore not accepted as current, and the inherited-pin GUI arm has not run.

An independent audit passed 17/17 checks for process closure and preservation; the freshness observation remains failed. The original 30-entry source state, current 573 runtime sources, 19 installed files and the installed daemon's unit/PID/birth were unchanged. This does not establish an expired session or criterion 489. A separate metadata-only investigation must obtain the exact owned parent and child revisions before a new fixed-revision content check can be registered; it must not substitute newly observed revisions into this failed arm.

### 2026-10-08: local retained-Stage observer stops before offline export

The [fresh local retention observer](benchmarks/icloud-native-stage-local-retention-2026-10-08-fe7f02dd-477c-4348-a0a1-6dab2a576f24.json) copied the stopped historical state under its five existing leases and queried only a complete detached journal copy. It stopped after 0.026 seconds, before starting the CLI export. Its private caller incorrectly compared the abandonment record's decoded-checkpoint digest with a sealed checkpoint file's ciphertext digest. Production computes these over different representations; the mismatch does not establish corruption or loss of the retained data.

The failed arm remains recorded. It started no daemon, made no provider request and performed no abandonment or replay. Root verified the complete original 31-entry state, current 573 runtime sources, 19 installed files and installed unit/PID/birth unchanged; an independent preservation audit passed 14/14. Offline export remains unproved by this arm, and criterion 490 remains open. A corrected observer must be a separately registered run preserving the record digest and sealed-file digest as distinct bindings.

### 2026-10-08: retained abandoned Stage passes a separate offline-export arm

The [separately registered corrected observer](benchmarks/icloud-native-stage-local-retention-2026-10-08-66914e7d-c4ee-4261-82f0-600c7ec64073.json) passed in 0.139 seconds. One current normal-feature CLI `export-save --offline` returned all 66,268 original payload bytes with the historical SHA-256. The exact persisted abandonment record, complete schema-18 logical journal frontier, sealed ciphertext and original payload were preserved; the only permitted clone change was timestamps on a byte-identical SQLite SHM file. No daemon, provider call, abandonment or replay occurred.

The independent terminal audit passed 27/27. Root also rechecked the complete original 31-entry state, current 573 runtime sources, all 19 installed files, installed unit/PID/birth and executable unchanged. The failed predecessor remains failed. This is a local retention/export proof joined to the historical typed receipt, not a fresh socket receipt lookup or evidence of the Stage's current cloud location, deletion, TTL cleanup or remote finality. It strengthens the bounded recovery evidence without closing criterion 490 or releasing installed delivery HOLD.

### 2026-10-08: separate metadata observation and committed candidate

A separate [two-node metadata observer](benchmarks/icloud-strata-inherited-two-node-metadata-2026-10-08-20cc01ca-3dec-4c3f-8fcb-3214b0f6a03e.json) confirmed the exact owned parent at revision `i2vp`. The child metadata API returned, but the observer's grouped identity/name/parent/size projection refused before retaining the child fields. The exact failing field remains unknown; historical archive length is not an admission rule for a current metadata-only observation. The once-only arm exited 1 after 2.129 seconds, with no content read or mutation. An independent closure/preservation audit passed 22/22, and Root separately checked the installed unit unchanged. The earlier strict old-tuple refusal remains failed. A new reduced typed-node findings arm is needed before a current child tuple can authorize a separate content or inherited-pin test.

The private observer's first offline compilation failed on a missing `ReadProvider` import; its failed build remains recorded. A separately registered corrected link passed with an independent 21/21 build audit. Neither compilation executed the helper or a provider request, and no product runtime source changed for these helper corrections.

The tested 573-source product candidate was committed as [1de7831](https://github.com/Dandiccf/cirrove/commit/1de78313df829dcea8e82c4d2b51dc38ae15506c) and fast-forward pushed to draft PR 86. Root verified every committed runtime blob against the exact successful full-check map. [CI run 37791280430](https://github.com/Dandiccf/cirrove/actions/runs/37791280430) is bound to that commit and was still in progress at this checkpoint. The existing local role builds retain their original `90c5b83` provenance; byte-equivalent sources do not relabel their compiled HEAD. Green CI, attested candidate packages, real-state compatibility proofs and installed delivery remain unproved. Criteria 488–490 and HOLD remain open.

### 2026-10-08: current Numbers identity and candidate package provenance

The separately registered [typed-node findings observation](benchmarks/icloud-strata-inherited-two-node-findings-2026-10-08-e8701163-0d8e-4f01-9d1e-32d60d4bae08.json) passed in 1.929 seconds. The same owned parent was at `i2vp`; the exact Numbers child was at `i2vo::i2vn`, with its expected name, parent and file kind. Its provider-declared size was 138,902 bytes, 41 fewer than the historical source. All ownership comparisons passed. The independent audit passed 26/26 and Root separately verified the installed unit and all 19 installed files unchanged.

This observation made two metadata method calls and requested no content. It does not establish the current DATA/PACKAGE representation, document contents, a cause for the remote change, or which discarded operand failed in the earlier observer. The historical refusals remain failed; a new fixed-current-revision content observation must precede the inherited-pin GUI arm.

The exact Arch split packages from the same candidate CI run have now been downloaded and cryptographically verified against the candidate commit, source ref, workflow and GitHub-hosted run identity. A complete archive inventory covered all 22 regular installed members, the four ELF programs and packaged unit, with no install scripts, hooks, links or duplicate installed members. The extracted candidate daemon's state-free `--storage-format-json` reported journal 21 and metadata 8. These are package provenance and compiled-format observations; no package was installed and no daemon was started. Full CI, coherent real-state backup, retained journal-14 compatibility and installed acceptance remain separate prerequisites.

The [completed candidate CI and package provenance record](benchmarks/icloud-candidate-package-provenance-2026-10-08-1de7831.json) now confirms all seven CI jobs passed for that exact commit. The independent package audit passed 16/16. Coherent backup, actual retained-state compatibility and installed delivery remain unproved; the running packaged daemon was not replaced or restarted.

The [current-source compatibility verifier controls and build](benchmarks/icloud-current573-retained-clone-verifier-build-2026-10-08-d8dd07bd-7b4b-4268-86cb-29c941b5411c.json) also passed: six exact synthetic controls, normal features and a frozen verifier ELF built against the same 573 runtime sources. The independent terminal audit passed 30/30. An earlier offline metadata attempt failed before compiling because its unseeded helper lock selected an unavailable newer transitive crate; that failed arm remains recorded. The separate successful arm seeded the exact candidate workspace lock and introduced no registry dependency upgrade. These controls and the compiled verifier do not prove compatibility of the user's actual retained state; that isolated-copy run remains outstanding.

### 2026-10-08: current-revision ordinary content read refuses

The [separate fixed-current-revision content observation](benchmarks/icloud-strata-inherited-current-content-2026-10-08-6e90476d-4c16-40f7-befd-5c6d3ffef7d9.json) stopped after 3.129 seconds with `Unavailable` during the ordinary read window. The pre-read parent and child guards passed, but no content chunk was accepted. The capture is empty and unvalidated; there is no current content hash, DATA/PACKAGE proof or post-read metadata fence. The independent closure/preservation audit passed 25/25 and Root separately verified the unchanged installed unit and all 19 installed files. No mutation or automatic retry occurred.

The ordinary adapter maps several SDK failures to `Unavailable`, including rejected download representations and lookup or range-response failures. This result therefore does not identify a transient network failure, package conversion, session expiry or product defect. The caller's failed terminal retains `content_observation: null`; the separately pinned reduced helper stdout preserves the actual failure stage without rewriting that terminal. A distinct diagnostic must identify a safe error category before the inherited-pin arm proceeds.

### 2026-10-08: SDK diagnosis identifies a package refusal

The [separate SDK diagnostic](benchmarks/icloud-strata-inherited-sdk-read-diagnostic-2026-10-08-5cde180c-dd7a-4f0c-becd-febdca5f3c3b.json) stopped once after 3.400 seconds with the exact whitelisted `package_representation_refused` category. Apple's download lookup returned a PACKAGE representation that the SDK's ordinary-content verifier refuses before a full content GET. The pre-read ownership/revision guards passed; no raw bytes, digest or after-read metadata fences were obtained. The actual observation remains failed. Independent closure/preservation checks passed 25/25, and Root separately checked the unchanged installed unit and 19 packaged files. No cloud mutation, automatic retry or service restart occurred.

This identifies the SDK category in this new arm. It does not retrospectively identify the collapsed `Unavailable` cause in the previous arm or establish when or why Apple changed the document's representation. The next question is whether the normal package-aware catalog projects this exact current document correctly, with unchanged metadata before and after. The ordinary DATA inherited-pin arm cannot use a PACKAGE specimen by changing its labels; it needs a distinct current DATA fixture. Criteria 488–490 and installed delivery remain open/on HOLD.

### 2026-10-08: normal catalog confirms current PACKAGE projection

The [separate normal-catalog observation](benchmarks/icloud-strata-inherited-normal-catalog-2026-10-08-3c753192-7002-47df-9789-5f0c21ebd0b6.json) passed once after 7.347 seconds. The unchanged normal package-aware adapter projected the exact owned Numbers document as `kind: folder, package: true`. Both parent/child metadata fences and the complete typed owned-folder model stayed unchanged: parent `i2vp`, child `i2vo::i2vn`, logical size 138,902 bytes, one folder entry and one native candidate. The independent audit passed 29/29, including original process closure and source/installed/state preservation; Root also checked the unchanged installed unit.

This establishes the normal PACKAGE catalog classification at these revisions. No package children were enumerated and no archive was downloaded or materialized; generated archive size/digest and editor readiness remain unproved. Logical source size is not an archive length. The result does not identify the earlier collapsed error's cause or when Apple changed representations. The inherited ordinary DATA pin test therefore needs a distinct fresh DATA document verified before opening it in Apple. Criteria 488–490 remain open.

During preparation for the installed-state compatibility run, the private verifier's legacy registration guard was found to require 571 sources even though its successful build used the current 573-source map. The [original build artifact](benchmarks/icloud-current573-retained-clone-verifier-build-2026-10-08-d8dd07bd-7b4b-4268-86cb-29c941b5411c.json) now records that correction beside the original claim. Its six controls and build remain passing; no actual retained-state compatibility had been claimed. A separate private successor changes only that guard to 573. Product runtime sources remain unchanged.

The [private 573-source registration successor](benchmarks/icloud-current573-retained-clone-verifier-build-2026-10-08-c81f6438-86cb-4829-9a51-bbda15d2c5f3.json) subsequently passed the same six controls and an actual normal-feature build; the independent audit passed 30/30. Only the private registration count changed. This corrects the invocation prerequisite, with no real retained-state run or installed change yet.

A separate [authentication-preserving admission control](benchmarks/icloud-preserved-account-auth-admission-controls-2026-10-08.json) first failed under the old all-accounts-Ready rule, passed under the corrected private caller rule, and passed all twelve controls. Existing authentication-required accounts keep their exact identity, desired access and authentication classification; mounted enabled accounts must actually be read-only in the recovery-only trial. Missing real baseline/status/mount evidence still refuses. These are synthetic caller controls, not an installed lifecycle result or a repair of the user's Google authentication.

### 2026-10-08: fresh Numbers DATA specimen verified before Apple

The [fresh ordinary Numbers DATA trial](benchmarks/icloud-numbers-data-before-apple-2026-10-08-5c522758-bf2d-4203-941d-0a79567b7aca.json) passed in 287.559 seconds within its original 600-second window, including 45 seconds reserved for cleanup. It created a distinct owned test folder and file, independently verified DATA A before the sole B overwrite, and then independently verified exact DATA B plus original A in Trash. Current B is 138,943 bytes with SHA-256 `223e42672ba3735b6464719ed6e2be70a107b437f0b11d267fbef0552e965724`; its fresh identity/revision is distinct from the earlier PACKAGE specimen. Original A is 138,881 bytes with its exact source digest. These current observations are measured receipt joins, not reuse of a historical digest as acceptance.

The independent audit passed 36/36. Nine inner and two outer process owners closed, the isolated mount/socket disappeared, and source573, installed19/PID birth and protected genuine/donor assets were preserved. Root separately checked the unchanged installed unit. No automatic replay, byte normalization or Apple editor invocation occurred. The typed journal checks use the actual pinned controller and compiled validators; the reviewer did not open the journal again. This supplies a new current DATA specimen for a separately registered inherited parent Keep/Stop test. That GUI test and normal read-only remount have not run; installed lifecycle, desktop deployment and reliability criteria remain open/on HOLD.

### Fresh fixed Numbers DATA content before inherited pins (8 October 2026)

A separately registered [exact freshness read](benchmarks/icloud-strata-inherited-exact-freshness-2026-10-08-526f65c2-d842-46a3-be8c-1e0ca4b35911.json) confirmed the new owned Numbers DATA version after the successful create/replace/Trash trial. Its fixed parent and child revisions remained unchanged, and the downloaded 138,943 bytes matched the expected SHA-256. The once reader finished in 8.943 seconds, with no mutation, retry or substitution of a newer revision; the independent audit passed 27/27. Original test state and all 19 installed artifacts remained preserved. This establishes the exact input for the inherited folder-pin trial; it does not establish inherited GUI or installed acceptance.

### 2026-10-08: inherited Numbers pin endpoint passes; physical journal guard fails

The [once inherited-folder trial](benchmarks/icloud-strata-numbers-data-inherited-pin-2026-10-08-dbb74b18-c5a2-4814-8c09-81ae7901f885.json) completed the GUI and readback endpoints: initial residency was zero; one parent Keep action produced the inherited child badge, matching menu and availability dialog, complete 138,943-byte residency and an account-generation event. One parent Stop action withdrew the inherited state and refreshed the child menu/badge; no direct child pin action occurred. The fetching description was observed through accessibility. The normal read-only mount returned the exact B bytes afterwards.

The whole registered run nevertheless exited 1 after 17.628 seconds because its final physical journal guard refused. The derived database bytes changed and its empty WAL/byte-identical SHM were recreated; original source state, 573 runtime sources, 19 installed files and three Strata/default files remained exact. All fifteen original owners closed and the isolated mount/socket disappeared. The independent audit verified 30/30 evidence and closure checks while explicitly preserving the failed whole outcome. No cloud write or automatic retry occurred.

A separate immutable-copy SQL diagnosis compared all 33 journal tables. Only the singleton ordinary metadata-publication completion changed: `done` 0 to 1 and `retry_after` to 0, with operation, body and failure count unchanged. The other 32 tables were identical. Normal read-only startup explicitly repairs these local completed publications; recovery-only startup suppresses that repair. The failed physical-parity trial stays failed. A new trial must register this precise local transition in advance while preserving every retained operation, payload and original source state; inherited installed acceptance and criterion 489 remain open.

### 2026-10-08: separately registered inherited-pin whole run passes

The [fresh successor](benchmarks/icloud-strata-numbers-data-inherited-pin-2026-10-08-10793800-3517-45a5-9b8b-105d2ce0a4ef.json) passed the whole once-only run in 17.426 seconds; its independent terminal audit passed 35/35. From zero resident bytes, one parent Keep action made the exact Numbers child fully available offline through its folder. One parent Stop action withdrew that inherited pin. The actual menus, availability dialog, kept badge and account-generation events were checked, and the normal read-only mount returned all 138,943 expected bytes with the exact B digest. Fetching was accessibility evidence; the withdrawn menu obscured the badge area, so withdrawal is established by typed state and events rather than a claim of unambiguous visual badge disappearance.

The registered semantic journal guard inspected all 33 tables and 116 schema objects. Only the exact ordinary metadata-publication acknowledgement changed, with its operation, body and failure count preserved; the other 32 tables and retained payloads were unchanged. Known empty WAL/SHM disappearance after SQLite last close was registered in advance. Twenty-one guard controls passed in the original child; a separate observation correction recovered the completed result after its private caller read the wrong log field, without rerunning tests. All fifteen process owners closed, and the private mount/socket disappeared. The original 30-member source state, 573 runtime sources, 19 installed files, installed daemon PID/birth and three default integration files stayed unchanged.

This closes the bounded private inherited-pin endpoint. It does not close installed desktop criterion 489, establish session-expiry/slow-link reliability or release installed delivery HOLD. The earlier whole physical-parity failure remains failed. The public record also corrects a stale top-level supervisor display pin against the actual approved V3 inputs and original process argv while retaining the immutable preregistration.

### 2026-10-08: real-state preparation refuses before backup; old service restored

The [first actual preparation](benchmarks/icloud-installed-stopped-backup-clone-2026-10-08-61f8697a-190f-42b5-8f93-acda8341f201.json) refused before creating a runtime mask or stopping the service. The private package-route allowlist omitted the current systemd manager's two standard attached-unit roots. Its nine read-only command owners closed; independent verification passed 12/12 while preserving the failed preparation outcome. A private successor admits only these two exact standard roots, retaining all unit, drop-in, template and executable checks; its independent source audit passed 12/12. The tracked installer preflight still needs the corresponding shipping correction and regression evidence.

The [separate successor preparation](benchmarks/icloud-installed-stopped-backup-clone-2026-10-08-b03eaf18-8600-4aa5-aff0-04e9b7f9d9ae.json) stopped the old packaged service cleanly, then refused at the existing-file lease prerequisite. One configured account had no operation lock: this file is created lazily by account actions and is not required to exist merely because the daemon has mounted that account. The other ten expected lock files were present. No inventory, backup, clone, original SQL observation, candidate startup or migration began. All 24 tool owners closed; the independent failed-endpoint verification passed 15/15. Source inspection confirmed the reached lease function only read and locked existing files, with no original file create or data write. This failed arm remains failed.

A separately reviewed, explicitly executed [early restoration](benchmarks/icloud-original-early-restoration-2026-10-08-b03eaf18.json) removed only that exact owned runtime mask and started the same old packaged daemon once. The exact installed executable inode, argv, settings, three mount paths and private socket were verified before handoff. This is restoration after the early read/flock-only refusal; it is neither rollback after migration nor a complete-backup or storage-tuple SQL proof. The next preparation must validate mandatory locks before stopping and preserve the specifically observed lazy operation-lock absence without creating it. Installed lifecycle criterion 488 remains open.

The independent early-restoration audit subsequently passed 20/20, including closure of all 23 tool owners, exact package/settings/source preservation and the actual new original daemon identity. A fresh installed baseline now binds that new process for subsequent trials; the previous PID is a historical endpoint and must not authorize a new run.

### 2026-10-08: real opaque TLS stall demonstrates the product metadata deadline

A [separately registered real transport trial](benchmarks/icloud-owned-stalled-link-2026-10-08-cf0f1e02-34fa-4684-a325-830b2d55fd84.json) used the unchanged compiled reader and a proxy configured only in that child. One connection reached the actual Apple Drive endpoint, forwarded 278 opaque client TLS bytes and withheld 99 server TLS bytes. The reader refused in its exact pre-transfer parent-metadata phase and exited itself after 95.619 seconds overall, under the product's configured 90-second listing-request timeout. The parent sent no signal; its original work/cleanup bounds were 120/15 seconds. The retained capture was empty.

The original whole caller nevertheless refused because its file-reading helper rejected the deliberately empty stdout. That failure is retained. A separately reviewed observation-only correction verified the same closed child and all remaining predicates without another request, child, clock or SQL action; its eighteen checks passed and the independent actual audit passed 26/26. Original 30-member test state, Root/W 573 source maps, compiled roles, 19 installed files and the restored original daemon identity were preserved.

This proves the bounded TLS/parent-metadata transport deadline for one controlled real upstream stall. The public error does not expose a typed timeout, and this arm does not prove authenticated HTTP completion, content-transfer throughput, expired-session handling, uncertain writes or repeatability. Criterion 490 remains open.


### 2026-10-08: complete retained backup, unsettled admission and restored old writer

The actual `a11c19a2-0701-43f1-a12d-3200d2636b96` arm completed a full independent backup and two disposable copies: 5,254 regular files, 5,271 total members and 5,985,486,884 bytes. The original, backup and compatibility clone were never SQLite-opened. The additional complete observation copy was the sole permitted SQL target, but a private adapter argument error stopped that arm before SQL opened. Independent verification checked the complete original/backup/clone byte and stat parity, all saved accounts and desired grants, the packaged files and the closed process owners (26/26 checks). The whole preparation remains **failed** and provides no upgrade admission. See [the actual backup arm](benchmarks/icloud-installed-stopped-backup-clone-2026-10-08-a11c19a2-0701-43f1-a12d-3200d2636b96.json).

A [separate focused read of an existing disposable copy](benchmarks/icloud-a11-existing-disposable-settlement-focus-c998fd86-119f-4c87-88ac-f8db6517deb6.json) subsequently opened all seven copied databases successfully. All integrity checks passed; one copied account retained seven pending mutations and nine incomplete queue entries. Eleven clean, unlinked working streams across the copied accounts remain relevant to recovery exports. The independent terminal audit passed 24/24. This establishes copied-state facts, without querying the original or backup, repairing pending work, or admitting an installed upgrade. Earlier failed observer arms remain failed.

The first explicit old-writer restoration removed the owned temporary mask and completed its reload, then stopped before issuing any start command. Its strict route comparison mixed fresh Python tuple stat records with persisted JSON lists. The failed result is preserved, with independent verification of the no-start outcome and unchanged original/backup (19/19 checks). A separately registered continuation canonicalized only JSON container shape, retained every route field/hash/stat check, and performed the first old-package start without repeating the stop, unlink or reload. The previous packaged daemon, private control socket and all three original mounts were restored; independent verification passed 20/20 checks. No candidate was installed or launched on the original state. There is no claim of unchanged original bytes or zero provider effects after the legitimate old writer resumed. See [the retained failed restoration](benchmarks/icloud-installed-complete-backup-original-restoration-2026-10-08-bcb4be86-62ca-48f9-95df-428357e76cff.json) and [the completed continuation](benchmarks/icloud-installed-already-unmasked-original-restoration-2026-10-08-f4a712c3-0e44-4727-a18b-9b72048d50cb.json).

After that closed handoff, the tracked installer was corrected to accept the two exact standard `systemd/user.attached` search roots. A new regression test first failed on the unchanged installer and passed after the one-line correction. Candidate-unit and drop-in refusal checks passed as well, and the complete portable suite passed 24/24. This is a synthetic installer regression, not installed delivery or real-provider reliability. The [complete project check](benchmarks/icloud-installer-attached-roots-fullcheck-2026-10-08-root-v1.json) then exited zero after 885.731 seconds, covering all ten sections, 87 Rust test groups and actual kernel mounts. See [the regression evidence](benchmarks/icloud-installer-attached-roots-regression-2026-10-08-f642.json). The current Root source differs in these two installer scripts from the historical 573-source build map; previous build/CI evidence must not be relabeled as a build of the changed source.

Normal CLI and daemon builds already implement iCloud ordinary and scoped native archive writes. The probe features enable diagnostics and controlled fault injection; they do not enable the account writer. A new `connect-icloud` connection defaults to read-only, and `--write-access` is an explicit choice. Same-account `reauth` preserves access unless `--write-access` or `--read-only` is selected. Actual installed connection, opt-in, reauthentication and downgrade with retained-byte recovery still require acceptance; manually configured isolated mounts do not prove that lifecycle. The installer fix above was committed as `f1e1375` and pushed to the existing draft development branch after the full check passed. Its [separate CI run](https://github.com/Dandiccf/cirrove/actions/runs/37827802526) was still running at this checkpoint.

The [separately registered archived-state copy](benchmarks/icloud-archived-unsettled-prepare-2026-10-08-bda93f48-13fe-49d3-bce6-9539227bd091.json) completed two full disposable copies of the preserved snapshot. Its [once-only compatibility arm](benchmarks/icloud-archived-unsettled-verify-2026-10-08-bda93f48-13fe-49d3-bce6-9539227bd091.json) then passed: the normal Store constructor migrated all four copied metadata databases from schema7 to schema8, while detached RecoveryJournal readers left all three clone journals at schema14. Every old column cell, completed cursor and queue frontier remained equal; settings, encrypted sessions, payloads and other protected files were unchanged. All eleven eligible working exports matched before and after, including four empty streams. Reduced queue joins established that the nine incomplete entries are exactly seven pending mutations and two resolved uploads, with no orphan, mixed-kind or sequence-mismatch rows. The independent closed-terminal audit passed 32/32. The original installed state was never opened, no Manager, provider, vault or upload worker was constructed, and both the historical archive and new backup remained intact. This proves local recovery-reader and metadata compatibility for the archived snapshot; writable journal14-to21 migration, actual daemon lifecycle and installed acceptance remain unproved by this arm. Criteria 488–490 and delivery HOLD remain open.

### 2026-10-08: archived writable migration and Debian CI timeout

A [fresh complete-copy writable migration](benchmarks/icloud-offline-writable-migration-2026-10-08-d7b9d7b9-80a4-41b6-877e-7703eb6f3ee0.json) passed through the normal public constructors: all three journals moved from schema14 to schema21, and all four metadata databases from schema7 to schema8. The helper exited zero in 349.044 seconds; its independent audit passed 32/32. All twenty legacy tables per account retained their column cells and queue frontiers, including the seven pending mutations and nine incomplete entries established by the earlier archive join. Thirteen new native/publication tables per account were empty. All eleven working exports matched byte for byte before and after, including four empty streams. Settings, encrypted sessions, payloads, the preserved source archive and the new backup remained intact. Only the known absent private operation lock was created, empty and mode0600, in the disposable clone.

The snapshot required no uncertain-attempt resets and contained no explicit retired-working rows, so this real arm gives no evidence for those branches. The [private helper controls](benchmarks/icloud-offline-writable-migration-helper-controls-build-8bb68ffd-9664-4ee6-ab7b-37a7121c08db.json) separately demonstrated one intended assertion failure with the parity guard omitted, twelve passing corrected tests and a normal-feature build; their independent audit passed 26/26. The [first helper build](benchmarks/icloud-offline-writable-migration-helper-controls-build-a36e2777-5913-4a20-9d11-06b5ca114262.json) remains failed: its SQLite sequence decoder did not compile, so exit101 earned zero negative-test credit. The successor changed only the signed SQL decode and checked conversion.

No Manager, credential vault, transfer worker or provider was constructed by the migration arm. The original installed state was never opened, all nineteen installed artifacts and the existing daemon identity were preserved, and all original test processes were closed without signals. This establishes writable-constructor compatibility for the historical retained snapshot. It does not establish a current-state installed upgrade, normal CLI authentication, enabled write opt-in or downgrade acceptance. Criteria 488–490 and installed delivery HOLD remain open.

The [exact f1e1375 CI run](https://github.com/Dandiccf/cirrove/actions/runs/37827802526) subsequently finished cancelled: five jobs passed, the Debian package job reached its forty-minute limit, and the dependent headless installation job was skipped. Debian was still downloading APT dependencies; unpacking, package hooks and Cirrove validation had not begun. Default recommendations pulled additional GNOME/WebKit/wallpaper packages, and the log gap around a 25.3 MB WebKit download was approximately 626 seconds. The deeper network cause is unproved. The initial Debian smoke installation now omits optional recommendations; required dependencies and package metadata are unchanged, and the separate headless installation retains default-recommendation checks. A complete new CI run is required before claiming this correction passed.

The next live endpoint uses the normal CLI for a fresh read-only connection, explicit same-account write opt-in and read-only downgrade with genuine retained conflict and working bytes. Each command requires Apple password entry in a local terminal, and a trusted-device code if requested; the saved-session helpers used earlier do not prove that authentication path. Installed lifecycle, Strata deployment and the remaining reliability boundaries remain separate requirements.

### 2026-10-08: finite paced DATA read and current package provenance

The [separately registered finite-paced read](benchmarks/icloud-owned-finite-paced-read-2026-10-08-41a5a972-4633-4e0d-865f-400f5b4507eb.json) downloaded the fixed owned Numbers DATA document through an opaque TLS relay limited to 8,192 downstream bytes per second. The original reader exited zero in 36.136 seconds; its before/after metadata fences and complete 138,943-byte capture matched the registered identity, revision and SHA-256. The independent offline audit verified all 29 checks, including real process/socket closure and preserved source state and installed artifacts. TLS framing and metadata traffic count toward the relay rate; this is not a plaintext file-throughput measurement.

The overall caller nevertheless exited one: it required empty stderr, whereas the diagnostic reader emitted six static phase-duration lines. The original failed terminal is retained. A private successor accepts only the exact six-line diagnostic shape; evaluating both original and successor predicates against the same captured stderr reproduced the original failure and accepted the successor, while added or missing lines remained refused. No cloud request was repeated for that correction. This proves the bounded SDK read endpoint and the private parser correction, not a normal FUSE slow-link result, expired-session recovery or full criterion 490.

Commit `6c6e161` records the archived migration evidence and Debian smoke dependency correction. Its [separate CI run](https://github.com/Dandiccf/cirrove/actions/runs/37839074059) subsequently completed with all seven jobs successful, including Linux checks, Arch, Debian, RPM, dependency-audit, mounted-read and headless installation. All four Arch archives, including the optional debug split, have independently verified attestations bound to this exact commit and run. The normal package inventory contains 22 regular installed members and four ELF programs; the state-free compiled format query reports journal 21 and metadata 8. No candidate was installed. These provenance observations do not establish current-state upgrade compatibility, installed lifecycle or Strata deployment; criteria 488–490 and delivery HOLD remain open.

### 2026-10-08: normal read-only slow-link trial reaches the cold file but times out

The [recovery-only predecessor](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-08-ac0ed7e3-3188-43ea-b9c6-14c0cf420a26.json) stopped before a kernel read. A detached metadata diagnosis found the older staged name and revision; recovery-only startup suppresses the local completed-publication repair. The original path response was not retained, so that diagnosis does not identify the exact failed predicate. The unused cache-query helper also contained a schema error; the correction is recorded beside the failed arm.

A separately registered [normal read-only successor](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-08-e97b2c11-841a-4aff-b1b8-8e2da3439f51.json) passed the current identity, canonical path, revision and initially uncached-file guards. It started one kernel reader with all upstream TLS traffic paced at 8,192 bytes/second. Metadata traffic consumed most of the original 105-second work budget. One content connection started, but the capture remained empty when the scheduled cleanup stopped the daemon. The reader's subsequent ENODEV is a cleanup observation, not an identified originating product fault. The whole run remains failed.

The independent metadata audit passed 25 checks for physical closure and retained evidence. The original state, protected clone payloads, nineteen installed artifacts and original daemon identity were unchanged. All journal schema properties and the other 32 table digests matched. The sole completed ordinary-publication row changed `done` from 0 to 1 and `retry_after` to 0, but its failure count increased from 1 to 3 during normal read-only retries. The registered guard allowed only an unchanged failure count; its refusal and the original terminal's incomplete preservation result remain recorded. A new trial must register the production retry-counter semantics and a work budget covering metadata, content and final fences before execution. Neither failed trial closes reliability criterion 490 or releases installed delivery HOLD.

A fresh [300-second successor](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-08-7f959f9c-0d90-4317-8011-3c46029558ca.json) retained the same pacing and product deadlines, while registering the existing capped publication retries. Its journal and original-state preservation checks passed. The kernel reader nevertheless returned EIO with no bytes: its receipt followed dispatch by 30.031 seconds, and the whole run closed after about 125 seconds, before the 285-second work boundary and daemon cleanup. The independent audit passed 27 checks while preserving whole failure. This timing is compatible with the adapter's 30-second total content deadline, but no typed timeout cause was observed. A controlled local reproduction must distinguish that hypothesis from other mapped transport failures before a product change. No candidate was installed and criteria 488–490 remain open.

The [first normal-daemon paced-read arm](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-08-ac0ed7e3-3188-43ea-b9c6-14c0cf420a26.json) then ended with a failed caller precondition after mounting and a successful normal CLI path query. No kernel file reader started, and the relay observed only metadata traffic. Both original child handles, their process groups, the isolated mount, socket and relay closed inside the original window; original test state and installed artifacts were preserved. A copied-metadata diagnosis found that this recovery-only startup retained the old staged filename and revisions: it deliberately suppresses the completed local publication repair used by normal read-only startup. The precise failed path-response predicate was not retained, so this observation does not retrospectively prove that predicate's cause. Source review also found that the private cached-node helper used nonexistent table names; that helper had not been reached in this run. Both preparation defects require correction before a fresh normal-startup arm. The failed arm remains failed and was not repeated.

On 9 October, [controlled local HTTPS tests](benchmarks/icloud-ordinary-read-deadline-synthetic-controls-2026-10-09.json) reproduced the ordinary adapter's overall deadline defect: a valid 34-second metadata, lookup, content and final-revision sequence was cut off after 30.04 seconds. The adapter now allows 270 seconds for that bounded sequence, while retaining the individual HTTP deadlines, cancellation and final revision checks. All nine focused tests passed, including changed-revision refusal and cancellation. The complete `scripts/check.sh` then passed all ten sections and was independently reviewed. Its earlier failure from an overlong test TMPDIR remains recorded; only the test environment changed for the complete rerun.

The three-file fix is committed as [934dd68](https://github.com/Dandiccf/cirrove/commit/934dd682f4127a742b432e0d99abcfb6b3a697b3). Its [separate CI run](https://github.com/Dandiccf/cirrove/actions/runs/37853143725) was still in progress at this checkpoint. The controlled reproduction does not identify every historical live EIO or establish normal mounted slow-link acceptance. A fresh normal CLI/daemon build and a separately registered live read remain required. No candidate was installed; criteria 488–490 and installed delivery HOLD remain open.

The exact corrected candidate subsequently produced a normal CLI and daemon with no probe features. Their independent build audit passed 22 checks, including all 574 committed source files, actual Cargo artifact associations, frozen binaries and original process closure. A [fresh mounted slow-link trial](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-09-40dbd879-f1b5-405b-b80f-112357bdd7b9.json) then passed the current revision and initially uncached-file checks but still returned EIO with zero delivered bytes, 31.581 seconds after reader dispatch. It closed at about 126 seconds, before the registered 285-second work boundary. Preservation passed for all journal tables, protected payloads, original state, installed artifacts and the installed daemon. The independent terminal audit passed 29 checks while retaining whole failure. This disproves treating the overall deadline correction alone as complete slow-link acceptance. The originating inner failure remains unidentified; the next diagnostic must retain only existing static response-rejection reasons and must not weaken revision or content guards. No candidate was installed and all three remaining full-integration criteria stay open.

### 2026-10-09: successful CI and isolated content-transfer cutoff

The exact `934dd68` [CI run](https://github.com/Dandiccf/cirrove/actions/runs/37853143725) subsequently completed with all seven jobs successful. Independent review verified the complete terminal, all 123 returned steps and the matching package artifacts; the Arch provenance review passed 15 checks. This verifies that candidate's build and CI endpoints. It does not establish installed account lifecycle or resolve the failed live reads.

A [separately registered diagnostic trial](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-09-dc2b13d7-5f8c-4340-be2d-8921d3055535.json) used the same normal binaries and deadlines. Its bounded consumer retained only an existing static rejection reason: `response stream interrupted`. That reason is emitted after the response's range and length checks, while reading its body. The reader still returned EIO with zero delivered bytes. Independent verification passed 32 checks for the observed failure, original process closure and preservation. No token, URL or raw provider response was retained. This narrows this trial's failure to the response body; it does not identify the underlying transport error as a timeout or retrospectively classify other trials.

Two [controlled localhost HTTPS tests](benchmarks/icloud-ordinary-body-transfer-red-2026-10-09-e877b4a7-e2c3-4090-a62c-1477cae8939c.json) then sent valid response headers and the first ten body bytes immediately, followed by a 33-second body pause. Both ordinary range and streamed-window reads failed their exact-content assertions under the unchanged production deadlines, before the final revision check. Independent review passed 20 checks, including contemporaneous source snapshots and actual compiler/test process closure. These controls reproduce a finite content-transfer deadline defect. A bounded request-budget correction is under validation; a new complete check and fresh normal mounted live acceptance remain required. Criteria 488–490 and installed delivery HOLD remain open.

The [corrected local controls](benchmarks/icloud-ordinary-body-transfer-green-2026-10-09-2ff868b3-3aa8-456e-b794-8fcf0c634cad.json) subsequently passed both tests in 66.11 seconds. Ordinary range and window content requests now use the existing bounded 300-second transfer budget, with the overall read capped at 360 seconds. The exact same fixtures returned complete bytes and reached the final revision check; their explicit 270-second test caller cap was unchanged. This correction remains uncommitted in the isolated worktree and has not yet passed the complete check or a fresh live mounted read. The successful `934dd68` CI run covers the preceding overall-deadline fix, not this new transfer-budget change. Installed acceptance and full criterion 490 remain open.

The new correction then passed the [complete `scripts/check.sh`](benchmarks/icloud-ordinary-body-transfer-fullcheck-2026-10-09-a5b22b29-32ab-442a-9460-9d1998fb60f7.json) in 779.40 seconds, with all ten sections, 87 successful Rust summaries and all eleven ordinary-read tests passing in both configurations. Independent fullcheck review passed 24 checks. The only source change during the check was test formatting; the production fix stayed byte-identical to the passing controlled run. It is now committed as [92b758e](https://github.com/Dandiccf/cirrove/commit/92b758ec4af69608912768320f074735e582de8c). A new normal build, fresh mounted live read and exact new-candidate CI are still required. No candidate was installed and criteria 488–490 remain open.

### 2026-10-09: normal mounted slow-link read passes

A [fresh normal build](benchmarks/icloud-ordinary-deadline-normal-build-2026-10-09-a37a0839-b759-4f31-86c3-da6b1ba7aee1.json) passed independent review of its source and artifact bindings. The [separately registered live read](benchmarks/icloud-normal-fuse-finite-paced-read-2026-10-09-87aacc3b-80ff-4c86-a023-ca2fb6a77736.json) then passed through a normal read-only FUSE mount, starting with zero resident content. Four kernel reads returned all 138,943 bytes with the registered SHA-256. The opaque TLS relay enforced an aggregate 8,192-byte/second limit; encrypted metadata and framing traffic are distinct from the independently hashed document capture.

The reader started 93.67 seconds after the original window began, the capture closed at 183.16 seconds and the whole terminal closed at 183.39 seconds. This was also inside the predecessor's 285-second work budget; the result does not establish that the enlarged 540-second window was necessary or identify the prior interruption's underlying error. Independent review passed 35/35 checks, including original state and installed-artifact preservation, all 33 journal tables with the registered publication acknowledgment/retry-counter transition, and closure of the owned processes, mount, socket and relay.

This is the first successful normal mounted slow-link endpoint after the retained failures. One run does not establish repeatability, expired-session recovery, in-flight namespace safety or full criterion 490. The [exact candidate CI run](https://github.com/Dandiccf/cirrove/actions/runs/37859594107) subsequently passed all seven jobs for commit `aa02b8d1`. Its Arch package attestations were independently verified, and a separate state-free package query confirmed journal schema 21 and metadata schema 8. These checks do not establish installed upgrade compatibility. No candidate was installed; criteria 488–490 and installed delivery HOLD remain open.

A [separate read-only saved-session probe](benchmarks/icloud-authentic-saved-session-read-2026-10-09-6ba422f0-7646-4214-a3b9-224a97089496.json) loaded the authentic older session from an isolated copy and successfully listed the root, counting 257 entries without logging their contents. Independent review passed 24 checks for actual process closure and preservation of the original state, isolated copy, sources and installed files. The session remained valid: this result does not provide an expired-session sample or prove fresh password/2FA reauthentication.

### 2026-10-09: current installed state preserved for isolated compatibility checks

The [registered snapshot](benchmarks/icloud-current-installed-coherent-copy-2026-10-09-8a6af95d-ac8d-458a-8b70-3f8f66c3493f.json) cleanly stopped the existing packaged daemon and copied its complete state to an independent disk. Independent review passed 25 preservation checks: the stopped original, backup and working clone each contained 5,254 files totaling 5,985,486,884 bytes, with matching contents and recorded metadata. The whole preparation nevertheless remains failed because the strict detached admission guard refused the copied journals. A [local diagnosis on the disposable observation copy](benchmarks/icloud-current574-existing-disposable-settlement-focus-1ee4c705-82ca-4222-be1a-6a33026091ba.json) identified `unresolved-or-incoherent-row`; the snapshot retains seven pending mutations and nine incomplete queue entries. Their individual correspondence was not established, and no queued cloud work was replayed by these tools.

A [separate restoration](benchmarks/icloud-original-package-restoration-2026-10-09-f88f32b0-2201-4a96-ac08-ced73387ceba.json) resumed the same old package on its unchanged original state. Independent review passed 32 checks, including unchanged installed files, all three mount routes and a local status comparison preserving OneDrive's ready state and both Google accounts' existing sign-in-required state. The candidate was not installed. These observations establish a preserved snapshot and restoration prerequisite; candidate compatibility and installed iCloud acceptance still need their own evidence.

### 2026-10-09: real-state metadata upgrade preserves recovery exports

A [standalone compatibility test](benchmarks/icloud-current574-metadata-only-real-state-clone-2026-10-09-436410c6-24ce-428e-9386-69ebf317195e.json) completed on the preserved real-state clone. The current metadata constructor upgraded schema 7 to 8 while leaving all three journals at schema 14. Its guards verified the old cells and frontiers and all eleven recovery exports, including four empty files. Independent review passed 31 checks for those results, unchanged backup and protected clone files, source and installed artifacts, and the restored old daemon's identity. The helper completed in 341.23 seconds within its registered 600-second bound.

This exercised a standalone local constructor, not a candidate daemon or its workers. It loaded no credentials and made no provider requests. The earlier preparation's detached-admission refusal remains recorded; pending operations were preserved rather than replayed. A launcher input-pin refusal occurred before any helper execution and is retained beside this result. The writable journal upgrade needs its own fresh copy and test. Criteria 488–490 and installed delivery HOLD remain open.

### 2026-10-09: writable real-state upgrade passes

A [separate archive-copy arm](benchmarks/icloud-current574-writer-archive-copy-2026-10-09-6c0a54b7-d673-48c7-85d7-c68c7fec86fd.json) created two complete fresh copies from the immutable captured backup. Independent review passed 26 checks for full content, recorded metadata, the exact constructor inventory digest, current sources, installed artifacts and daemon preservation. The copier opened no SQLite databases and launched no writer.

The [writable constructor test](benchmarks/icloud-current574-writable-real-state-clone-2026-10-09-a977a9ee-c5e5-4e27-bb44-56ec8966ffb6.json) then completed on one fresh copy in 339.60 seconds. All three journals migrated from schema 14 to 21, and metadata migrated from 7 to 8. Before opening any constructor, the helper recorded the expected startup transformations; the endpoint matched those predictions, preserved the active authority and frontiers, retained all eleven recovery exports, and rescued bytes subject to the registered retirement cleanup. Independent review passed 33 checks for these guards, export contents, protected files, backup, sources, installed artifacts and process closure. An auditor-only source-map lookup failure is retained beside its corrected audit; the helper was not repeated.

This proves compatibility of the offline constructors with this captured real state. It does not prove a running candidate daemon, enabled provider workers or installed account lifecycle. The next endpoint is a normal candidate daemon in recovery-only mode on another copy, using its active recovery listing and export commands. The historical snapshot's detached-admission refusal and criteria 488–490 remain open; no candidate has been installed and delivery HOLD remains in place.

### 2026-10-09: guarded daemon trial stops at account-state mismatch

The [normal recovery-only daemon trial](benchmarks/icloud-current574-normal-recovery-only-real-state-clone-2026-10-09-fe3d36c3-d7cf-43d2-b2d3-8902f27a917a.json) started once on another complete copy of the upgraded state, with private mount paths. All three accounts mounted read-only and OneDrive reported ready. Both Google accounts reported `updating_or_offline` instead of their recorded `sign_in_required` state, so the unchanged readiness guard refused the trial before active recovery listing or export. The candidate daemon exited gracefully after TERM; its mounts, socket, relay thread and sockets closed. No automatic retry occurred.

This trial's daemon-only CONNECT guard allowed the two metadata API hosts and denied OAuth transport before an upstream connection. Its retained counts show 24 denied OAuth requests and zero upstream connections or TLS bytes for denied requests. Source review establishes that a blocked token refresh maps to unavailable/offline, so this transport restriction cannot preserve the normal authentication classification or diagnose a revoked Google grant. The [local guard controls](benchmarks/icloud-recovery-only-local-CONNECT-denial-controls-604a3988-075d-4b94-ae17-b43ad2a2167a.json) passed their intended counterfactual failure and corrected denial checks; those controls do not turn this daemon trial into a pass.

The next useful endpoint needs normal authentication transport with the old daemon temporarily stopped, avoiding concurrent refresh of shared credentials. Any successfully refreshed credentials must be retained rather than rolled back. Active recovery exports, complete running-daemon journal preservation and installed acceptance remain unproved by this failed trial; criteria 488–490 and delivery HOLD remain open.

Independent review subsequently passed 31 preservation and closure checks while retaining the whole trial as failed. It verified the immutable backup and upgraded source copy, protected runtime-copy payloads and settings, persistent journal bytes, all current runtime sources and normal binaries, the nineteen installed files and the original daemon's identity. No after-run SQL snapshot was created, so this audit does not claim complete logical parity of all 33 journal tables. An auditor-only receipt-filename error occurred before tree inspection and is retained beside the corrected filesystem audit; neither the daemon nor provider operations were repeated.

### 2026-10-09: normal authentication and active recovery pass on a real-state copy

The [separately registered maintenance trial](benchmarks/icloud-current574-normal-recovery-only-maintenance-2026-10-09-a9cd685d-ed29-4180-b71e-9082b7a9ec6a.json) passed with the normal candidate binaries and a recovery-only disposable schema21/8 copy. The existing packaged daemon stopped cleanly before candidate authentication; a fresh, independent byte backup of the stopped original completed first. Normal metadata, OAuth and verified-identity transport was permitted through five exact hosts only while the old consumer was closed. No upload workers were enabled.

All three copied accounts mounted read-only. OneDrive reached fresh Ready; both Google accounts reported actual `sign_in_required`, preserving their previous classifications. There was no observed account-state transition, and CONNECT traffic does not prove a specific token refresh or rotation. Active service commands exported all eleven registered recovery files with exact bytes and working generations. Detached before/after snapshots matched every schema member and typed-cell summary across all 33 journal tables; persistent journal bytes also remained unchanged.

The candidate, mounts, socket and relay closed, then the same installed package restarted once against its unchanged original state. Independent review passed 34 checks, including the fresh backup's 5,254 files and 5,985,486,884 bytes, historical backup and upgraded source-copy preservation, protected runtime-copy payloads, exports, current sources, nineteen installed files and restored service identity. The auditor did not scan the restarted mutable original tree or open it with SQLite. Successful authentication credentials were retained, with no keyring backup or rollback; no rotation is inferred.

Earlier attempts remain failures: [the initial preflight](benchmarks/icloud-current574-normal-recovery-only-maintenance-2026-10-09-ee95e689-e471-46f7-b617-c2f35ddb718b.json) rejected the existing tray, and [the subsequent process scan](benchmarks/icloud-current574-normal-recovery-only-maintenance-2026-10-09-ca0930c9-6973-4a2e-82e0-7654af51aa3a.json) encountered protected Linux processes. Both stopped before masking or stopping the service. Corrections and their limits are recorded beside those outcomes. The final scanner recognizes only the exact registered installed tray, refuses inaccessible Cirrove-named consumers, and records protected non-Cirrove omissions; it is not adversarial all-process attestation. Selected controls and a live read-only process scan preceded the passing arm. A separate Root registration mistake also refused before namespace creation and is retained; none of these arms was automatically repeated.

This closes the registered running-daemon recovery endpoint for this captured state. It does not settle preexisting uncertain queues, install the candidate, permit an original schema21 writer, repair Google grants or prove iCloud session expiry. Criteria 488–490 and installed delivery HOLD remain open. The scoped session-rejection helper and its bounded owner have independently reviewed artifacts; their next sequence requires a genuine local-person native sign-in, a separately valid control after that sign-in, one target logout, the same control afterward, and public reauthentication with retained-byte proof. Deliberate revocation must remain distinct from natural expiry.

### 2026-10-09: complete isolated native state prepared for session recovery

The [offline full-state copy](benchmarks/icloud-isolated-full-native-state-copy-2026-10-09-81cca9ba-b494-4013-a7a2-6ceeae71bdf1.json) completed successfully. All 22 members, including the journal and its sidecars, sealed payload and dirty working bytes, were retained. Only the copied account's mount path changed; its disabled/read-only settings remained intact. The source state, saved-session control, nineteen installed artifacts and running packaged daemon were preserved. A separate content inspection found no contradictions.

This prepares the retained-byte baseline for a fresh native sign-in and session-recovery trial. It does not prove authentication, session isolation, expiry or installed acceptance. The local-person password/2FA step and subsequent control/logout/reauthentication sequence have not run; criteria 488–490 remain open.

### 2026-10-09: abandoned native Stage recovered through a fresh running service

The [active recovery trial](benchmarks/icloud-active-abandoned-stage-recovery-2026-10-09-d6cb88f0-470c-4f76-87c5-624481e234f8.json) passed on a complete isolated copy of the previously abandoned native operation. The normal candidate daemon mounted that account read-only in recovery-only mode. Public `native-stage-abandonment` returned the exact historical original and Stage identities, and active `export-save` recovered the registered 66,268-byte Pages archive with its exact SHA-256.

Detached before/after observations matched all 30 schema18 journal tables, including the resolved operation, completed queue entry and retained abandonment record. Persistent database, WAL, payload and encrypted checkpoint bytes remained exact. Only the SQLite shared-memory file's modification/change timestamps differed; its contents and stable identity fields stayed exact. Protected copied files and member sets, the complete original source inventory, nineteen installed artifacts and the existing packaged daemon were preserved. The test daemon, child processes, mount and socket closed. Separate inspection confirmed the receipt, export checksum, journal summaries and physical closure.

This fills the earlier offline retention trial's missing running-service receipt/export endpoint. The observed account state was `updating_or_offline`; local recovery did not require Ready. No new abandonment, cloud cleanup, replacement replay or credential repair was requested. The receipt remains historical evidence, not proof of the Stage's current remote location. Session expiry, fresh reauthentication, installed acceptance and full criterion 490 remain open.

### 2026-10-09: candidate packages run on a new Arch test machine

The [guest package/runtime trial](benchmarks/arch-owned-empty-package-runtime-2026-10-09-de9e5b30-006d-4056-ada7-66e1f699c4dc.json) passed after installing a fresh Arch/GNOME system on a separately owned virtual disk. The initial root and tester homes had no Cirrove state or developer shadows, and no Cirrove daemon was running. All four candidate package versions and packaged executable/unit checksums matched the verified archives. The packaged service and its control socket then ran with zero accounts.

The separately installed optional Strata companion matched the prior tested executable and provider inputs. It started on the guest's glibc 2.44 for three seconds, with default file-manager associations and existing preferences preserved. Secret Service had a bus owner; unlock and credential persistence were not tested. This is an installation and startup prerequisite, not real-provider GUI acceptance. The guest powered off, all seven owned host processes closed, and the original host service and existing virtual machines were unchanged. Criteria 488–490 remain open; the next installed endpoint requires actual iCloud account lifecycle and Strata workflows.

### 2026-10-09: installed keyring and native connection dialog readiness pass

The [separate installed readiness arm](benchmarks/arch-installed-public-connection-readiness-2026-10-09-586f901a-7830-49a3-b310-6d7c81a89eaa.json) ran the production `cirrove keyring-check` once in the new guest. Root observed the desktop's new Login keyring prompt and supplied only the guest's public fixture password. Cirrove's synthetic credential write/read/removal passed, and Cirrove-owned keyring item searches were empty before and afterward. The installed CLI's native connection help also passed.

Root inspected the actual installed GTK window, selected iCloud Drive, and reviewed screenshots showing empty Apple email/password fields, Allow changes off and the local-access enforcement disclosure. Sign in was not submitted. The arm completed in 209.22 seconds; all eight owned host processes closed and the guest powered off. No Apple authentication or real account lifecycle was tested. The guest-side detailed receipt remains stored there for retrieval on its next necessary boot; its exact checksum and printed success are retained. Criteria 488–490 remain open.

### 2026-10-09: controlled content interruption is safe; same-copy recovery fails

The [registered interruption arm](benchmarks/icloud-normal-read-interruption-recovery-2026-10-09-c6e840c6-6146-4b56-bd4c-58ec7bc2fe9b.json) used the normal read-only daemon and a cold real Numbers DATA file. The opaque relay cut exactly one content connection after forwarding 32,768 TLS bytes. The kernel read returned EIO with zero bytes delivered; it did not return a truncated document as success. The complete journal comparison preserved all 33 tables apart from the previously registered singleton publication acknowledgment and bounded retry-counter transition. Protected payloads, original state, source and installed artifacts remained preserved. Independent review passed 24 checks of the scoped safe outcome and original process closure.

After that original arm closed, a [separately registered read](benchmarks/icloud-normal-read-interruption-recovery-2026-10-09-a5bbb689-638c-4dc2-bad1-e5c23d004fbe.json) used the same retained clone without deleting cache or resetting credentials, with the fault disabled. This recovery arm failed: another EIO returned zero bytes, and no content-host connection was opened. All 33 logical journal tables remained exactly equal to the first arm's final snapshot. The six original processes, final mount and socket closed; independent review confirmed the failure and preservation. The 30.76-second interval from kernel-read dispatch to failure receipt suggests a pre-content deadline but does not establish the cause. The relay's scheduling and the adapter's lookup stages require diagnosis. No automatic repetition occurred, and no recovery or full criterion 490 credit is claimed.


### 2026-10-09: same retained copy reads successfully with fair test-proxy scheduling

A [separate fair-scheduler diagnostic](benchmarks/icloud-normal-read-interruption-recovery-2026-10-09-08bdbd8d-8f80-45bb-9555-22220b636611.json) used the same retained failed recovery copy, without removing cache, resetting credentials or altering provider data. Aggregate pacing remained 8,192 bytes/s with 512-byte quanta; request and complete-read limits were unchanged. The normal read-only FUSE daemon returned all 138,943 bytes through four kernel reads with the expected checksum, starting from zero resident bytes. The complete 33-table journal frontier remained exactly unchanged, protected payloads and original/installed state were preserved, and all three owned processes, mount, socket and relay closed. Independent review passed 29 scoped checks.

The registered local socket comparison first showed that the old insertion-order scheduler could starve another buffered connection, then showed that round-robin service delivered its 512 bytes. The successful live successor reported one content connection and a maximum buffered wait of 0.244 seconds. These results support correcting the test proxy; they do not uniquely attribute the preceding live failure or justify increasing production deadlines. The original failed recovery remains a failure. This is one bounded read-recovery result, without repeatability, independent remote postflight, natural session expiry or full criterion 490 acceptance.


### 2026-10-09: saved cookie lifetime regression fixed locally

The [registered local cookie regression](benchmarks/icloud-cookie-expiration-local-2026-10-09.json) demonstrated an actual failure in the old public session-snapshot API: restoration replayed a positive relative `Max-Age`, renewing an already elapsed cookie. The identical public-API fixture passed with the fix, together with ten additional controls for expiry precedence/boundaries, deletion, scope/order, consistent metadata, repeated snapshots, legacy adoption and bounded extreme ages. Both actual test binaries used normal features, with exactly one expected old-code assertion failure and eleven new-code passes. Formatting and focused Clippy passed. The [integrated complete project check](benchmarks/icloud-cookie-expiration-integrated-check-short-tmp-2026-10-09.json) then passed without `--fast`, including kernel mounts, scripts, the ledger and documentation. An earlier full run failed three desktop tests because the chosen temporary prefix exceeded Unix socket path limits; the identical test binary passed all fifteen tests with a shorter disk-backed prefix before the complete successor ran. The first failure remains recorded. No credentials, keyring or provider were accessed.

Newly received cookies now retain receipt time and absolute expiration inside the sealed snapshot. Replayed finite cookies use absolute expiration or deletion tombstones. Whole-second precision can expire a cookie up to one second early. Unchanged legacy ciphertext still lacks its original receipt time; its explicitly labelled restoration anchor is durable only after an explicit snapshot save. This fixes future local cookie persistence, not an observed natural Apple session expiration. Criterion 490 and installed delivery remain open.
