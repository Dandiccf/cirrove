# Google Drive preview

Google Drive is implemented as a **writable My Drive and Shared Drive preview under validation**.
An account connected with **Allow changes** uses Cirrove's shared FUSE namespace,
durable journal, conflict handling, cache and offline pin jobs. A read-only grant
still produces a read-only mount.

The mounted path now creates and edits binary files, performs editor-style atomic
replacement, creates folders, renames and moves files and folders, and moves files
and observed-empty folders to Google's trash. Every mutation carries an exact item
identity and the current conditional version. A stale mounted save becomes a
visible conflict and does not overwrite the independently changed Google item.
The bounded live record for these operations is
[`google-drive-writable-mount.json`](benchmarks/google-drive-writable-mount.json).

## What it presents

- Files and folders from My Drive or one selected Shared Drive per connection,
  with paginated listings and polled changes.
- Ordinary binary files through bounded ranges. The adapter compares Google's
  binary `headRevisionId` before and after the download, and checks the response
  range and length before returning bytes to the cache. Sequential reads can use
  the shared read-session contract to stream an 8 MiB window into private cache
  staging under one before/after revision check. A changed final revision rejects
  the whole staged window. The separate numeric file version is exposed only as
  a write observation; it is never sent to Google as an HTTP ETag.
- Shortcut targets in the same user collection. Shared-drive targets remain
  unsupported. A shortcut requiring a resource key becomes a browser link.
- Native Google Docs and Sheets as read-only `.gdoc` and `.gsheet` package
  folders. Opening one materializes the current document as `Document.docx` or
  `Spreadsheet.xlsx`, alongside `Open in Google.url`. Cirrove compares the
  numeric source version before and after export, hashes the exact generated
  artifact, and stages it into the ordinary durable content cache before
  publishing its size. Package ancestry refuses rename, removal and edits that
  would otherwise imply unsupported native-document import. Google's
  `files.export` endpoint limits this path to 10 MiB; PDF and other formats,
  larger exports, and native write-back remain open.
- Provider listings give every pre-existing name its complete item ID before the extension:
  `report [item-id].txt`. This initial policy is deliberately stable across
  pagination, rename, restart and duplicate sibling names. `/`, NUL and `%` are
  escaped; long stems are shortened on a UTF-8 boundary to fit 255 bytes. Items
  created or explicitly renamed through a writable mount retain the natural name
  the application chose while that exact identity is acknowledged. This changes
  mounted presentation only; it never rewrites a remote name just to format it.

Shared Drive discovery and a separate collection are implemented with
synthetic coverage and one bounded Workspace live run. The selected drive gets
its own root, listing, change cursor, cache identity and mount. Root, folder,
binary content, change-feed recovery, daemon restart and small native DOCX/XLSX
exports passed in an isolated mount; see the [live record](benchmarks/google-shared-drive-live.json).
An explicit write grant now permits ordinary binary-file changes in a selected
Shared Drive preview. The bounded live evidence below covers one Workspace
administrator and one owned drive; it does not establish general reliability.
An isolated [write probe](benchmarks/google-shared-drive-write-live.json) created
and read back a test folder and files. It exposed a stale Google session receipt
that kept a committed replacement at `VerifyRequired`. Exact-ID and digest
reconciliation now handles an uncertain session inspection; a bounded live
follow-up restored the fixture and passed replacement, rename and stale-conflict
worker checks. An [isolated mount probe](benchmarks/google-shared-writable-mount-live.json)
then passed run-owned file creation and an interrupted 16 MiB upload across a
private daemon restart. A second isolated mount passed rename, move, replace
and trash on run-owned items. Restricted roles and longer sessions remain open.
Native document imports, exports over 10 MiB, PDF and other export formats, push
webhooks, resource-key transport and large-library/long-session acceptance are not
claimed.
There is no service-account or another client's credential import.

A bounded [native import probe](benchmarks/google-native-import-live.json) could
replace the contents of one test Doc and Sheet and export the expected small
DOCX/XLSX packages. Its conflict check failed: a stale native Doc media upload
returned HTTP 412, yet the rejected content appeared in later exports. That
transport cannot safely implement native writeback. Native packages remain
read-only while a revision-aware native path is investigated.

## Write boundary found during the preview

