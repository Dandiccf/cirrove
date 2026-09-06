# Isolated OneDrive upload validation

This is a developer workflow, not a writable filesystem release. The ordinary
Cirrove mount remains read-only. These checks use real Microsoft Graph writes,
but accept no existing cloud filename or item as a mutation target: each run creates
a unique `Cirrove-Write-Validation-<UUID>` folder and synthetic files inside it.

## Connect a separate test grant

In your own Microsoft app registration, add Microsoft Graph **delegated**
`Files.ReadWrite.All` alongside `User.Read`. Use the same native `http://localhost`
redirect and PKCE setup from [OneDrive setup](onedrive-setup.md). No client secret
or application permission is required. Tenant consent policy can still require an
administrator. Changing the registration alone does not update an existing grant.

Build the current development CLI, then use a separate private state directory:

```sh
cargo build --workspace --locked
mkdir -p -m 700 .local-state/write-validation
./target/debug/cirrove connect \
  --label upload-validation \
  --client-id YOUR_APPLICATION_CLIENT_ID \
  --mount-path "$PWD/.local-state/write-validation/mount" \
  --state-dir "$PWD/.local-state/write-validation" \
  --write-access
```

Select the intended account in Microsoft's browser dialog, grant consent, and
choose the intended Documents drive in the CLI. An explicit `--drive-id` can skip
the drive picker when you already know the selected ID. Check the displayed account
and tenant before running the write command.

`--write-access` is opt-in and requires an explicit state path. Such connections
start disabled, so a daemon will not mount them automatically. Existing accounts
and credential records default to read-only. Token refresh preserves the originally
requested permission mode; it cannot turn a read-only connection into a write
connection. Reauthentication preserves the account's explicit mode.

## Run the basic provider checks

```sh
./target/debug/cirrove validate-onedrive-uploads \
  --label upload-validation \
  --state-dir "$PWD/.local-state/write-validation"
```

The command requires an idle, disabled account with explicit write consent. It
acquires the account's operation and ownership locks before using its credentials.
Your ordinary mount can continue using its separate connection.

Checks currently cover:

- A file larger than one upload fragment, including Unicode and punctuation in its name.
- Independent content readback and SHA-256 verification.
- Failure on a colliding new name, followed by verification of the original bytes.
- Empty-file creation and readback.
- Deferred, conditional replacement of this run's own file.
- Rejection of the previous revision and verification of the replacement bytes.
- A competing edit after a replacement has staged all bytes, but before its final
  commit; the competing content must remain intact and the upload must report conflict.

The folder name is persisted locally before creation. A lost folder-creation
response stops that run rather than reusing an existing directory. Local events,
immutable snapshots and upload receipts remain in `write-checks/<UUID>/` inside the
test state. Session URLs stay exclusively in the Cirrove credential vault.

The test folder and local data are **retained for inspection**, including on failure.
There is no automatic cleanup of failed or uncertain operations. Running the command
again starts a new isolated folder; it does not silently resume the preceding suite.
The underlying transfer journal supports recovery, but an explicit interrupted-suite
resume/cleanup workflow remains to be added.

## Check namespace changes

Use the same separate disabled account and write grant:

```sh
./target/debug/cirrove validate-onedrive-mutations \
  --label upload-validation \
  --state-dir "$PWD/.local-state/write-validation"
```

Each run creates `Cirrove-Namespace-Validation-<UUID>` and accepts no existing cloud
target as an argument. It creates synthetic files, renames and moves them, reads
back their bytes, rejects a colliding destination and stale rename, conditionally
deletes a disposable fixture, and verifies that a stale delete preserves its newer
revision. Folder rename/move checks retain an existing child and verify its bytes.

The deliberate successful deletion applies only to a file created by that run.
Other fixture files, conflicts and local snapshots remain for review. Private
receipts live in `namespace-checks/<UUID>/`. The command does not implement recursive
folder removal, automatic cleanup or writable FUSE. Lost results can need manual
review; it never treats a failed lookup alone as proof of deletion.

## Check a real notification-session renewal

The notification validator can remain connected for the adapter's real renewal
interval, without accelerating its clock:

```sh
./target/debug/cirrove validate-onedrive-notifications \
  --label upload-validation \
  --state-dir "$PWD/.local-state/write-validation" \
  --check-renewal
```

It first performs the existing create/two-rename notification checks on its own
unique fixture. It then waits for the healthy subscription's approximately
50-minute renewal, requires a new subscription to connect, and performs one more
conditional rename. A fresh notification-triggered delta must contain that change.
The initial connection must have lasted at least 49 minutes; waiting for renewal
is bounded at 55 minutes. Keep the process running and the machine awake for the
check. A failed or interrupted session is incomplete evidence, not a passing renewal.

The command holds the separate validation account's owner lock for its lifetime.
Other mutation validators must wait for it to finish. Its private event log records
the waiting, renewal and reconnection stages. It retains its generated cloud folder,
never modifies the ordinary service, and does not validate suspend/resume or
application-visible updates after renewal. Those remain separate gates.

## Check directory freshness through an actual mount

Use the same separate, disabled account and explicit write grant, in a desktop
session with `/dev/fuse`, `fusermount3` and an unlocked Secret Service keyring:

```sh
./target/debug/cirrove validate-onedrive-freshness \
  --label upload-validation \
  --state-dir "$PWD/.local-state/write-validation"
```

The command creates `Cirrove-Freshness-Validation-<UUID>` and temporarily mounts
only that new folder, read-only. A validation-only adapter supplies a baseline
containing just this root and disables notifications; it never requests Graph
delta or file content. Actual directory listings use the normal Graph adapter,
service activity worker, metadata store and kernel FUSE projection.

After completing the first real listing, it creates one child folder and renames
only that child twice, using Unicode names. Each conditional rename first verifies
that the fixture still has its expected name and parent. The check reads directory
names through the mount until the change appears. It records acknowledgement-to-
visibility timing and directory-page counts privately in `freshness-checks/<UUID>/`.
An additional baseline cannot satisfy a passing check. Results apply to this
selected drive and small fixture, not large directories or desktop window updates.

The temporary engine and mount stop on success, error, timeout or handled Ctrl+C.
Generated cloud folders and private evidence remain for review. Account settings,
the ordinary daemon and its existing mount are not changed. This is directory-only
validation: it neither establishes content download performance nor enables writes
through mounted paths. Running it again creates a new fixture; it does not accept
an existing cloud folder as a mutation target.

## Remaining release gates

A passing check covers only its selected account and tested operations. Broader
concurrent changes (rename, move, delete), actual process interruption, personal and
business differences, quota/lock failures, and application saves through writable
FUSE still require validation. If a provider rejects conditional final commit,
the adapter stops and retains the local edit; it does not fall back to unconditional
replacement. See [the save contract](adr/0002-durable-local-edits.md).

Keep live account evidence private. Public regression tests and reports must use
synthetic identities and data.

References: [Microsoft consent and token flow](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow),
[upload-session contract](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0).
