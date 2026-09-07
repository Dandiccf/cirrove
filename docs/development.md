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

The interrupted-client fixture needs Python 3 and actual kernel FUSE access:

```sh
cargo test -p cirrove-service --lib --locked filesystem::capacity::real_interrupted_clients_release_namespace_references_and_snapshots -- --exact --ignored --nocapture --test-threads=1
```

Its one test exercises both cold LOOKUP and OPENDIR. A provider-page barrier proves
the request is in flight before SIGKILL is sent only to the synthetic client. Normal
server completion, reference/snapshot cleanup and an offline metadata revisit must
then succeed. This does not inject failure of the reply write itself.

## Namespace capacity baseline

When working in multiple Git worktrees, keep a separate Cargo target directory
for each checkout (the default local `target/` is suitable). A shared
`CARGO_TARGET_DIR` can reuse an incremental test executable from another checkout;
a successful command reporting zero selected tests does not validate new work.
Check the test names and nonzero expected counts before accepting a result.

To measure direct cached name lookups through the actual kernel mount, without
first listing the directory:

```sh
cargo test -p cirrove-service --lib --locked --release --no-run
CIRROVE_NAME_LOOKUP_FILES=500000 timeout 90s \
  cargo test -p cirrove-service --lib --locked --release \
  real_indexed_name_lookup -- --ignored --nocapture --test-threads=1
```

The default CI fixture uses 50,000 files; the explicit command uses 500,000.
It checks 16 different filenames spanning the directory and one missing name,
without directory snapshots, content reads or provider metadata calls. The
combined stat operations must finish within 500 ms; reported RSS/PSS samples
surround that phase and exclude initial indexing. This does not measure a full
directory listing, file content, desktop thumbnail activity or long-session churn.
The [recorded 500k-file lookup run](benchmarks/indexed-name-lookups.json) includes
source hashes, process memory samples and the exact measured scope.

To isolate the metadata Store's read allocations, run the 500,000-entry
single-directory fixture in release mode. Each variant needs its own process:

```sh
cargo test -p cirrove-store --lib --locked --release --no-run
for snapshot in 0 1; do
  for mode in stream collect; do
    CIRROVE_DIRECTORY_SNAPSHOT="$snapshot" CIRROVE_DIRECTORY_READ_MODE="$mode" \
      timeout 240s cargo test -p cirrove-store --lib --locked --release \
      large_directory_read_memory -- --ignored --nocapture --test-threads=1
  done
done
```

This Linux fixture creates metadata in bounded pages and exercises both the delta
index (`snapshot=0`) and persisted foreground-snapshot representation (`snapshot=1`).
It reports read time, RSS/PSS, process high-water mark and database size. The stream
variant asserts under 64 MiB of extra RSS/high-water growth during the read; the
collect variant deliberately retains the returned nodes to expose the remaining
compatibility API cost. Snapshot fixtures are prepared in SQL to avoid contaminating
the read baseline with a full-directory input vector. The fixture does not exercise
FUSE, foreground network publication or full pipeline memory, and needs temporary
disk space for two copies of the metadata plus indexes and WAL.

Run the explicit synthetic 500,000-file benchmark on a machine with several GiB of
free memory and disk space, with the same FUSE prerequisites as the kernel suite:

```sh
cargo test -p cirrove-service --lib --locked --release --no-run
for per_directory in 1000 500000; do
  CIRROVE_NAMESPACE_PER_DIRECTORY="$per_directory" timeout 600s \
    cargo test -p cirrove-service --lib --locked --release \
    namespace_capacity_baseline -- --ignored --nocapture --test-threads=1
done
```

For a smaller pilot, prefix the command with `CIRROVE_NAMESPACE_FILES=10000`.
The fixture creates 500 directories of 1,000 files by default; the second variant
puts all 500,000 files in one directory. For a smaller pilot, the requested
per-directory count must not exceed `CIRROVE_NAMESPACE_FILES`. It generates
metadata in pages, and indexes it through the normal SQLite refresh path. It
traverses the actual temporary mount three times, changing every file's revision
between passes. Each post-pass sample waits for directory handles to close and
prints retained views, map capacity, RSS/PSS, peak RSS and logical snapshot bytes.
The first-entry sample measures snapshot construction with a directory held open;
closed samples assert that its temporary storage reservations returned to zero.
It refuses unexpected
content or foreground metadata requests; it reads no real account or keyring.
Each immediate `closed_after_revision_*` sample is followed by an
`after_invalidation_revision_*` sample after normal kernel invalidation and
reference-aware reclamation leave only the root. Kernel references are not
fabricated or discarded to reach that count. Record both phases: logical view
retirement alone does not establish a process-memory plateau. The checked-in
[parent-lifetime record](benchmarks/namespace-parent-lifetime.json) uses `--release`;
select the same build profile when comparing timings and memory.
The [snapshot-page record](benchmarks/directory-snapshot-pages.json) uses separate
release processes for both directory layouts and records first-entry latency,
logical temporary storage and the remaining process-memory slope.

