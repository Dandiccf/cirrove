# iCloud Drive write integration boundary

Cirrove's normal iCloud connection is read-only. The write experiments in
PR #86 operate in fresh, explicitly owned validation folders. This note
records the concrete adapter boundary needed to turn those experiments into
an ordinary account implementation; it is not a claim that writes are ready.

## Current code boundary

- `Settings::validate` rejects writable iCloud accounts, and
  `accounts::write_provider` refuses iCloud. The desktop hides its write
  control. The manager's write factory now receives `Account` and an owned
  `WriteContext`: private state root, account metadata index, shared journal
  and checkpoint vault. Engine has acquired the account lock before this
  context is opened. The upload workers use the same journal and vault;
  iCloud selects sealed operation checkpoints and other providers keep
  `DesktopVault`. The production factory now selects the context-aware iCloud
  router, but settings still reject writable iCloud before this path is reached.
  The router implements new-file uploads, staged replacement, simple folder
  operations and rename/move/Trash of regular files up to 32 MiB;
  combined move/rename now has a durable three-step implementation and a bounded
  account-router live result; mounted and interrupted relocation acceptance remain open.
  A synthetic FUSE regression covers ownership at construction, writer
  failure after ejection, and a later successful writable remount. A writer
  error no longer silently selects a read-only mount. Published old write
  controls are released before rebuilding the journal and replaced after
  a successful remount.
- Normal `ICloudDrive` resolves a cold directory by listing its parent. Its
  single-item `ReadProvider::node` still refuses a cold non-root ID. A later
  [exact-item investigation](benchmarks/icloud-trash-exact-lookup-2026-09-30.md)
  identified the earlier endpoint failures as a request/response envelope bug.
  The corrected direct transport matches active and recoverable Trash metadata;
  enabling cold-node discovery through that transport needs its own scope and
  projection validation. A path string cannot supply the missing identity.
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
  The existing 32 MiB verification limit still applies; transient or changed
  remote content during hashing currently surfaces as uncertain preparation.
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
- `ICloudFileCreate` now accepts zero-byte creates and still refuses files over
  32 MiB. The [zero-byte protocol investigation](benchmarks/icloud-empty-file-create-2026-09-29.md)
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

Remaining product questions include over-32-MiB files,
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
