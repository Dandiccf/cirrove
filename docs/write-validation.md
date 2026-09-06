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
