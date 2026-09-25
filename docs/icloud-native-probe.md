# Native iCloud Drive read probe

This is an **experimental, read-only protocol probe**, not an installed Cirrove
drive or a release claim. It uses Cirrove's own Rust HTTP and SRP implementation;
it does not invoke rclone or inspect another client's configuration, credentials,
mount or service. Apple's general iCloud Drive web protocol is undocumented and
can change without notice.

The probe offers an interactive Apple account sign-in, trusted-device code,
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
rejected. The same parent listing checks ETag and size on both sides:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com' --range-in 'FOLDER::zone::parent-id' 'FILE::zone::opaque-id' 4096 8192 ./downloaded-range
```

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

The keyring value contains Apple session cookies and tokens, never the password.
The key is derived from the account identifier; a restored value is bound to that
identifier and its service and cookie hosts are validated. This path has only a
synthetic round-trip test so far. An expired or rejected session requires a new
interactive sign-in; automatic renewal has not been established.

## Live validation

On 2026-09-25, one account completed the password and trusted-device-code flow,
and the native probe listed its iCloud Drive root with both files and folders.
The first listing attempt hit the previous 30-second HTTP timeout; the retry
with a 90-second listing limit succeeded. An individual-file lookup then returned
HTTP 400. A second read path used the file's known parent listing before and
after downloading one ordinary file; it saved 135,295 bytes and the local file
size matched. The file contents were not inspected. These are single-account,
single-file read-only observations, not proof of repeatability, general account
compatibility or reliable content-version semantics. No Cirrove mount was created.
The validation record contains no account identifier, file names, item IDs,
tokens or response bodies.
The exact-range path has local parser tests but awaits a live response and an
independent byte-for-byte comparison with the previously saved file.