The test's successful exit means its measurement completed correctly, **not** that
the memory release gate passed. It provides a comparison for namespace lifetime changes.
It does not combine the large-library run with alias/depth, held-mapping or 24-hour
cases in [the namespace gate](adr/0005-namespace-memory.md). Run it explicitly rather
than adding a multi-GiB benchmark to every default CI test run.

CI runs the smaller actual-kernel regression with:

```sh
cargo test -p cirrove-service --lib --locked real_directory_listing_releases_unlooked_up_projections -- --ignored --nocapture --test-threads=1
```

It enumerates 3,000 files, checks that plain listings did not retain thousands of
views, and verifies that an open old file and a newly opened revision keep separate
inodes while further directory listings occur.

### Cold foreground directory publication

Use a dedicated worktree target directory for these synthetic fixtures:

```sh
cargo test -p cirrove-store --test directory_publication --locked
cargo test -p cirrove-store --lib --locked observations::directory_publication::tests::
cargo test -p cirrove-service --lib --locked abandoned_cold_fetch
cargo test -p cirrove-service --lib --locked real_cold_directory_pages_publish -- --ignored --nocapture --test-threads=1
CIRROVE_COLD_DIRECTORY_FILES=500000 cargo test -p cirrove-service --lib --release --locked real_cold_directory_pages_publish -- --ignored --nocapture --test-threads=1
cargo test -p cirrove-store --release --locked directory_publication_capacity -- --ignored --nocapture --test-threads=1
```

The kernel fixture starts with the root route cached and no completed delta index
or cached listing for its large child directory. Its provider generates 1,000 nodes
per page. It verifies cold enumeration, snapshot release, offline revisit and
remount without additional provider requests or content reads. Default size is
10,000 files for CI; the explicit release run uses 500,000. Run memory benchmarks
in separate processes so previous tests do not contaminate the process high-water
mark. The Store-only benchmark separately publishes 500,000 nodes, confirms the
same listing unchanged, then publishes changed metadata. It reports TEMP database
pages separately from process RSS; neither includes a total physical-memory claim.

The production limits include two builders per account and 512 MiB per TEMP
database, covering staged rows, comparison tables and indexes. Journal/transient
files, the persistent metadata database/WAL and kernel page cache are additional.
These are temporary-filesystem allocations; a host that places temporary files
on tmpfs consumes host memory outside the process RSS measurement. No real-provider
reliability, instant first-entry latency or 24-hour memory plateau is established.
See [recorded measurements](benchmarks/directory-publication.json).

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

