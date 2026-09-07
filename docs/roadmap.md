# Roadmap

The six remaining release milestones are tracked in
[OneDrive 1.0 product milestones](product-milestones.md). The stages below retain
the history and acceptance criteria for the existing engineering foundation.

Milestones are acceptance gates, not release dates. Stages 1–3 form the first
read-only OneDrive preview. Implementation and validation are tracked separately
in [Validation](validation.md); none is complete merely because it compiles.

## 0 — Foundation

- [x] Independent Apache-2.0 Rust workspace and provider boundaries.
- [x] Graph adapter, atomic staged indexes and resumable checkpoints.
- [x] Private status service, CLI, tests and systemd template.
- [x] Publish the repository and observe successful initial GitHub CI.

## 1 — Accounts and authentication

- [x] Browser OAuth with PKCE, state/nonce and signed OIDC identity validation.
- [x] Serialized token refresh, rotation storage and stale-401 protection.
- [x] Desktop Secret Service credentials and atomic non-secret settings.
- [x] Own app registration, visible account/tenant/drive selection and reauthentication.
- [ ] Real Microsoft consent and refresh through a configured Cirrove registration.

A shared Cirrove registration is optional future deployment work. It is not
currently bundled; another project's client ID is never substituted.

## 2 — Persistent metadata service

- [x] Per-account worker ownership, per-drive delta cursors and structured status.
- [x] Bounded retry/cooldown, cancellation and persisted last-success state.
- [x] Linked-drive discovery, projected shortcut identities and bounded traversal.
- [x] Cached browsing, foreground cold-directory fetches and kernel invalidation.
- [ ] Verify linked-library deletion, duplicates, cycles and revoked permissions
      across real OneDrive/SharePoint tenants, including folder-only sharing.
- [ ] Real-account read-only operation over at least 24 hours, network loss,
      credential expiry and service restart. Record recovery evidence.

## 3 — Read-only filesystem

- [x] Linux FUSE adapter with stable directory/content-version inodes and independent
      metadata/content capacity, including bounded content-read admission.
- [x] Version-checked ranged reads, request coalescing and bounded disk block cache.
- [x] Checksum verification, eviction and interrupted cache-publication recovery.
- [x] Real local FUSE reads, large offsets, offline restart, read-only enforcement
      and ejection/remount against synthetic data; shared/private mappings and
      isolation of old/new open file revisions.
- [x] Synthetic cold/warm latency and 96-reader thumbnail burst measurements.
- [ ] Replace independent per-block Graph validation with shared version-bound
      read sessions and bounded transfer windows. Prove representation identity,
      renewal safety and measured request reduction; this is a OneDrive 1.0 blocker.
      See [read-session efficiency](adr/0004-read-session-efficiency.md).
- [ ] Record reproducible cold/warm p50/p95 latency, API requests, peak memory and
      thumbnail-storm behavior. Separate local fixtures from real-provider results.
- [ ] Validate ordinary desktop applications and long-lived open files on real data.

## 4 — Safe writes and offline controls

- [ ] Explicit local durability and cloud-acknowledgement states.
- [ ] Persistent upload journal, fsync ordering and atomic cache publication.
- [ ] Conditional writes, resumable uploads, conflicts and idempotent retries.
- [ ] Correct rename/delete behavior, partial permissions and content rewrites.
- [ ] Pin/unpin policy, cache reservations and offline availability semantics.
- [ ] Fault injection at every journal transition; verify original and edited bytes.

## 5 — Desktop experience and distribution

- [ ] GTK4/libadwaita settings, tray, Nautilus badges and context actions.
- [ ] Clear connection removal, cache cleanup and recovery guidance.
- [ ] Arch/AUR packaging, reproducible releases and supported-version policy.

## 6 — Additional providers

- [ ] Google Drive adapter, shared drives and explicit Docs/Sheets export behavior.
- [ ] iCloud feasibility, isolated compatibility adapter and visible limitations.

Modern components do not establish production readiness. Sustained real-provider
use and independently reviewable recovery results remain release requirements.
