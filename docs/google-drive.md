# Google Drive preview

The next provider is Google Drive. This is an implemented **read-only My Drive
preview with synthetic validation**, not a claim of reliable Google-account
operation. It uses Cirrove's existing engine, SQLite staging, disk cache, pin
jobs and FUSE mount. No Google upload or mutation worker is implemented or
selected, even if a token has wider permissions.

## What it presents

- Files and folders from My Drive, with paginated listings and polled changes.
- Ordinary binary files through bounded ranges. The adapter compares Google's
  monotonically increasing file version before and after the download, and
  checks the response range and length before returning bytes to the cache.
  There is no fabricated HTTP ETag or Google write precondition.
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

Shared drives, Google writes, document exports, push webhooks, resource-key
transport and real-account/large-library/long-session acceptance are not claimed.
There is no service-account or another client's credential import.

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
Do not run two daemons against one state directory. No live Google connection
was made during the implementation recorded below.

## Validation and boundary findings, 2026-09-19

`cirrove-googledrive` tests exercise the actual HTTP parser and transport, with
synthetic loopback responses: pagination, pre-scan change frontier, catch-up,
opaque scoped cursors, incomplete searches, removals/trash, quota cooldown,
cancellation, exact content ranges, changed versions and document links.
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
reliability. The live acceptance still needs the owner's Google app and consent,
then listing, byte comparison, refresh, external changes and offline/restart
checks with evidence recorded here.
