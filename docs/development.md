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
The workspace includes the GTK desktop; see [Desktop preview](desktop.md) for its
system dependencies, isolated fixtures and native-window test. CI also validates
the desktop entry and runs the window test under Xvfb. This is X11 display coverage,
not native Wayland or a full GNOME/Plasma session; those release gates are in the
[product milestone plan](product-milestones.md#5-polished-desktop-experience).

A plain root `cargo build --locked` selects the non-GTK default members, including
the CLI and daemon. Use `cargo build -p cirrove-desktop --locked` for the desktop.
Keep `--workspace` in the full contributor checks above so the desktop is tested
too. Default selection changes the no-flag commands, not explicit `--workspace`.

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
cargo test -p cirrove-service --test read_only --locked real_ -- --ignored --nocapture --test-threads=1
cargo test -p cirrove-service --lib --locked content::windows::tests::graph::kernel::real_ -- --ignored --nocapture --test-threads=1
cargo test -p cirrove-service --test writable_session --locked real_ -- --ignored --nocapture
cargo test -p cirrove-service --test read_only --locked synthetic_latency_report -- --ignored --nocapture
```

All mounts and files in these checks are synthetic and live in temporary
directories. No cloud credentials are loaded. The benchmark prints one JSON line
with cold/warm/listing percentiles and process peak RSS; its simulated provider
delay is not a measurement of Microsoft or internet performance.

The read-only kernel suite runs independent fixtures sequentially in CI because it
includes bounded latency and application-memory assertions. Concurrency within a
fixture remains enabled, including 96 simultaneous thumbnail reads and independent
account/mount checks. File opening has a separate bounded setup phase before the
thumbnail read-burst deadline; cached-directory latency retains its own assertion.

## Namespace capacity baseline

Run the explicit synthetic 500,000-file benchmark on a machine with several GiB of
free memory and disk space, with the same FUSE prerequisites as the kernel suite:

```sh
timeout 600s cargo test -p cirrove-service --lib --locked namespace_capacity_baseline -- --ignored --nocapture --test-threads=1
```

For a smaller pilot, prefix the command with `CIRROVE_NAMESPACE_FILES=10000`.
The fixture creates 500 directories of 1,000 files at its default size, generates
metadata in pages, and indexes it through the normal SQLite refresh path. It
traverses the actual temporary mount three times, changing every file's revision
between passes. Each post-pass sample waits for directory handles to close and
prints retained views, map capacity, RSS/PSS and peak RSS. It refuses unexpected
content or foreground metadata requests; it reads no real account or keyring.

The test's successful exit means its measurement completed correctly, **not** that
the memory release gate passed. It provides a comparison for namespace lifetime changes.
It does not cover the single huge-directory, alias/depth, held-mapping or 24-hour
cases in [the namespace gate](adr/0005-namespace-memory.md). Run it explicitly rather
than adding a multi-GiB benchmark to every default CI test run.

CI runs the smaller actual-kernel regression with:

```sh
cargo test -p cirrove-service --lib --locked real_directory_listing_releases_unlooked_up_projections -- --ignored --nocapture --test-threads=1
```

It enumerates 3,000 files, checks that plain listings did not retain thousands of
views, and verifies that an open old file and a newly opened revision keep separate
inodes while further directory listings occur.

## Read-transport observations

`cirrove inspect-onedrive-read --label ACCOUNT --item ITEM_ID` checks the HTTP
validators of one selected file without mounting or changing cloud content. Use
`--drive DRIVE_ID` for a linked collection and `--state-dir PATH` for a separately
configured development account. The small samples remain in memory; stdout is
sanitized JSON. The account credential broker may refresh its token as usual.
The 60-second probe deadline and content request budget bound the operation.
An initial CLI node lookup precedes that deadline and has the normal metadata
request deadline. See [read-session efficiency](adr/0004-read-session-efficiency.md)
for report semantics, observed behavior and the remaining acceptance boundary.

## Experimental shared read-session validation

```sh
cirrove validate-onedrive-read-session --label ACCOUNT --item ITEM_ID \
  --state-dir /absolute/path/to/separate-development-state
# Add --drive DRIVE_ID for a specifically selected linked collection.
```

This explicit GET-only command reads five samples of at most 256 KiB via a shared
experimental session, then compares each with an independent conservative read.
It prints comparison results, phase times and cumulative adapter request/byte
counters. Subtract adjacent snapshots to obtain phase cost; the initial node lookup
precedes `before`. Authentication and automatic redirects are not counted. No file
contents, URLs, tokens or raw validators are printed. It uses the selected account's
credential broker, which may refresh credentials normally. Use the separate
development identity during live-daemon testing to avoid competing token brokers.
No mount, cloud write or installed-service change is performed.

This does not enable the fast path in ordinary accounts. Synthetic 1 GiB request
counts and the initial live samples are recorded in
[ADR 0004](adr/0004-read-session-efficiency.md). The bounded sequential-window
fallback now has an actual-adapter/shared-cache 1 GiB fixture with JSON request,
staging, timing and sampled-RSS evidence. The CLI above exercises exact ranges,
not windows; use the fixture command documented in the ADR for window checks.
The wider correctness/provider/desktop performance gates remain open.
