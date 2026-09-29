# iCloud Drive write integration boundary

Cirrove's normal iCloud connection is read-only. The write experiments in
PR #86 operate in fresh, explicitly owned validation folders. This note
records the concrete adapter boundary needed to turn those experiments into
an ordinary account implementation; it is not a claim that writes are ready.

## Current code boundary

- `Settings::validate` rejects writable iCloud accounts, and
  `accounts::write_provider` refuses iCloud. The desktop hides its write
  control. The manager's write factory receives only `Account`; its generic
  worker uses `DesktopVault` for upload checkpoints.
- Normal `ICloudDrive` resolves a cold directory by listing its parent. Its
  single-item `ReadProvider::node` deliberately cannot fetch a cold non-root
  ID because Apple's item endpoint failed in the live probe. A path string
  cannot supply the missing identity.
- The normal-build iCloud Create, folder and file mutation adapters accept
  exact `Node` inputs and sealed account sessions, but there is no account-wide
  router selecting them by each journal request. The mounted validator
  supplies those nodes from a bounded test tree and its owned journal.
- The two-ID replacement and conditional Trash handoff still have
  `write-probe`-gated, owned-fixture types. Their constructor requires an
  original SHA-256 from a prior confirmed fixture upload. A pre-existing
  iCloud file has no such journal row. The upload request carries item ID and
  ETag but neither parent nor original digest. A restart after moving the old
  item to Trash cannot recompute that digest from the visible folder.
- `ICloudFileCreate` refuses empty files and files over 32 MiB. The zero-byte
  live experiment did not establish a safe successful upload. Content uses
  one streamed HTTP POST per file, not resumable network chunks; a lost POST
  result must reconcile the reserved exact item rather than resend blindly.

## Required implementation sequence

1. Give the iCloud write factory the account's private state and metadata
   index after the engine has established account ownership. An operation
   router must resolve `Scope(account, collection, item)` against the indexed
   node and its parent chain, then independently re-observe the exact parent,
   name, ID and ETag before any Apple mutation. Never infer identity from a
   path or a duplicate name. SQLite lookups must end before network awaits.
2. Replace the fixture-only source-digest assumption with a durable
   mutation-free preflight. For an existing file, hash the exact original
   revision through the read transport and checkpoint that digest together
   with source ID, ETag, parent and target operation before allocating or
   registering staged content. Reopening a replacement must rebuild solely
   from the journal and sealed checkpoint even if the old ID is in Trash.
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
