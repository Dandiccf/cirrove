# Development

## Branches and installed builds

Develop coherent changes on a branch based on `origin/main`. Merge a reviewed
increment after its required checks pass; the six product milestones are release
acceptance gates, not a requirement to keep all work off `main` until 1.0.
Record remaining capability and validation gaps in the milestone document.

A GitHub merge or local build does not replace an installed daemon. Installation
and service restart are a separate step. Keep the running build and the tested
source revision in local development records, especially during long observations.

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
checks status, exclusive daemon ownership, recovery after SIGKILL and SIGTERM
cleanup, then removes only its own temporary data.
HTTP tests use loopback fixtures with fake tokens. They never authenticate to Microsoft.

## Long-session observation

After connecting a real account and starting its service, record local health with:

```sh
python3 scripts/observe-service.py \
  --cli "$HOME/.local/bin/cirrove" \
  --output .local-state/service-observation.jsonl
```

The default run lasts 24 hours and samples status once per minute. It creates a
private mode-0600 log and refuses to overwrite an existing file. It records only
aggregate mount/feed states; names, account identifiers, paths, provider messages
and credentials are omitted. Keep even this reduced log private unless its owner
explicitly approves publishing it. The script performs no cloud reads or mutations.

A completed observation is evidence of sampled availability, not proof of all
recovery behavior. Controlled outage/restart tests, real token refresh and file
integrity checks are separate gates. An interrupted observer records an incomplete
run. Its synthetic output/privacy checks run with
`python3 scripts/test-observe-service.py`.

## Controlled Microsoft Graph metadata check

Prefer the [browser sign-in flow](onedrive-setup.md). The older metadata-only
bootstrap below remains available for adapter development. Obtain a short-lived Graph access token
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

This starts the account service, mounts enabled configured drives and initiates
read-only metadata requests. An unlocked desktop keyring is needed for credentials.
See the unit for runtime/state paths and restart policy.

The systemd unit deliberately permits the installed `fusermount3` helper to
perform its privilege transition. Enabling `NoNewPrivileges` would break mounts
on typical desktops. The filesystem remains restricted to the owning user.

To uninstall, stop and disable `cirroved.service`, remove the two installed binaries
and the installed unit, then run `systemctl --user daemon-reload`. Local metadata
under `~/.local/state/cirrove` can be archived or removed separately. Delete only Cirrove-labelled credentials from your desktop keyring to remove saved
sign-in grants, or revoke the application consent in Microsoft account settings.
Do not delete data through a mounted cloud path. This preview makes no cloud
mutations during uninstall.

## Filesystem validation

The default workspace suite also exercises the local upload journal, including
actual child-process termination at durable save/attempt/acknowledgement boundaries.
Run just that component with `cargo test -p cirrove-service --test upload_journal`.
These fixtures use only synthetic local data and do not require a cloud account.
The journal has a separate Graph upload worker and an experimental local writable
FUSE API; ordinary mounts remain read-only. Working-copy and generation tests run
with `cargo test -p cirrove-service --test working_files --test transfers`.
The kernel suite includes synthetic writable save and process-crash checks.

Kernel mount tests also need `/sys/fs/fuse/connections` mounted as `fusectl`, with
per-connection control access for the mount owner. Normal systemd desktops usually
mount this when FUSE is loaded; restricted containers need to provide it explicitly.
The service does not mount or reconfigure this system filesystem automatically.

The default suite skips tests needing kernel FUSE access. Run these explicitly in
a Linux session with `/dev/fuse` and `fusermount3`:

```sh
cargo test -p cirrove-service --test read_only --locked real_ -- --ignored --nocapture
cargo test -p cirrove-service --test writable_session --locked real_ -- --ignored --nocapture
cargo test -p cirrove-service --test read_only --locked synthetic_latency_report -- --ignored --nocapture
```

All mounts and files in these checks are synthetic and live in temporary
directories. No cloud credentials are loaded. The benchmark prints one JSON line
with cold/warm/listing percentiles and process peak RSS; its simulated provider
delay is not a measurement of Microsoft or internet performance.
