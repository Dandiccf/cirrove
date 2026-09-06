# Roadmap

Milestones are acceptance gates, not release dates.

## 0 — Foundation (current)

- [x] Independent Apache-2.0 Rust workspace and architecture boundaries.
- [x] Metadata provider contract, Graph adapter and injected token source.
- [x] Atomic staged indexes, restart continuation and account isolation tests.
- [x] Private status service, CLI and systemd unit template.
- [x] Local mock Graph tests and repeatable CI checks.
- [ ] Publish and observe the first successful GitHub CI run.

## 1 — OneDrive account and metadata service

- [ ] Browser OAuth/PKCE, refresh serialization and Secret Service credential storage.
- [ ] Show and verify account, tenant and selected drive; own/project app registration.
- [ ] Account configuration, job ownership and structured health/events API.
- [ ] Persistent per-drive delta workers and bounded retry/cooldown scheduling.
- [ ] SharePoint shortcut discovery/projection, duplicate/cycle and revoked-target handling.
- [ ] Expired cursor recovery and invalidation of affected directories.
- [ ] Real-account read-only validation over at least 24 hours, including network loss.

## 2 — Read-only filesystem

- [ ] Select FUSE 3-compatible Rust adapter; define inode and callback lifetime rules.
- [ ] Cached directory lookup with background refresh and bounded wait for cold paths.
- [ ] Version-aware ranged reads, durable disk cache and coalesced concurrent requests.
- [ ] Pins, cache eviction and file-manager invalidation.
- [ ] Test large files with bounded memory, thumbnail storms and slow providers.
- [ ] Record cold/warm p50/p95 latency and API requests per operation; publish results.

## 3 — Recoverable writes

- [ ] Explicit local durability and cloud acknowledgement semantics.
- [ ] Persistent upload journal, fsync ordering and atomic cache publication.
- [ ] Conditional writes, resumable uploads, conflict preservation and retry idempotency.
- [ ] Correct rename/delete behavior, partial permissions and provider content rewrites.
- [ ] Fault injection at every journal transition; verify original and edited bytes.

## 4 — Desktop and additional providers

- [ ] GTK4/libadwaita account settings, tray, Nautilus badges and context actions.
- [ ] Google Drive adapter with shared drives and explicit document export behavior.
- [ ] iCloud compatibility feasibility, with limitations visible before connection.
- [ ] Arch/AUR packaging, reproducible release process and supported-version policy.

No milestone is complete solely because unit tests pass. Production readiness
requires real-provider tests, sustained use and independently reviewable recovery evidence.