Drive v3 supplies the stable read model, including the numeric file `version` and
the binary `headRevisionId`, but it does not return the strong HTTP ETag needed by
Cirrove's conditional-write contract. Drive v2 still returns that ETag. The
adapter therefore carries `google-version:<number>` as an observation, performs
an exact v2 and v3 read immediately before mutation, requires both views to name
the same ID, parent, raw name and version, and sends only the resolved v2 ETag in
`If-Match`.

Content and metadata use separate lineages. Google's general file version advances
for metadata-only changes; using it as a content revision made a successful rename
look like a concurrent content edit. Binary nodes now use v3 `headRevisionId` as
`google-revision:<id>` for cache and writeback lineage, while the numeric version
continues to guard the next mutation. Drive documents `headRevisionId` as the head
revision for binary files. The adapter retries only read-only v2/v3 confirmation
when the two API versions are briefly inconsistent; it never repeats an uncertain
mutation during that settling window.

The durable upload worker now supports current-version replacements, zero-byte
replacement, final-commit conflicts and exact-ID/SHA-256 reconciliation after a
lost response. The mutation worker supports prepared folder creation, exact-ID
file/folder rename and move, recoverable trash, and stale-version conflicts. The
live worker gate replaced and restored one deterministic 8 MiB-plus file, renamed
and restored it, and observed `Conflict` when reusing the stale pre-rename version.
The direct probe also proved that a session opened before an intervening metadata
change cannot commit its bytes. Plans, corrections and results are recorded in
[`google-drive-v2-content-probe.json`](benchmarks/google-drive-v2-content-probe.json)
and [`google-drive-v2-worker-probe.json`](benchmarks/google-drive-v2-worker-probe.json).

The normal service selects this transport only for an account whose successful
Google sign-in recorded read-write access. Drive permits duplicate sibling names,
so provider observations use stable full-ID suffixes while acknowledged local
names remain natural. A destination scan cannot make create, rename or move atomic
against another Drive client. Cirrove therefore relies on exact identities and
conditional source updates to preserve conflicting data rather than claiming
Google offers POSIX namespace transactions. Folder trash remains a
list-then-PATCH operation and is not atomic POSIX `rmdir`.

The create path pre-generates the Google item ID and durably stores it before the
first mutating request. Resumable session URLs stay in the credential vault and
are accepted only on the configured Google origin and upload path. Reconciliation
uses the exact prepared ID and hashes the exact file. No token, session URL, strong
ETag or raw provider body is written to logs or benchmark artifacts.

## Check or create your own Google app

