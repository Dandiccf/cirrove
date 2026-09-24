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
- [ ] Bound namespace residency independently of the content cache: implement
      reference-aware reclamation, paged directory snapshots and targeted
      invalidation; pass the [500k-file / long-session memory gate](adr/0005-namespace-memory.md).

## 4 — Safe writes and offline controls

- [ ] Explicit local durability and cloud-acknowledgement states.
- [ ] Persistent upload journal, fsync ordering and atomic cache publication.
- [ ] Conditional writes, resumable uploads, conflicts and idempotent retries.
- [ ] Correct rename/delete behavior, partial permissions and content rewrites.
- [ ] Pin/unpin policy, cache reservations and offline availability semantics.
- [ ] Fault injection at every journal transition; verify original and edited bytes.

## 5 — Desktop experience and distribution

- [x] Initial GTK4/libadwaita saved-account overview, service status and mount controls.
- [ ] Complete native setup/settings, tray, Nautilus badges and context actions.
- [ ] Clear connection removal, cache cleanup and recovery guidance.
- [ ] Native Arch/AUR, Debian/Ubuntu `.deb` and Fedora `.rpm` packaging, APT/COPR
      updates, reproducible releases and supported-version policy. Pass clean
      installation, login/reboot, upgrade and removal on each declared family;
      see [Distribution](distribution.md).

## 6 — Additional providers

- [ ] Google Drive: [writable My Drive and Shared Drive previews](google-drive.md)
      have bounded live read, create, edit, rename, move, trash, conflict,
      mounted-write and recovery evidence. A selected Shared Drive was also
      mounted and written through the reviewed branch without the earlier test
      override. Native Docs/Sheets remain read-only packages with DOCX/XLSX,
      PDF and OpenDocument exports. Direct GET-only PDF/OpenDocument exports
      passed on two test items; one installed-mount read probe passed twice
      after an unexplained first-attempt assertion.
      A later exact-fixture probe preserved a Doc heading, bold/italic text and
      bullets plus a Sheet's frozen bold header, currency format, SUM/AVERAGE
      formulas and column chart through all six direct exports and a fresh
      selected-Shared-Drive mount. Only the two registered fixtures were then
      trashed. Arbitrary-document, comments/suggestions, macro and concurrent
      native-write fidelity remain outside that bounded result.
      Native writeback failed a stale-update integrity
      probe and remains disabled. A subsequent installed first-open probe passed
      for the same Doc and Sheet after the cached-package correction. A direct
      Drive `files.download` probe exported a 22.3 MB XLSX that `files.export`
      rejected at its 10 MiB limit, but a 38.8 MB PDF of that Sheet took about
      163 seconds. Large native exports are not yet integrated into Cirrove's
      eager three-format package path. One isolated Shared Drive hour completed 61
      matching reads with a fresh feed. A separate fresh private mount kept its
      content cache empty for an hour through one accepted indexing interval,
      then read an exact uncached 16,777,263-byte fixture in 5.247 seconds.
      Restricted roles, longer sessions, larger exports and broader fidelity
      remain open. The
      Cirrove OAuth app is still in Testing and needs public branding and
      Drive-scope verification before general Google-account onboarding.
- [ ] iCloud Drive: complete the [direct Linux read feasibility gate](adr/0016-icloud-drive-needs-a-supported-transport.md).
      Rclone and pyicloud demonstrate access to existing Drive files through
      Apple's undocumented web transport; a Fedora 44 rclone FUSE mount was
      inspected as a working read/write example. An isolated, read-only probe must
      establish stable item identity, content revisions, bounded complete
      refresh, ranged reads and reauthentication before a Cirrove adapter or
      account picker is added. The probe and provider must be native Cirrove code,
      with no rclone runtime, configuration or credential dependency. Writes
      require separate conflict and recovery evidence.

Modern components do not establish production readiness. Sustained real-provider
use and independently reviewable recovery results remain release requirements.
