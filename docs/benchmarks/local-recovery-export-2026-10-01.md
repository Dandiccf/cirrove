# Local recovery export validation — 2026-10-01

This implements a bounded-memory, cancellable copy of one retained immutable save
through the service and CLI. It does not call a cloud provider or mutate its
journal record. A short control request starts a tracked background job; explicit
completion carries a verified receipt. The runtime permits one export at a time.
The file-descriptor operations use the safe `rustix` fs API; 1.1.4 was already in
the lockfile transitively and is now a direct service dependency. No unsafe code
or new package version was introduced.

## Evidence

Five local tests cover exact/empty bytes, private permissions, unchanged journal,
existing destinations, dangling/ordinary symlinks, symlinked ancestors, traversal,
source-journal targets, cancellation, corruption, a racing destination creator,
source-path replacement and destination-parent rename. The renamed-parent test
first failed against the initial implementation, which preserved the correct
opened directory but could report the now-stale requested path. After adding the
prepublication directory identity/path check, it passed with no published copy.

`real_recovery_export_uses_the_service_without_replaying_a_failed_save` passed
through an actual synthetic FUSE mount, private socket and the real `cirrove`
CLI. It exported a conflicted save, checked exact bytes and explicit completion,
verified the conflict was unchanged, refused a known mount destination through
the socket and independently refused the FUSE destination in the core copier.
The synthetic cloud contained no published content afterward. No real account
credentials or personal files were used.

Job tests cover retained completion receipts and dismissal; desktop model tests
prevent export work from appearing as offline pinning. CLI/UI integration limits
are documented in [the user-facing contract](../local-recovery-export.md).
No throughput, large-file reliability or real-account export claim is made here.

Full `scripts/check.sh` passed (2026-09-30 23:23:17–23:31:00 UTC),
including formatting, clippy, workspace, feature/kernel, script, translation and
ledger checks. Its private runner manifest/log are retained under
`.local-state/local-recovery-export-check-2026-10-01/`; temporary files used
private disk-backed `/var/tmp` on btrfs, and the worktree's separate Cargo target.

All 11 native window scenarios then passed with
`CARGO_TARGET_DIR=.target-icloud-feasibility cargo test -p cirrove-desktop --test window --locked -- --ignored --test-threads=1`.
GTK reported the existing unavailable accessibility-bus warning; no test failed.
This validates the development checkout, not an installed daemon. The regular
installation was not replaced or restarted; installed acceptance remains open.
