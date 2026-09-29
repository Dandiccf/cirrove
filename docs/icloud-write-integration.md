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
  The router implements new-file uploads and simple folder operations;
  replacement, file mutations, combined move/rename and live acceptance
  remain open.
  A synthetic FUSE regression covers ownership at construction, writer
  failure after ejection, and a later successful writable remount. A writer
  error no longer silently selects a read-only mount. Published old write
  controls are released before rebuilding the journal and replaced after
  a successful remount.
- Normal `ICloudDrive` resolves a cold directory by listing its parent. Its
  single-item `ReadProvider::node` deliberately cannot fetch a cold non-root
  ID because Apple's item endpoint failed in the live probe. A path string
  cannot supply the missing identity.
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
  destination snapshot. A retained plan authorizes reconciliation only, never
  blind replay. Missing plans remain indeterminate. Parent ancestry excludes
  packages, shortcuts and moves into any descendant. The initial direct-child
  negative control passed without the new ancestry guard because the underlying
  adapter already refused that case; the corrected grandchild case fails with
  the guard removed. Synthetic tests also cover checkpoint rebinding and index
  changes. These tests do not establish live account-wide write reliability.
  Recovery after interruption between saving a plan and sending the request,
  file mutations, replacement and combined move/rename remain open.
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
- The two-ID replacement and conditional Trash handoff still have
  `write-probe`-gated, owned-fixture types. Their constructor requires an
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
  General account-wide operation routing and acceptance are still missing.
- `ICloudFileCreate` refuses empty files and files over 32 MiB. The zero-byte
  live experiment did not establish a safe successful upload. Content uses
  one streamed HTTP POST per file, not resumable network chunks; a lost POST
  result must reconcile the reserved exact item rather than resend blindly.

## Required implementation sequence

1. Use the new owned `WriteContext` to construct the iCloud write factory.
   The account state, metadata index, shared journal and sealed upload vault
   are now available after Engine establishes ownership. An operation
   router must still resolve `Scope(account, collection, item)` against the indexed
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

Remaining product questions include zero-byte and over-32-MiB files,
prolonged session expiry, quota failures, concurrent editors, ordinary
application atomic saves and recovery UI. No ordinary iCloud write path is
enabled by this document.
