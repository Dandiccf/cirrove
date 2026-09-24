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
after the Apple account name. To save one listed file to a **new** local path:

```sh
CARGO_TARGET_DIR="$PWD/.target-icloud-probe" cargo run -p cirrove-icloud --bin cirrove-icloud-probe -- 'your-apple-id@example.com' --read 'FILE::zone::opaque-id' ./downloaded-file
```

Each invocation signs in again. Advanced Data Protection PCS approval, SMS-only
2FA, session persistence, complete large-directory pagination, content revision
semantics and installed FUSE integration are not yet implemented or validated.
The probe fails closed on these cases; it does not silently publish an incomplete
directory or unverified file to Cirrove's cache. The existing Fedora iCloud mount
is untouched.
