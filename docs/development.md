# Development

## Local checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/smoke.py
cargo doc --workspace --no-deps --locked
```

The smoke test creates a temporary private directory, runs the compiled CLI/daemon,
checks status and SIGTERM cleanup, then removes only its own temporary data.
HTTP tests use loopback fixtures with fake tokens. They never authenticate to Microsoft.

## Controlled Microsoft Graph metadata check

This milestone does not implement login. Obtain a short-lived Graph access token
through your own approved developer application. Use the least delegated permission
needed for the selected drive (Graph documents Files.Read for the user's own drive;
additional libraries may require wider permissions and tenant consent). Store it in
a private regular file without pasting it into command arguments, Git or issue logs.
Never extract another application's credentials as an automatic setup step.

```sh
cargo run --locked --bin cirrove -- index-onedrive \
  --account personal-test \
  --drive YOUR_DRIVE_ID \
  --token-file /absolute/private/path/access.token \
  --state-dir /absolute/private/path/cirrove-test
```

The token file must be owned by the current user, mode 0600 or stricter and no larger
than 64 KiB. The state directory must be owned by the current user and mode 0700.
The account flag is a local namespace label; **this bootstrap does not verify the
signed-in account identity**. Use a new label/state directory when changing identities.
The selected drive is not discovered automatically. The command indexes metadata
only; it does not read file content or issue write requests. Tokens do not refresh.

Repeated calls use the stored delta cursor. Interrupted pagination resumes. A 410
error returns CursorExpired; use `--reset` to stage a replacement baseline while
keeping existing visible metadata until completion. There is no automatic retry
scheduler: wait for the stated cooldown before retrying a throttled request.

## Optional user-service installation (after development testing)

The repository does not install or enable anything automatically.

```sh
cargo build --release --locked
install -Dm755 target/release/cirrove ~/.local/bin/cirrove
install -Dm755 target/release/cirroved ~/.local/bin/cirroved
install -Dm644 packaging/systemd/cirroved.service ~/.config/systemd/user/cirroved.service
systemctl --user daemon-reload
systemctl --user enable --now cirroved.service
cirrove status
```

This starts only the metadata status service. It does not mount drives or initiate
cloud requests. See the unit for runtime/state paths and restart policy.

To uninstall, stop and disable `cirroved.service`, remove the two installed binaries
and the installed unit, then run `systemctl --user daemon-reload`. Local metadata
under `~/.local/state/cirrove` can be archived or removed separately. There is no
cloud data to delete during uninstallation in this milestone.
