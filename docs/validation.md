# Validation record

Date: 2026-09-06. Local environment: Arch Linux, kernel 7.1.9-arch1-2,
Rust 1.98.1, FUSE 3.18.2.
This is evidence for a **read-only development preview**, not completed real-provider
acceptance for roadmap stages 1–3.

## Local checks

- Formatting and strict Clippy cover all crates and test targets.
- Default workspace suite: **39 tests passed**. Six kernel-FUSE/benchmark tests
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
shared with metadata calls. Package fixtures verify that OneNote-style and future
package types do not abort a delta or children listing. cTag tests allow metadata-only
changes during a download and reject changed content, retaining eTag fallback coverage.

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

Five tests ran against **real kernel FUSE mounts** in temporary directories:

- Normal file reads, linked-library projection, duplicate-alias inodes, deep
  traversal, large seeks, EROFS on writes, stable inodes/cache after restart,
  ejection/remount and folder navigation during 96 stalled application reads, all
  released by shutdown with ENODEV.
- The actual account manager's automatic remount, persisted disable/enable,
  refusal to obscure a new local file, shutdown cleanup and restart.
- Shared read-only and private copy-on-write Python mappings; a private modification
  does not change shared bytes. A synthetic 3 GiB mapping can read beyond 2 GiB while
  the application process's peak RSS stays below 128 MiB.
- A content change through the provider exposes a new regular-file inode, including
  file shortcuts, while open old mappings retain their bytes. A following rename
  with the same cTag reuses both inode and cached content with no new range request.
- A burst of 96 simultaneous 64 KiB reads from distinct 3,100,000-byte files completes
  while cached folder listings retain independent capacity. The fixture adds 250 ms
  per content request and the cache makes 96 range calls, without retries.

An early stalled-read run exposed an EINTR retry loop during shutdown. The stopped
mount now returns ENODEV; the corrected test passed. Its old fixture process and
mount were cleaned up. No existing cloud mounts or data were changed.

Regression evidence before these fixes: shared mmap returned ENODEV; a package
aborted its containing listing; 64 of 96 burst reads returned EAGAIN. All three
corrected cases passed. The burst's 20 cached directory listings measured p50
1.10 ms and p95 3.00 ms locally. This is simulated thumbnail-style I/O, not a run of
Nautilus's actual thumbnailers or a real-provider performance claim. The sample
directory contains one entry; the separate benchmark below covers 525 entries.
The repeat command and measurements are in
[synthetic-thumbnail-burst.json](synthetic-thumbnail-burst.json).

The optional metadata content-tag field is backward-readable from prior preview
databases. The revised cache/inode keys can cause a one-time cold cache and changed
regular-file inodes on upgrade. Old cache blocks remain quota-accounted and evictable;
no cloud data is written. Mappings still cannot demand an uncached historical version
after it changes remotely; see [FUSE semantics](architecture.md#fuse-and-lifecycle).

## Local performance fixture

The isolated debug-build benchmark performs 20 cold/warm reads of 3,100,000-byte
files and 20 listings of a 525-entry cached directory. Its synthetic provider adds
50 ms latency; it performs no Graph, OAuth or internet traffic.

| Operation | p50 | p95 |
| --- | ---: | ---: |
| Cold content read, metadata already indexed | 123.07 ms | 142.97 ms |
| Warm content read | 1.45 ms | 1.96 ms |
| Cached directory listing | 10.16 ms | 11.12 ms |

There were 20 provider range calls for the cold reads and zero for warm reads.
Process peak RSS was 76,616 KiB, including fixture generation and the FUSE service
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

The [mapping and concurrent-read CI run](https://github.com/Dandiccf/cirrove/actions/runs/34020162248)
passed on Ubuntu 24.04 at `1e301dc541ecad23622409ab4d9aed524628a7d1`.
All 39 default tests and five explicit kernel-FUSE tests passed, including the
memory-mapping and 96-reader cases. Formatting, strict Clippy, builds, executable
smoke and Rustdoc also passed. The kernel advertised direct-I/O mmap support on
both the local Arch session and this Ubuntu runner.

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