The [mounted application workload](adr/0004-read-session-efficiency.md#mounted-application-workload-and-measured-tradeoffs)
runs in a separate release CI job. It compares the original, strong-session and
conservative-window paths through actual FUSE and a separate Python reader process.
The strong-session variant now uses conditional windows too; the earlier per-range
results remain in the baseline artifact. Both window paths are exercised
with zero and 20 ms imposed response latency. Use the ADR's explicit commands for
this fixture; it is ignored in the normal suite and does not contact a cloud account.

### Targeted metadata invalidation

```sh
cargo test -p cirrove-store --lib --locked metadata_changes
cargo test -p cirrove-service --lib --locked residency
cargo test -p cirrove-service --lib --locked real_targeted_ -- --ignored --nocapture --test-threads=1
CIRROVE_INVALIDATION_FILES=500000 cargo test -p cirrove-service --lib --release --locked real_targeted_invalidations_preserve -- --ignored --nocapture --test-threads=1
```

The kernel fixture indexes generated metadata, resolves 3,000 files, holds an old
file descriptor, and changes one file. It expects only that file and its parent
attributes to be notified, retaining unrelated cached views. A 100-file delta burst
then runs alongside 16 cached metadata requests with the existing 500 ms navigation
bound. A separate alias fixture checks both target updates and source-link rename,
plus replacement-baseline scope invalidation. Full recovery still retires unused
views through real kernel FORGET, never manufactured reference counts.

Schema 6 adds only the metadata revision covering index; migration and index
validation commit atomically with the version. Earlier binaries cannot reopen
schema 6. Keep a compatible metadata backup for deployment rollback. These fixtures
use isolated state and do not upgrade an installed account.

### Shared projection payloads

The representation benchmark uses the real projection, reverse index and residency
code in one process, without a kernel mount, SQLite or provider calls. It retains
three 12-level routes (ordinary plus two shortcut aliases), takes 32 extra view
clones, then releases references and checks retirement to the root. Run each size
in a fresh release process; process RSS after retirement includes allocator retention.

```sh
cargo test -p cirrove-service --lib --release --locked shared_projection_payload_baseline -- --ignored --nocapture --test-threads=1
CIRROVE_PROJECTION_FILES=500000 cargo test -p cirrove-service --lib --release --locked shared_projection_payload_baseline -- --ignored --nocapture --test-threads=1
```

[Raw results](benchmarks/shared-projection-payloads.json) identify the baseline source
and fixture hash. This is separate from the actual-kernel namespace/invalidation
fixtures above and cannot establish their large-library or long-session gates.

The current resident representation also uses an ordered map, uniquely owned
identity-index strings and a boxed optional link descriptor. Exactly equal live
alias payloads can share the existing node allocation. The representation fixture
and its meaning remain the same; current output additionally reports Node's inline
size, while map-capacity diagnostics elsewhere report null for the ordered map.
[The compact-representation comparison](benchmarks/compact-resident-metadata.json)
records separate release processes and both memory and population-time results.

### Combined namespace traversal and churn

`real_combined_namespace_churn` mounts generated metadata with 12-level paths and
two distinct aliases of a shared tree. The indexed file count is split between
primary and shared collections; both aliases are traversed, so the projected count
is 1.5 times the indexed count. Each of three full passes stats every projected
file through a bounded application queue (128 entries, eight workers by default).
It keeps 24 old file descriptors and three directory snapshots across version changes
and a rename beyond the initial directory buffer, checks their old state, then
verifies inode-preserving offline revisit and remount. No provider calls are allowed.

```sh
# Small correctness fixture, also included in the kernel CI suite.
cargo test -p cirrove-service --lib --release --locked real_combined_namespace_churn -- --ignored --nocapture --test-threads=1
# Full many-directory workload; this stats 750,000 projected files per pass.
CIRROVE_CHURN_FILES=500000 cargo test -p cirrove-service --lib --release --locked real_combined_namespace_churn -- --ignored --nocapture --test-threads=1
# The same library with one large directory per collection.
CIRROVE_CHURN_FILES=500000 CIRROVE_CHURN_PER_DIRECTORY=250000 cargo test -p cirrove-service --lib --release --locked real_combined_namespace_churn -- --ignored --nocapture --test-threads=1
# Sustained active-set churn after the initial three full passes.
CIRROVE_CHURN_FILES=500000 CIRROVE_CHURN_SECONDS=86400 cargo test -p cirrove-service --lib --release --locked real_combined_namespace_churn -- --ignored --nocapture --test-threads=1
```

The default is 4,000 indexed files in 2,000-file groups. `CIRROVE_CHURN_STAT_WORKERS`
accepts 1–16 workers. Each invocation owns a temporary state directory and mount;
its data is removed after successful shutdown. Run long measurements separately
from other mount/latency fixtures and retain the exact source, command, process
handle and complete output. The large traversal can take many minutes.

Sustained mode lasts the requested duration (up to 86,400 seconds) after the initial
traversals. It changes nine items per collection and stats the first group of each
route repeatedly, with up to 30 seconds between rounds. Normal targeted invalidation
participates; this phase does not force full sweeps after each round. The three full
passes and final cleanup explicitly request kernel invalidation to exercise FORGET.
New content versions retain persistent inode history, measured separately from live
views; this runner does not implement history collection.

Output distinguishes reference/index/candidate counts, map capacity, open handles,
snapshot logical bytes, process RSS/PSS and metadata database/WAL sizes and inode rows.
Counts are not shared-payload byte accounting, and map capacity is not an allocation
size. Samples are observations of concurrent state, not one atomic system snapshot.
`navigation_ms` includes the metadata commits, three warm stats and waiting for the
new version's inode; its unchanged limit is 500 ms. The fixture uses zero-length
files and does not exercise content reads or mappings. Small and short-duration
results in [the fixture record](benchmarks/namespace-churn-fixture.json) validate
the runner, not the full 500k/24-hour memory or real-provider acceptance gates.
