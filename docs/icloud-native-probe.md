# Native iCloud Drive read probe

This is an **experimental, read-only integration**, not an installed Cirrove
drive or a release claim. It uses Cirrove's own Rust HTTP and SRP implementation;
it does not invoke rclone or inspect another client's configuration, credentials,
mount or service. Apple's general iCloud Drive web protocol is undocumented and
can change without notice.

The metadata probe offers an interactive Apple account sign-in, trusted-device code,
root/folder listing by opaque Drive item ID, and downloads of ordinary files up
to 16 MiB. A download checks the item's ID, ETag and size before and after the
transfer, bounds memory, and refuses to overwrite a local destination. Those
checks are a conservative first read; they do **not** yet establish that Apple's
ETag is a reliable content revision for Cirrove's mounted cache.

From the repository root:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com'
```

The terminal prompts for the password and, when required, a trusted-device code.
Neither is passed as a command argument or saved by this probe. It prints one
line per root item: kind, size, opaque ID and name. To list a folder, pass its ID
after the Apple account name. To save one listed file to a **new** local path,
provide the ID of the folder that contained it:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com' --read-in 'FOLDER::zone::parent-id' 'FILE::zone::opaque-id' ./downloaded-file
```

The probe also has an individual-item lookup path, though it returned HTTP 400
for the live file tested here:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com' --read 'FILE::zone::opaque-id' ./downloaded-file
```

To test an exact nonzero byte range, use the known parent and pass decimal
`OFFSET` and `LENGTH` (at most 4 MiB). The response must identify the exact
requested bounds and full file size in `Content-Range`; a full-file response is
rejected for a proper subrange. When the request covers the entire file, an
exact-size HTTP 200 response is also accepted. The same parent listing checks
ETag and size on both sides:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com' --range-in 'FOLDER::zone::parent-id' 'FILE::zone::opaque-id' 4096 8192 ./downloaded-range
```

