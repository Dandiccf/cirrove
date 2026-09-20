# Google Drive preview

Google Drive is implemented as a **read-only My Drive preview with bounded
real-account validation**. It uses Cirrove's shared engine, SQLite staging, disk
cache, pin jobs and FUSE mount. The adapter also implements prepared creation,
conditional binary replacement and exact-ID namespace mutations behind the shared
provider interfaces. Those write transports have synthetic coverage and a bounded
live check on one Cirrove-created fixture, but ordinary Google mounts remain
read-only while the mounted name/collision contract is unresolved.

The live v2/v3 check established the core conditional path: Cirrove observes a
v3 numeric file version, resolves it to the current strong Drive v2 file ETag
immediately before a write, and requires `If-Match` at the mutation boundary. A
resumable upload whose file changed after session creation was rejected at final
commit. The durable transfer worker then replaced and restored an 8 MiB-plus
binary file, while the mutation worker renamed and restored it and rejected a
stale version. Exact ID and SHA-256 readback confirmed the restored bytes.

## What it presents

- Files and folders from My Drive, with paginated listings and polled changes.
- Ordinary binary files through bounded ranges. The adapter compares Google's
  binary `headRevisionId` before and after the download, and checks the response
  range and length before returning bytes to the cache. Sequential reads can use
  the shared read-session contract to stream an 8 MiB window into private cache
  staging under one before/after revision check. A changed final revision rejects
  the whole staged window. The separate numeric file version is exposed only as
  a write observation; it is never sent to Google as an HTTP ETag.
- Shortcut targets in the same user collection. Shared-drive targets remain
  unsupported. A shortcut requiring a resource key becomes a browser link.
- Google-native documents, including Docs and Sheets, as nonempty `.url` browser
  links. They are **not exported document contents**. DOCX/XLSX/PDF exports and
  their version, size and cache semantics remain future work.
- Every projected name carries its complete item ID before the extension:
  `report [item-id].txt`. This initial policy is deliberately stable across
  pagination, rename, restart and duplicate sibling names. `/`, NUL and `%` are
  escaped; long stems are shortened on a UTF-8 boundary to fit 255 bytes.
  It changes the mounted presentation, never the remote filename. More natural
  names with suffixes only for collisions need a separate, stable namespace
  policy; this preview does not pick an arbitrary winner between duplicates.

Shared drives, mounted Google writes, document exports, push webhooks,
resource-key transport and real-account/large-library/long-session acceptance are not claimed.
There is no service-account or another client's credential import.

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

The write connection remains disabled and the normal service does not select the
Google write transport. Drive permits duplicate sibling names, while the mounted
preview gives every name a stable full-ID suffix. A destination scan cannot make
create, rename or move atomic against another Drive client, and the raw-name to
mounted-name behavior has not yet passed a mounted application-save test. Folder
trash also remains a list-then-PATCH operation rather than atomic POSIX `rmdir`.
Those namespace gates must close before an ordinary Google mount can be writable.

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
empty mount folder and your private Desktop OAuth client JSON, then sign in in
the browser. The drive-selection page offers **My Drive**. Google does not offer
an Allow changes switch. You can also connect through the CLI:

```sh
cirrove connect-google --label google \
  --client-json /absolute/path/to/cirrove-google-client.json \
  --mount-path /absolute/path/to/an/empty/folder
```

The write validator uses a separate state directory and connection:

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

The write-grant connection is saved disabled and settings validation refuses to
enable it. Reauthenticate an older validation connection once after this scope
change:

```sh
cirrove reauth google-create-validation \
  --state-dir /absolute/path/to/private/validation-state \
  --write-access
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
validator grant. Account tests require a
Google write-validation connection to remain disabled. Synthetic folder tests
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
