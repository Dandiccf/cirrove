# Validation record

Date: 2026-09-06. Local environment: Arch Linux, Rust 1.98.1, FUSE 3.18.2.
This is evidence for a **read-only development preview**, not completed real-provider
acceptance for roadmap stages 1–3.

## Local checks

- Formatting and strict Clippy cover all crates and test targets.
- Default workspace suite: **37 tests passed**. Three kernel-FUSE/benchmark tests
  are intentionally skipped in the default suite and run explicitly below.
- Built both binaries and generated workspace Rustdoc.
- Executable smoke test passed: synthetic staging, status, SIGTERM and socket cleanup.
- Updated systemd user-unit template verified with the locally built daemon path;
  the unit was not installed or enabled on the user's desktop.
- Real desktop Secret Service test passed: write, read and remove one uniquely
  named synthetic Cirrove credential. No existing credentials were read or changed.

## Authentication and Graph transport

Synthetic OIDC tests require a valid RSA signature, issuer, audience, nonce and
expiry. Callback tests reject wrong state, host, method and duplicate parameters.
A 32-caller expiry test causes exactly one refresh, persists the rotated grant and
keeps a delayed old-token 401 from expiring the newer token. A keyring-save failure
prevents publishing the replacement grant. The RSA fixture key is intentionally
public and used only for offline tests.

Local HTTP fixtures test delta translation, tombstones and linked-drive identities;
410 recovery signalling; 429 cooldown; stalled-request cancellation; rejected
foreign continuations and authenticated redirects. Content checks verify exact
ranges, reject version changes and oversized/incorrect bodies, and assert that
Graph bearer headers never reach signed-content requests. Download throttling is
shared with metadata calls.

These fixtures do not authenticate to Microsoft or establish real token-refresh,
consent-policy, CDN or SharePoint behavior.

## Storage and actual filesystem tests

Storage checks preserve old indexes until a staged refresh finishes, reopen
interrupted pagination, isolate accounts, apply final occurrences/tombstones and
reject out-of-order pages. Foreground observations newer than a feed round survive
that round and yield to the next one. Stable inode batches survive database reopen.
Shortcut ancestry queries exclude unrelated subtrees and terminate parent cycles.

The content tests check 32-reader coalescing, offsets beyond 2 GiB in a synthetic
3 GiB file, offline cache reuse after restart, corruption recovery, interrupted
publication cleanup, quota enforcement and shared-failure retry suppression.

Two tests ran against **real kernel FUSE mounts** in temporary directories:

- Normal file reads, linked-library projection, duplicate-alias inodes, deep
  traversal, large seeks, EROFS on writes, stable inodes/cache after restart,
  ejection/remount and folder navigation during a stalled application read.
- The actual account manager's automatic remount, persisted disable/enable,
  refusal to obscure a new local file, shutdown cleanup and restart.

An early stalled-read run exposed an EINTR retry loop during shutdown. The stopped
mount now returns ENODEV; the corrected test passed. Its old fixture process and
mount were cleaned up. No existing cloud mounts or data were changed.

## Local performance fixture

The isolated debug-build benchmark performs 20 cold/warm reads of 3,100,000-byte
files and 20 listings of a 525-entry cached directory. Its synthetic provider adds
50 ms latency; it performs no Graph, OAuth or internet traffic.

| Operation | p50 | p95 |
| --- | ---: | ---: |
| Cold content read, metadata already indexed | 123.69 ms | 142.93 ms |
| Warm content read | 2.22 ms | 2.69 ms |
| Cached directory listing | 9.02 ms | 10.71 ms |

There were 20 provider range calls for the cold reads and zero for warm reads.
Process peak RSS was 79,272 KiB, including fixture generation and the FUSE service
in one process. This is not an isolated daemon-memory measurement. The native Graph
adapter additionally checks metadata before and after uncached blocks, so these
figures must not be presented as real OneDrive latency.

Machine-readable evidence and the repeat command are in
[synthetic-performance.json](synthetic-performance.json).

## GitHub CI

The [foundation CI run](https://github.com/Dandiccf/cirrove/actions/runs/34014890227)
passed at `fa22f21`. The [read-only preview CI run](https://github.com/Dandiccf/cirrove/actions/runs/34018513735)
passed on Ubuntu 24.04 at `c4a877eb17e1539c42d4199ac78cebcf035b8e3b`.
Formatting, strict Clippy, all 37 default tests, both explicit kernel-FUSE lifecycle
tests, binary builds, executable smoke and Rustdoc completed successfully. The
keyring check and benchmark remain separate local checks described above.

## Required before calling stages 1–3 complete

- Cirrove's own Microsoft app registration, real consent and verified work-account,
  tenant and drive selection.
- Real access-token expiry/refresh, revoked consent, keyring locking/unlocking and
  recovery across a desktop/service restart.
- Linked SharePoint folders, folder-only permissions, duplicate shortcuts, removed
  links and revoked targets on real accounts.
- At least 24 hours of read-only operation, including a controlled network outage
  and recovery. No completed soak test is claimed.
- Real-provider cold/warm p50/p95, request counts and ordinary application previews;
  synthetic performance does not satisfy that gate.
- Repeat the recovery checks on another supported system; CI covers the synthetic
  Linux path, while real-account behavior still needs desktop validation.

Writes, pinning, conflict recovery, desktop badges/settings, Google Drive and iCloud
remain later milestones. Unit tests and orderly reopen tests do not simulate actual
power loss, hardware failure or every application behavior.
