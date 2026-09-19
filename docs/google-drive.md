# Google Drive preview

The next provider is Google Drive. This is an implemented **read-only My Drive
preview with synthetic validation and a first real-account smoke check**, not a
claim of reliable Google-account operation. It uses Cirrove's existing engine, SQLite staging, disk cache, pin
jobs and FUSE mount. A create-only upload transport exists behind the provider
interface and is tested with synthetic HTTP. The service does not select it,
request write permission or expose a writable Google mount, even if a token has
wider permissions.

## What it presents

- Files and folders from My Drive, with paginated listings and polled changes.
- Ordinary binary files through bounded ranges. The adapter compares Google's
  monotonically increasing file version before and after the download, and
  checks the response range and length before returning bytes to the cache.
  Sequential reads can use the shared read-session contract to stream an 8 MiB
  window into private cache staging under one before/after version check. A
  changed final version rejects the whole staged window. There is no fabricated
  HTTP ETag or Google write precondition.
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

Google Drive permits duplicate sibling names and `files.update` does not expose
the conditional ETag replacement used by the OneDrive adapter. Cirrove therefore
does not map its OneDrive write behavior onto Google or request write consent.

The create transport uses `files.generateIds` before any mutating request. The
provider-neutral worker stores that opaque prepared identity in the credential
vault before asking Google to start a resumable session. The adapter sends 8 MiB
chunks, requires every non-final chunk to be a multiple of 256 KiB, resumes at
Google's exact reported byte offset and refuses a reply that claims bytes beyond
the submitted range. Session URLs are accepted only on the configured Google
origin and upload path; they remain in the vault checkpoint and are never sent as
Bearer credentials or written to SQLite and diagnostics.

An expired session can be replaced once during verification while retaining the
same generated file ID. The old session URL is removed before that prepared
checkpoint is saved. Reconciliation addresses that exact ID and streams the
remote content through the read adapter to compare its SHA-256 digest. Synthetic
HTTP and transfer-worker faults cover persistence before mutation, a lost
session, partial remote offsets, receipt identity, content reconciliation and a
foreign session URL. These checks establish the local protocol behavior; no live
Google mutation has been run.

This transport is create-only and remains disconnected from the service. Drive
allows duplicate sibling names, so Cirrove still lacks the atomic collision rule
required by `UploadIntent::Create`. Safe replacement is also unresolved because
`files.update` has no OneDrive-style conditional ETag. Those two namespace and
conflict decisions, write consent and explicit live mutation validation must be
completed before a writable Google mount can be offered. See Google's
[pre-generated ID guidance](https://developers.google.com/workspace/drive/api/guides/manage-uploads)
and [`files.update` reference](https://developers.google.com/workspace/drive/api/reference/rest/v3/files/update).

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
5. Cirrove requests `openid email profile` and
   `https://www.googleapis.com/auth/drive.readonly`. Google's `drive.file`
   scope covers selected/app-created files and is insufficient for this
   filesystem's whole-drive index. Readonly Drive access is a restricted scope;
   Google sets the publication and verification requirements for an app offered
   to other users. See [Google's scope guide](https://developers.google.com/workspace/drive/api/guides/api-specific-auth).

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

## Validation and boundary findings, 2026-09-19

`cirrove-googledrive` tests exercise the actual HTTP parser and transport, with
synthetic loopback responses: pagination, pre-scan change frontier, catch-up,
opaque scoped cursors, incomplete searches, removals/trash, quota cooldown,
cancellation, exact content ranges, changed versions and document links.
The streamed-window scenario asserts that bytes are accepted only when the final
Google version still matches and rejected when it changes after transfer.
Removing only that final lookup/check made the exact scenario fail because its
expected final metadata request never occurred; restoring it passes both arms.
Authentication tests exercise Google issuer/audience/nonce/expiry/signature and
verified-email checks, token-subject binding on refresh, serialized rotation,
stale-401 protection and refusal to use a rotation that the keyring cannot save.
The Microsoft authentication tests remain part of the same suite.

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

## First real-account connection, 2026-09-19

The owner created and authorized a separate Google Desktop OAuth app in Testing,
with their own account as its only test user. Drive access is read-only. The
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
