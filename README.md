# Cirrove

**Your clouds. One filesystem.**

Cirrove is an open-source Linux cloud-filesystem project, starting with OneDrive
and SharePoint through Microsoft Graph. The goal is responsive, on-demand file
access across providers, with local metadata, disk caching and recoverable saves.
Google Drive is the next planned provider. iCloud Drive requires a separate
compatibility strategy because it lacks an equivalent public general-drive API.

**Status: development foundation, not a usable cloud mount yet.** There is no
FUSE mount, browser sign-in, token refresh, content download, upload, tray or
Nautilus extension in this milestone. The daemon serves local status; it does not
run automatic cloud indexing. Do not replace an existing cloud client with it.

## What works today

- A Rust workspace with provider-neutral identity and paginated change contracts.
- A read-only Microsoft Graph delta adapter, using a persistent HTTP client,
  cancellation, finite request deadlines and an account-wide throttling cooldown.
- SharePoint shortcut target identities are preserved in metadata. Automatic
  discovery/tracking of the linked drives is **not implemented yet**.
- SQLite staging with durable continuation checkpoints. A complete refresh and
  its delta cursor become visible in one transaction. Interrupted refreshes resume;
  replacing an expired baseline keeps the old index visible until completion.
- A Linux user daemon (`cirroved`) and CLI (`cirrove`) communicating over a private
  Unix socket. The API currently exposes status only.
- Tests for interrupted metadata refreshes, account isolation, request budgets,
  Graph response handling, throttling, cancellation and service shutdown.

## Build and try without a cloud account

Requires Linux, Rust 1.98.1 (pinned in `rust-toolchain.toml`), a C/C++ build toolchain,
CMake and pkg-config. Rustup installs the pinned toolchain if needed. SQLite is
bundled; TLS uses Rustls. No FUSE development headers are required for this milestone.

```sh
cargo build --workspace --locked
cargo run --locked --bin cirrove -- demo --state-dir "$(mktemp -d)"
```

The demo uses synthetic metadata only and exercises reopening a partially staged
refresh. No cloud account, file upload or download is involved.

Start the local daemon in one terminal:

```sh
cargo run --locked --bin cirroved
```

Then query it in another:

```sh
cargo run --locked --bin cirrove -- status
```

Defaults follow XDG conventions: `$XDG_STATE_HOME/cirrove` (fallback
`~/.local/state/cirrove`) and `$XDG_RUNTIME_DIR/cirrove/control.sock`. Explicit
`--state-dir` and `--socket` paths support isolated development. Private directories
must have mode `0700`. The daemon never overwrites an existing socket path. For a
manual run left behind by SIGKILL, confirm the old process is gone before removing
its stale socket. The supplied systemd unit manages its runtime directory.

## Developer OneDrive indexing

The Graph adapter can index a **specified drive ID** with a short-lived access
token supplied in an owned, private regular file. This is a developer bootstrap,
not end-user authentication. See [the developer guide](docs/development.md) for
limitations and the least-privilege setup. Credentials from other clients are never
automatically read or migrated.

## Architecture and contributing

| Crate | Responsibility |
| --- | --- |
| `cirrove-core` | Stable identities, metadata provider contract, cancellation and request budgets |
| `cirrove-store` | Transactional metadata staging and completed indexes |
| `cirrove-onedrive` | Microsoft Graph adapter and token-source boundary |
| `cirrove-service` | Daemon, CLI and metadata refresh coordinator |

Read [Architecture](docs/architecture.md), [Roadmap](docs/roadmap.md),
[Validation](docs/validation.md) and [Contributing](CONTRIBUTING.md).

Cirrove is a new codebase. It does not copy or depend on Stratosync or rclone.
The project is informed by practical issues encountered while testing cloud
filesystem integrations; modern components alone are not a reliability claim.

## License

[Apache License 2.0](LICENSE). Copyright 2026 Cirrove contributors.
