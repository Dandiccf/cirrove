# Foundation validation

Date: 2026-09-06. Environment: Linux / Arch, Rust 1.98.1.

## Completed locally

- `cargo fmt --all -- --check` — passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — passed.
- `cargo test --workspace --locked` — 16 tests passed; no ignored tests.
- `cargo build --workspace --locked` — both binaries built.
- `python3 scripts/smoke.py` — demo staging/reopen, daemon status, SIGTERM and socket cleanup passed.
- `cargo doc --workspace --no-deps --locked` — generated successfully.
- `systemd-analyze --user verify` — unit template passed with ExecStart pointed at the locally built daemon; the unit was not installed.

The Graph fixtures use a real local HTTP listener and a fake token. They verify
response translation, linked-drive target identity, tombstones, delta completion,
410 handling, 429 cooldown, cancellation while the server stalls, rejected foreign
continuations and disabled redirects. They do not contact Microsoft.

Storage checks reopen interrupted staged refreshes, keep old baseline data visible
until a replacement completes, isolate accounts, reject out-of-order pages and
apply the final occurrence of an item. They simulate interruption by closing and
reopening SQLite; they are not power-loss or filesystem-corruption tests.

Service checks verify status with an idle connected client, refusal to overwrite
an existing socket-path file, failed-refresh continuation and graceful cleanup.
The separate executable smoke test uses temporary private directories only.

## GitHub CI

The [initial CI run](https://github.com/Dandiccf/cirrove/actions/runs/34014890227)
passed on Ubuntu 24.04 for commit `fa22f2189e82ed979cfb340d4aa170b07adfd3c3`.
Formatting, Clippy, all 16 tests, binary build, executable smoke test and Rustdoc
completed successfully. Subsequent publication notes change documentation only.

## Not established by this milestone

- Real Microsoft-account consent, token refresh or sustained Graph operation.
- Automatic SharePoint target-drive discovery and tracking.
- Filesystem latency, large-file memory behavior or file-content integrity.
- Crash-safe uploads, offline writes, pinning, conflict recovery or file-manager UX.
- Google Drive or iCloud operation.
- Installation/autostart on a second machine.

The README and roadmap intentionally keep these capabilities unclaimed.
