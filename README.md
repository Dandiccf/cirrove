# Cirrove

[![CI](https://github.com/Dandiccf/cirrove/actions/workflows/ci.yml/badge.svg)](https://github.com/Dandiccf/cirrove/actions/workflows/ci.yml)

**Your clouds. One filesystem.**

Cirrove is an Apache-2.0 Linux cloud-filesystem project, starting with OneDrive and
linked SharePoint libraries through Microsoft Graph. It keeps metadata locally and
fetches file content on demand into a bounded disk cache.

**Status: read-only development preview, under validation.** Browser authentication,
metadata workers and a FUSE mount are implemented. Local synthetic tests exercise
actual filesystem reads and recovery. Real Microsoft consent, sustained operation
and provider latency still require validation. Do not replace a trusted cloud
client with this preview. Writable mounts, pins, tray UI and Nautilus badges are not
implemented. A separate developer upload worker is undergoing validation.

## Current implementation

- Browser Microsoft OAuth with PKCE, signed identity verification, account/tenant
  display and explicit drive selection. Use your own app registration.
- Secret Service credential storage, serialized refresh, stale-token protection
  and account-specific reauthentication.
- Persistent delta workers, transactional SQLite staging, resumable pagination,
  backoff and an account-wide provider cooldown. Graph Socket.IO notifications
  now wake the incremental metadata feed; periodic checks remain a fallback.
  Provider delivery latency is separate from Cirrove's reaction time; see
  [change notifications](docs/adr/0003-change-notifications.md).
- Linked-drive discovery and shortcut projection with separate target identities.
  Folder-only SharePoint sharing and revoked targets still need live validation.
- Read-only FUSE projection with persistent directory and content-version inodes,
  including shared read-only and private memory mappings. Directory requests have
  capacity reserved separately from a bounded queue of content reads.
- Version-checked 4 MiB range cache, concurrent-request coalescing, checksums,
  bounded eviction and interrupted-publication recovery.
- Private status socket, desired mount state, accidental-ejection remount and
  graceful worker/session shutdown. Settings and metadata persist across runs.
- Durable upload snapshots, keyring-backed session checkpoints and bounded Graph
  upload fragments, exercised with synthetic HTTP/fault fixtures. An explicit
  [isolated write check](docs/write-validation.md) is available for live validation;
  these workers are not enabled in ordinary mounts. Conditional rename, move,
  folder creation and file deletion now share durable ordering with uploads;
  their isolated checks preserve collisions and uncertain results.

These are implementation capabilities, not a production-readiness claim. See the
[validation record](docs/validation.md) for what has actually been tested.

## Build and try

Requires Linux, Rust 1.98.1 (pinned), a C/C++ build toolchain, CMake and pkg-config.
Rustup installs the toolchain if necessary. SQLite is bundled; HTTPS uses Rustls.
Mounting additionally needs `/dev/fuse`, `fusermount3` (`fuse3` on Arch), and a kernel
advertising `FUSE_DIRECT_IO_ALLOW_MMAP`. The mount rejects missing support explicitly.
Browser sign-in needs `xdg-open` and a desktop Secret Service keyring.

```sh
cargo build --workspace --locked
cargo run --locked --bin cirrove -- demo --state-dir "$(mktemp -d)"
```

The demo uses synthetic metadata and no cloud account. To create a personally owned
Microsoft development environment, follow [Personal Microsoft/Azure/Entra setup](docs/microsoft-developer-setup.md).
If you already have an Entra directory, use the shorter [OneDrive setup](docs/onedrive-setup.md).
Once an account is connected, start its read-only mount with:

```sh
./target/debug/cirroved
```

From another terminal:

```sh
./target/debug/cirrove status
./target/debug/cirrove accounts
```

Cirrove uses `$XDG_STATE_HOME/cirrove` (fallback `~/.local/state/cirrove`) and
`$XDG_RUNTIME_DIR/cirrove/control.sock`. Explicit `--state-dir` and `--socket` paths
support isolated development. State directories must be private (`0700`).
The daemon holds an ownership lock and recovers a disconnected control socket
after a crash. Disconnected FUSE mounts are recovered only when their account
identity matches; live mounts and unrelated paths are preserved.

Existing cloud clients, mounts and credentials are not imported or modified.
The systemd template is supplied separately and is not installed by a build.

## Architecture and contributing

| Crate | Responsibility |
| --- | --- |
| `cirrove-core` | Provider-neutral identity, metadata/read/upload contracts, cancellation and request budgets |
| `cirrove-store` | Transactional metadata, observations, persistent inodes and cache index |
| `cirrove-onedrive` | Microsoft Graph metadata, version-checked ranged reads and experimental resumable uploads |
| `cirrove-auth` | Microsoft browser authentication, keyring and refresh broker |
| `cirrove-service` | Daemon, CLI, account workers, FUSE projection and content cache |

Read [Architecture](docs/architecture.md), [Roadmap](docs/roadmap.md),
[OneDrive 1.0 milestones](docs/product-milestones.md),
[Development](docs/development.md) and [Contributing](CONTRIBUTING.md).

Google Drive is the next planned provider. iCloud requires a separate compatibility
assessment because its API situation differs. Cirrove does not copy or depend on
Stratosync or rclone; lessons from those integrations inform the recovery tests.

## License

[Apache License 2.0](LICENSE). Copyright 2026 Cirrove contributors.