1. In [Google Cloud Console](https://console.cloud.google.com/), select a project
   you own for Cirrove, or create one. Under **APIs & Services**, enable **Google
   Drive API** for that project.
2. In **Google Auth Platform**, configure the app's branding and audience. For
   a personal development project with an external audience in Testing, add your
   own Google account as a test user.
3. Under **Clients**, look for an OAuth client of application type **Desktop
   app** belonging to this project. A web client or service-account key is not
   interchangeable. Create a Desktop app client if none exists, and download
   its client JSON.
4. Keep that file private, outside the repository, and set its permissions:

   ```sh
   chmod 600 /absolute/path/to/cirrove-google-client.json
   ```

   The JSON must contain an `installed` object and a `client_id` ending in
   `.apps.googleusercontent.com`. Do not paste the file or its contents into
   a chat or a bug report. Cirrove reads its client ID and optional client
   secret; endpoints in the downloaded file cannot redirect authentication.
5. An ordinary connection requests `openid email profile` and
   `https://www.googleapis.com/auth/drive.readonly`. An explicit write-validation
   connection requests `https://www.googleapis.com/auth/drive`. `drive.file`
   covers files created or explicitly selected through the app, but it cannot
   support a filesystem that must update arbitrary existing My Drive files. Both
   Drive scopes are restricted; Google sets the publication and verification
   requirements for an app offered to other users.
   See [Google's scope guide](https://developers.google.com/workspace/drive/api/guides/api-specific-auth).

For an external app in Testing, a refresh grant including Drive access normally
expires after seven days; signing in again is then expected. See
[Google's refresh-token policy](https://developers.google.com/identity/protocols/oauth2#expiration).

## Connect

Install the developer build first, following [Development](development.md).
The 0.1.0 release does not include Google support.

In the window, choose **Connect a drive → Provider → Google Drive**, select an
empty mount folder and your private Desktop OAuth client JSON, choose whether
**Allow changes** is enabled, then sign in in the browser. The drive-selection
page offers **My Drive** and any Shared Drives visible to that account. You can
also connect My Drive through the CLI:

```sh
cirrove connect-google --label google \
  --client-json /absolute/path/to/cirrove-google-client.json \
  --mount-path /absolute/path/to/an/empty/folder \
  --write-access
```

Omit `--write-access` for a read-only connection. Reauthenticate an existing
connection with `cirrove reauth google --write-access` or turn off writes with
`--read-only`. To choose a Shared Drive in the CLI, add its ID from that drive's
Google Drive URL as `--drive-id`; My Drive remains the default. The separate validator
remains available for protocol probes:

```sh
cirrove connect-google --label google-create-validation \
  --client-json /absolute/path/to/cirrove-google-client.json \
  --mount-path /absolute/path/to/a/separate/empty/folder \
  --state-dir /absolute/path/to/private/validation-state \
  --write-access

cirrove validate-google-create \
  --label google-create-validation \
  --state-dir /absolute/path/to/private/validation-state
```

The validator creates its top-level test folder through the mutation
worker, which persists a generated Google ID before mutation, then creates
an 8 MiB-plus multipart file and an empty file inside that folder, then reads
both back by exact provider identity and SHA-256. It next persists an exact
metadata-probe plan, renames the multipart file with its current strong HTTP
ETag and attempts a second rename with the stale ETag. It then persists payload
sizes and SHA-256 digests, conditionally replaces that file with one small
payload, attempts a distinct replacement using the stale ETag and reads the
winning bytes back. Finally it persists the same non-secret plan for two 8
MiB-plus payloads, performs an aligned resumable replacement and repeats the
stale attempt. It then obtains a fresh strong ETag and sends another 8 MiB-plus
replacement through the provider-neutral journal, vault checkpoints and transfer
worker; reusing that ETag for a second worker operation must end in conflict. It
then creates a nested destination folder through the same mutation worker and
sends a rename followed by a move through it. Exact-ID
inspection must observe both destinations and a follow-up using the stale rename
base must end in conflict. It then creates an archive folder, obtains the nested
folder's exact strong-ETag snapshot, moves that nonempty folder into the archive
and confirms its child remains addressable. Reusing the old folder snapshot must
also conflict. A
missing strong ETag or an accepted stale update fails the command. Session URLs
and ETags are not written to its evidence log. It retains
the cloud folder and private local journal for review.
Running it changes Google Drive and needs explicit authorization for that run.

Access/refresh tokens and the optional desktop client secret are stored in
Cirrove's Secret Service entries. Non-secret settings retain the provider,
client ID, verified identity and root ID. Reauthentication uses the existing
Sign in again control or `cirrove reauth google`; it retains the original
identity and desired mount state. Removal, mount controls and offline pinning
use the existing account lifecycle.

For an isolated live validation, pass `--state-dir` to the connection command
and start `cirroved` with that state directory and a separate `--socket`.
Do not run two daemons against one state directory. The first live check below
used the installed daemon and an additional Google account, alongside OneDrive.

## Validation and boundary findings, 2026-09-20

`cirrove-googledrive` tests exercise the actual HTTP parser and transport, with
synthetic loopback responses: pagination, pre-scan change frontier, catch-up,
opaque scoped cursors, incomplete searches, removals/trash, quota cooldown,
cancellation, exact content ranges, changed versions and document links.
The streamed-window scenario asserts that bytes are accepted only when the final
binary head revision still matches and rejected when content changes after transfer.
Removing only that final lookup/check made the exact scenario fail because its
expected final metadata request never occurred; restoring it passes both arms.
Authentication tests exercise Google issuer/audience/nonce/expiry/signature and
verified-email checks, token-subject binding on refresh, serialized rotation,
stale-401 protection and refusal to use a rotation that the keyring cannot save.
The Microsoft authentication tests remain part of the same suite. Google OAuth
tests distinguish the normal read-only grant from the explicit full `drive`
write grant. Account tests cover enabled Google read-write connections and reject
write factories for read-only grants. Synthetic folder tests
pre-generate an ID, verify the exact create request and inspection, exercise the
provider-neutral prepared mutation path and refuse an occupied case-folded
destination before POST. A separate exact folder snapshot test requires a strong
HTTP ETag before the live validator can enqueue relocation. A service restart
test proves the prepared ID survives
the journal handoff and is not generated a second time. Three metadata-probe and three small-content-probe tests require the
original strong ETag on the first PATCH, distinguish a rejected stale retry from
an ignored precondition, and issue no PATCH when metadata has no strong ETag.
Four resumable-probe tests additionally cover aligned multi-range transfer,
rejection at session creation, rejection at finalization, an accepted stale
replacement and the no-ETag/no-session case. Both content paths read the exact
resulting bytes and compare SHA-256. Five shared-worker adapter tests additionally
cover durable target preparation, conditional session creation, a changed ETag
before mutation, a 412 at finalization, exact-hash reconciliation after a lost
success response and rejection of weak, malformed or empty requests without
network access. Ten validation-mutation tests cover prepared folder creation
and collision plus conditional exact-ID file and folder moves, a
changed source ETag before destination lookup, case-folded destination collision,
applied file/folder and uncommitted reconciliation outcomes, and rejection of a weak
ETag without network access. Seven further cases cover exact conditional file and
observed-empty folder moves to Google's trash, refusal of a nonempty folder,
pagination through every child-list page, reconciliation of both exact trashed
identities and refusal to treat a 404 alone as confirmed removal. The write
validator does not invoke this removal path. No cloud mutation was used for
these checks.

`crates/cirrove-service/tests/google_drive.rs` drives both real adapters through
one store and cache, deliberately giving them equal account, collection, file,
content revision and size values but different bytes. Restarted offline cache
reads still return the correct provider's content. A second scenario interrupts
both the initial listing and catch-up and reopens the database: visible metadata
and the completed cursor remain paired until catch-up publishes, without taking
a new starting frontier. A kernel fixture mounts Google through `Engine` and
`CloudFs`, reads duplicate names, rejects a write open, unmounts and reopens
cached content with stable inodes while the HTTP server is offline.

The version-change scenario was also run as a negative control: removing only
the final metadata/version comparison made the exact
`tests::reads_require_exact_ranges_and_unchanged_monotonic_versions` test fail
(one test executed), because it returned bytes after the file had changed.
Restoring the comparison makes that scenario pass. This checks that the test
detects the missing guard; it does not establish Google's live consistency.

```sh
cargo test -p cirrove-auth -p cirrove-googledrive --locked
cargo test -p cirrove-service --test google_drive --locked
cargo test -p cirrove-service --test google_drive --locked real_ -- --ignored --test-threads=1
scripts/check.sh
```

The full `scripts/check.sh` completed successfully on 2026-09-19, including
641 passing Rust test executions, the kernel groups, script tests and the
acceptance ledger. All six native window scenarios also passed in the local
Wayland/Xwayland session, including switching between Google and Microsoft;
the pure-X11-only window-class assertion does not apply to that session.
Temporary test files used a short directory on `/home` (btrfs). An earlier
attempt with the full nested worktree path exceeded Unix socket path limits;
that environment failure is not counted as provider evidence.

The kernel fixture is included in `scripts/check.sh` and CI. These are correctness
scenarios, not memory or latency measurements and not evidence of live Google
reliability. Broader live acceptance still needs independent byte comparison,
token refresh, external changes and offline/restart checks with evidence recorded
here.

The callback correction subsequently passed the complete `scripts/check.sh`: 642
Rust test executions plus the kernel groups, script tests, ledger and docs build.

The resumable-precondition probe then passed the complete `scripts/check.sh` on
2026-09-19: 646 Rust test executions plus the kernel groups, script tests,
ledger and docs build. Its exact positive test also failed when only the
outgoing `If-Match` header was removed, then passed after restoration. This is
still synthetic evidence; the live validator has not been run.

Connecting validation-only replacement to the durable worker then passed the
complete `scripts/check.sh` on 2026-09-19: 650 Rust test executions plus the
kernel groups, script tests, ledger and docs build. Its exact worker test failed
when only the resumable session's `If-Match` header was removed and passed after
restoration. This remains synthetic evidence; the live write validator has not
been run.

Connecting validation-only rename and move to the mutation worker, together with
synthetic recoverable file removal, then passed the complete `scripts/check.sh`
on 2026-09-19: 659 Rust test executions plus the kernel groups, script tests,
ledger and docs build. The exact move and trash tests each failed when only their
`If-Match` header was removed and passed after restoration. The live validator
still has not been run, and it does not invoke the removal path.

Extending that worker with a durable prepared item identity and routing both
validator folders through it added two Google HTTP cases and one restart case.
Removing only the journal write for that identity made the restart test fail with
no prepared item after the simulated lost response; restoring it passed. The
complete `scripts/check.sh` then passed with 662 Rust test executions, all kernel
mount groups, script tests, the acceptance ledger and docs. No live Google
mutation was used.

Extending the validation-only namespace adapter to folder relocation and
observed-empty folder trash added four Google HTTP cases. The relocation and
trash tests each failed when only their outgoing `If-Match` header was removed;
the nonempty-folder test failed when only the child listing was bypassed. The
restored cases all passed. This is synthetic protocol evidence only: the
list-then-PATCH sequence cannot provide atomic POSIX `rmdir`, the live validator
does not call it, and no Google mutation participated. The complete
`scripts/check.sh` then passed with 666 Rust test executions, all kernel mount
groups, script tests, the acceptance ledger and docs.

Preparing the live validator to relocate a run-owned nonempty folder added one
exact-ID folder snapshot case. Removing only its strong ETag response header made
that test fail before any mutation could be enqueued; restoration passed. The
complete `scripts/check.sh` then passed with 667 Rust test executions, all kernel
mount groups, script tests, the acceptance ledger and docs. The live validator
still has not been run.

Adding exact folder-move reconciliation and a paginated child guard added two
Google HTTP cases. Removing folder decoding made the recovery case fail; making
the guard ignore the second child page made the other case attempt a forbidden
PATCH. Restoration passed both controls. The complete `scripts/check.sh` then
passed with 669 Rust test executions, all kernel mount groups, script tests, the
acceptance ledger and docs. No live Google mutation was used.

The authorized write validation used a separate disabled connection with
`drive.file` and five registered attempts. The first run established that an
exact prepared folder create does not receive a strong ETag. The second and third
runs established that Drive content-sniffs the recognizable repeated-`0x47`
payload as `video/mp2t` even when metadata, session initialization and every
nonempty PUT declare `application/octet-stream`; exact ID, name, parent, size and
SHA-256 still matched. The fourth run accepted that ordinary file MIME receipt,
then correctly reopened after Drive advanced the new item from receipt version 1
to stable version 6 during post-upload processing.

The fifth registered run is the completed endpoint. Folder creation reached
`Applied`; the 8,388,621-byte resumable create and the empty create both reached
`Uploaded` and passed independent exact-ID/SHA-256 readback. The following
metadata capability probe returned `NoStrongEtag` at version 4. The validator
therefore stopped before any rename, replacement, move, trash or delete request.
All run-owned cloud folders/files and private journals remain for review. This is
live evidence that provider-neutral prepared folder creation and durable file
creation work on this account. It is also evidence that Cirrove cannot safely
offer conditional Google updates under its current conflict contract. It does
not establish general provider reliability or permit a writable Google mount.
The full chronology, corrections and pre-registered decision rules are in
`docs/benchmarks/google-drive-write-validation.json`. The final code passed
`scripts/check.sh` with 671 Rust test executions, all kernel mount groups, script
tests, the acceptance ledger and docs.

The pre-registered Drive v2 follow-up reused only the retained multipart test
file. An exact v2 metadata GET returned a strong file-resource ETag at version 6.
A title PATCH with that ETag succeeded at version 7; a second title PATCH with
the same now-stale ETag returned HTTP 412. An exact re-read supplied the current
ETag, and a final conditional PATCH restored the original title at version 8.
The ordinary daemon was not restarted, and its Google account remained ready
with no stuck changes or failed uploads. This establishes metadata compare-and-set
for that API version, account and binary file. Conditional content upload,
trash/delete, shared drives and mounted-write behavior remain untested. The plan,
decision rule and result are in
[`google-drive-v2-etag-probe.json`](benchmarks/google-drive-v2-etag-probe.json).

The 2026-09-20 content and worker follow-up used the same retained run-owned
binary file. Drive rejected a resumable final commit after an intervening rename,
accepted a current conditional replacement and rejected the stale session at its
eventual commit boundary. The provider-neutral worker then reached `Uploaded`
for replacement and restoration, `Applied` for rename and name restoration, and
`Conflict` for the stale rename. Exact SHA-256 and name checks confirmed the
fixture was restored. The run also exposed and corrected two real consistency
boundaries: v2 and v3 can settle at different times, and metadata changes advance
file `version` without changing binary `headRevisionId`. The write connection now
has the full `drive` scope but stays disabled. Stable mounted-name mapping,
duplicate collisions, folder trash atomicity and mounted application saves remain
open gates; this result does not enable an ordinary writable Google mount.

### Writable mounted milestone, 2026-09-20

The next pre-registered run closed those mounted-name gates for My Drive binary
files. An isolated ordinary read-write account mounted with `rw`; a real FUSE
workflow created and edited a file, performed an editor-style atomic replacement,
renamed it, created a folder, moved the file into it, and removed a file and an
observed-empty folder. A synthetic kernel fixture repeated the workflow with
Google's ID-qualified provider names. A provider presentation hook now preserves
the natural name attached to an acknowledged exact identity without hiding an
independent remote rename.

Google can advance a newly created file's numeric version after returning its
upload receipt. The first atomic-save attempt exposed that race: the replacement
bytes arrived, but the chained cleanup used the earlier version. Production Google
creates now wait for two matching exact-ID observations before the journal
acknowledges the receipt. The corrected atomic save retained its natural name and
left no failed or stuck change.

For the conflict arm, another client renamed the exact remote item between an
open mounted file and its save. The stale upload reached `Conflict`; an exact
provider read proved the foreign bytes were unchanged, and **Keep both** retained
the local bytes as a separate item. Restarting the isolated daemon preserved the
resolved local data and returned with no stuck or failed change. A separate live
pin check made an existing Google binary file resident, then released the pin and
its reservation. Finally, cleanup enumerated and trashed only the registered
run-owned tree by exact identity. The installed OneDrive and read-only Google
mounts ran throughout this measurement.

This one-account run establishes the bounded workflow, not general provider
reliability. The conflict-resolution view currently keeps the resolved local name
alongside the separately named remote item; that presentation is safe but can be
surprising. Shared Drives, native Docs/Sheets content, cross-provider moves and
large-library or long-session acceptance remain outside the claim. See
[`google-drive-writable-mount.json`](benchmarks/google-drive-writable-mount.json).

### Broader live acceptance, 2026-09-20

A second pre-registered run used the installed writable mount and the owner's
ordinary Google connection. A deterministic 20,971,643-byte file crossed the
mount in one writeback entry and matched the direct Drive readback by SHA-256.
LibreOffice 26.8 created an ODT in the mount, reopened it there and wrote a DOCX
beside it; both exact provider reads matched the mounted files. An open descriptor
kept the old bytes through an atomic replacement while the path and Google exposed
the replacement. A rename made through the provider adapter appeared in the mount
after 8.263 seconds, and a subsequent mounted edit of that renamed identity also
matched the direct provider read.

The aggregate read validator's first execution classified all eight selected
files as changed. The diagnostic follow-up found no byte discrepancy: all eight
files and 23,147 bytes matched, while the existing index carried metadata and
content-version lineage written by the older Google implementation. Exact current
metadata and the mounted reads agreed. One previously indexed shortcut target was
already absent at Google. The validator now reports provider version changes,
missing items, unavailable reads and byte mismatches separately instead of
combining them into one counter.

The run also found a provider-neutral integration defect. A newly uploaded file
kept its natural visible name in FUSE, but the control socket used only the
ID-qualified indexed presentation, so `pin --path` and `paths` could not resolve
the name shown by Files or Dolphin. Writable control requests now walk the same
durable namespace overlay as FUSE and hand the confirmed provider identity to the
shared pin engine. The regression test failed without that correction and passed
with it. After installing the checked candidate, a 5,243,003-byte file was pinned,
became fully resident in two blocks, appeared as directly pinned, and was unpinned
through the same natural path. The reservation returned to zero. Both test trees
were removed only by their recorded Google folder IDs; the account finished ready
with no pins, failed uploads or stuck changes.

These results cover one My Drive account and bounded binary-file workflows. They
do not cover Shared Drives, native Google document import, a published and
verified OAuth application, full offline outage behavior, or long sessions and
large libraries. The registered plan, first failures, corrections and exact
outcomes are in
[`google-drive-live-acceptance-2.json`](benchmarks/google-drive-live-acceptance-2.json).

## First real-account connection, 2026-09-19

The owner created and authorized a separate Google Desktop OAuth app in Testing,
with their own account as its only test user. The ordinary mounted account uses
read-only Drive access; the separate disabled validation connection was upgraded
from `drive.file` plus read-only access to the full `drive` scope on 2026-09-20. The
client JSON stays private outside the repository; the resulting grant is stored
in Cirrove's Secret Service entry.

The first attempt exposed a real integration defect: Google's redirect used
`127.0.0.1`, but the shared callback parser still required `localhost` in the Host
header. It rejected the callback and the login timed out. The new exact-loopback
test failed on that implementation (one test executed). The corrected parser
requires the host and port of the registered redirect, including rejecting a
different loopback host or port. All 14 authentication unit tests then passed.
The browser also displayed `ERR_BLOCKED_BY_CLIENT` during the redirect; no
browser protection was disabled. The corrected login received its callback,
verified the Google identity and connected successfully despite that browser
display issue.

The live account reached ready with 1,350 indexed entries and one completed feed.
Its FUSE mount reports `ro`. Three small binary files (319, 1,507 and 334 bytes)
and one 101-byte Google-document browser link were read twice through the mount;
sizes and repeated bytes matched. A later bounded GET-only validator compared
three non-link files (925 bytes total) byte-for-byte between a fresh adapter read
and the mounted view, with no changed or unavailable sample. This exercises two
Cirrove paths and is not a comparison with a separately implemented Google client.

The same validator checked the only indexed shortcut target directly. Google
returned not found; it was not a Shared Drive permission refusal or merely absent
from Cirrove's baseline. The dangling shortcut still produces `ENOENT` in the
mount. The existing OneDrive account was ready with both feeds and its mount still
present after Google connected. The validator is reproducible without printing
cloud names or IDs:

```sh
cirrove validate-google-read --label google
```

The preregistration and failures remain with the result in
[the first-connection record](benchmarks/google-drive-first-connection.json).
This small functional check does not establish large-library, long-session,
offline/restart or token-lifetime reliability.

## Native Docs/Sheets export preflight, 2026-09-21

A pre-registered GET-only run exercised the production package exporter against
one existing Google Doc and one existing Google Sheet without recording their
names or IDs. The DOCX export was 1,345,146 bytes while Drive metadata reported
22,953; the XLSX export was 45,701 bytes while metadata reported 13,619. Both
source versions stayed stable and both exposed a revision through
`revisions.list`, while neither exposed binary `headRevisionId`.

Repeating the export with the same source versions produced different hashes, and
the XLSX size changed by 18 bytes. The implementation therefore identifies the
exact generated artifact by SHA-256 and transfers those already materialized
bytes into the service content cache before its directory entry becomes visible.
It does not treat the source metadata size or version as a unique identity for
Google's generated ZIP bytes. The full preregistration, correction and aggregate
results are in
[`google-native-export-preflight.json`](benchmarks/google-native-export-preflight.json).
This is evidence for two bounded readable exports on one account, not general
format-fidelity or provider-reliability evidence.

## Shared Drive preflight, 2026-09-21

The adapter now lists Shared Drives visible to the signed-in user and can bind a
read-only connection to one selected drive. Its listing and change requests use
the drive ID and Google's shared-drive parameters; file reads stay within that
collection. Synthetic provider and service tests cover discovery pagination,
collection-scoped refresh, root and content caching, and conditional-write request
shape. At this preflight stage, account validation refused a writable Shared
Drive connection.

A read-only query through the original personal Google account returned zero
accessible Shared Drives. A separate Workspace account subsequently created an
owned test drive and folder. The [2026-09-22 live run](benchmarks/google-shared-drive-live.json)
read one binary fixture and small native Doc/Sheet exports through a separate
read-only FUSE mount. It found and fixed a change-feed parser failure: Google
also sends drive-level changes without `fileId`. After the fix, the isolated feed
resumed its saved cursor and indexed a new external file before any mount listing.
The test daemon was stopped without changing the installed daemon or its mounts.
This is one bounded account/drive run. A later isolated
[write probe](benchmarks/google-shared-drive-write-live.json) confirmed create,
exact readback and direct v2 conditional operations in a new owned folder. A
committed replacement initially failed to settle because its saved session
reported a stale version. After the worker gained exact reconciliation for an
uncertain inspection, a follow-up run restored the deterministic fixture and
passed replacement, rename and stale-conflict checks. An isolated
[mounted write probe](benchmarks/google-shared-writable-mount-live.json) then
passed a small create and an interrupted 16 MiB upload, with exact cloud
readback and no duplicate sibling after restart. The subsequent mounted
mutation run passed rename, move, replace and trash for only run-owned content.
Writable Shared Drive preview connections now require an explicit write grant;
broader roles and long sessions remain unverified.