To check whether a content edit changes Apple's ETag, choose a **small test
file you own**, then run this in one terminal session:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com' --revision-check-in 'FOLDER::zone::parent-id' 'FILE::zone::opaque-id'
```

After it reports a baseline, change that test file's **contents** on
iCloud.com and press Enter. The probe compares bounded content hashes and
ETags for up to one minute. It prints neither the content, hashes nor IDs and
does not modify the cloud file itself. One successful comparison is evidence
for that one revision transition, not a guarantee for every iCloud file.

By default, each invocation signs in again. Advanced Data Protection PCS approval, SMS-only
2FA, complete large-directory pagination, content revision
semantics and installed FUSE integration are not yet implemented or validated.
The probe fails closed on these cases; it does not silently publish an incomplete
directory or unverified file to Cirrove's cache. The existing Fedora iCloud mount
is untouched.

An experimental session checkpoint can now be saved in Cirrove's existing desktop
Secret Service keyring and reopened without entering the Apple password again:

```sh
cirrove-icloud-probe 'your-apple-id@example.com' --save-session
cirrove-icloud-probe 'your-apple-id@example.com' --resume-session
```

The `--resume-session` option can also follow a folder, file or range operation,
so subsequent read tests can reuse the saved session without another password
or trusted-device-code prompt. The standalone save/resume checks print only a
root-item count, not item names or IDs.

The experimental adapter's metadata walker can walk at most the specified number of
folder pages and print counts without exposing names or IDs:

```sh
cirrove-icloud-probe 'your-apple-id@example.com' --snapshot-pages 32 --resume-session
```

It preserves a bounded continuation between pages in memory for this probe. A
page limit reports an incomplete scan; it never publishes a partial tree as a
completed Cirrove index. The shared service now stages full-snapshot feeds and
keeps the old visible index until a complete new round, but the iCloud adapter
is not yet installed in that service. The isolated foreground mount below uses
the separate on-demand path.
The snapshot continuation visits folder IDs in stable order and fingerprints a
canonicalized listing, so an unchanged folder can be returned in a different
order without restarting the walk. Changed metadata still expires the cursor.
The probe reports counts every 16 pages during a long scan. These lines are
progress only: ending at the page limit still means the tree is incomplete.

The adapter also implements on-demand folder pages and bounded exact-range
reads for the shared read contract. A mounted read checks the published node's
ETag and size against the live parent listing before and after the range.
For large accounts, its separate on-demand feed seeds only the root and lets
the shared directory cache fetch complete folders as they are opened. A listing
whose announced item count does not match its returned children is reported as
an incomplete provider response, rather than as an offline connection. A folder
without an announced count is refused for the same reason. This
avoids making the 94,154-node incomplete scan a prerequisite for first
navigation; it does not provide a complete offline account index.
This path has synthetic checks but no live revision-change validation. Apple's
ETag behavior, cold lookup of an unknown item, session retention and account
scale still block installing it as a Cirrove drive.

## Isolated read-only File Manager mount

The feature-gated foreground mount probe connects this adapter to Cirrove's
shared Engine and FUSE filesystem without adding an account to the installed
service. It creates private metadata and cache state under the **iCloud
worktree's** `.local-state/icloud-mount-preview/`, not the running Cirrove
installation. Its Apple session remains in the probe process and is discarded
when that process exits. The mount is explicitly read-only; do not use it for
files that have no other copy. A private disk-backed `TMPDIR` on the same
filesystem as the worktree is required for SQLite spill files.

From the iCloud worktree, after creating a private disk-backed temp directory:

```sh
export TMPDIR="$HOME/.cache/cv-ic2"
export SQLITE_TMPDIR="$TMPDIR"
CARGO_TARGET_DIR="$PWD/.target-icloud-feasibility" cargo run -p cirrove-service --features icloud-probe --bin cirrove-icloud-mount-probe -- 'your-apple-id@example.com'
```

Confirm `TMPDIR` is disk-backed with `findmnt -T "$TMPDIR"` before starting.
The terminal prompts locally for the regular Apple account password and trusted
device code. Before mounting, it requires a complete live root listing from
Apple. It prints the isolated mount path only after FUSE starts. Open that
folder in Files; press Ctrl+C in the same terminal to unmount. Do not remove a
preview directory while it is mounted. Each run keeps a private manifest and
state for diagnosis; an `exited` marker records every process exit, including
failed sign-in, while `unmounted` records a completed mount shutdown. The probe
does not edit an existing Fedora iCloud mount or the
installed Cirrove service. The foreground process must remain running for
browsing; this is a validation tool, not an installed iCloud account flow.

The service now has a read-only account constructor and a lazy
Cirrove-keyring-backed adapter. The branch exposes an explicitly experimental
`cirrove connect-icloud` terminal command and an iCloud choice in the
connection window. The window collects the Apple password and code locally;
both re-sign-in and creation of a new isolated account through the window have
passed one live validation each. No existing installation is
migrated. For isolated validation, pass a
private test `--state-dir` instead of using the installed daemon's state.
`cirrove reauth LABEL` repeats native Apple sign-in for a configured iCloud
account without replacing its item index. Both commands request the regular
Apple account password and trusted-device code in the local terminal; neither
accepts a password as a command argument. The connection remains read-only.
Settings validation rejects writable iCloud accounts and roots other than the
opaque Apple Drive root. The metadata feed requires the keyring session and a
complete live root listing before publishing its synthetic root. One isolated
service restart has restored a newly saved session; expiry and renewal still
need validation before this becomes a normal connection choice.

The keyring value contains Apple session cookies and tokens, never the password.
The key is derived from the account identifier; a restored value is bound to that
identifier and its service and cookie hosts are validated. An expired or rejected
session requires a new interactive sign-in. Apple HTTP 401/403 responses from
Drive metadata and download lookup now reach the service as authentication errors
instead of generic temporary failures. Automatic renewal has not been established.

## Live validation

On 2026-09-25, one account completed the password and trusted-device-code flow,
and the native probe listed its iCloud Drive root with both files and folders.
The first listing attempt hit the previous 30-second HTTP timeout; the retry
with a 90-second listing limit succeeded. An individual-file lookup then returned
HTTP 400. A second read path used the file's known parent listing before and
after downloading one ordinary file; it saved 135,295 bytes and the local file
size matched. The file contents were not inspected. These are single-account,
single-file read-only observations, not proof of repeatability, general account
compatibility or reliable content-version semantics. No Cirrove mount was created
at that stage.
The validation record contains no account identifier, file names, item IDs,
tokens or response bodies.
The same account then saved a Cirrove-owned session in the desktop keyring. A
separate process restored it without a password or code prompt and listed 28 root
items. Using that restored session, the probe read 8,192 bytes starting at offset
4,096 and verified the exact range response and unchanged surrounding metadata.
Those bytes matched the same slice of the previously saved complete file byte for
byte. This is a single-file, single-session observation; renewal after expiry and
content-version behavior under concurrent changes remain untested.
On a later scan attempt, the saved keyring entry returned zero bytes, so the
live full-snapshot walk did not begin. A separate synthetic Secret Service
write/read round-trip succeeded; the cause of the empty older entry remains
unknown. Re-sign-in and retention across another process restart need validation.
With a fresh interactive sign-in, a subsequent 256-page metadata walk reached
94,154 nodes and then reported the configured page limit, not a completed
snapshot. The account has substantially more metadata than the first 32-page
probe exposed; the total size and time to complete remain unknown. The current
probe did not save a continuation across processes, and no partial tree was
published to a Cirrove mount.

On 2026-09-25, the experimental `connect-icloud` command saved a fresh
read-only account in private test state. A separate `cirroved` instance with
its own control socket mounted that account through the normal manager and
FUSE path. The mounted root listed 30 entries. Two previously selected files
read fully through the mount, with 19 and 135,295 bytes matching their visible
sizes. A local create attempt returned `EROFS`; it did not reach Apple. After a
clean service stop and restart, the same keyring-backed account mounted without
another password or code prompt, listed 30 root entries and read the 19-byte
test file again. This proves one isolated service lifecycle and two file reads,
not long-term session retention, account-wide completeness, refresh latency or
general file compatibility. The installed daemon and other mounts were not
restarted or changed. After a user edit to the existing small test file on
iCloud.com, the first mounted read rejected the old revision with `ESTALE`.
Opening the root again and retrying read the new 42-byte version (previously
19 bytes). This demonstrates rejection and recovery for one live content
change, not a bound on refresh latency or behavior across concurrent edits.

The same isolated account was re-authenticated through Cirrove's GTK window.
The user completed its local sign-in without sharing secrets with the agent;
the service then unmounted and remounted that account. The
remounted account listed its root and read the 42-byte test file. A subsequent
service restart restored the new keyring session without another prompt and
again listed 30 root entries and read that file. This checks one manual
re-sign-in and restart, not automatic renewal after Apple expires a session.

A second private test state exercised first-time account creation through the
GTK connection window. Its default mount folder was new and empty. The user
completed sign-in locally; the service showed one ready iCloud account and a
read-only mount. The root listed 30 entries and the existing 42-byte test file
read fully. After a clean stop and restart, the new account mounted without
another prompt and both checks passed again. The second test service and mount
were then stopped. This validates one window-created account, not unattended
renewal, long sessions or compatibility with other Apple accounts.

On a later start on 2026-09-25, both private test states entered
`sign_in_required` immediately. Their mounts could still display 30 cached root
names; that was not evidence of a working Apple session. An exact search of
Cirrove's desktop Secret Service entries found neither account's credential ID.
The cause of their disappearance is not established. A short synthetic keyring
entry survived a start and clean stop of the isolated service, so that lifecycle
alone did not reproduce the loss. Both test services were stopped without
touching the installed daemon. Durable session retention remains a beta gate;
the successful immediate restarts above do not close it.
