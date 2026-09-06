# Recovery and large-library validation

This record covers deterministic checks for the first OneDrive 1.0 milestone.
It contains no private-account measurements. The product milestone remains open
pending sustained real-provider recovery and lifecycle evidence.

## Changes

- A partial SQLite shortcut index and upward ancestry traversal replace walking
  the entire subtree on every linked-library discovery pass. Schema version 3
  adds only this index; existing metadata and delta cursors are preserved.
- Daemon startup recovers a disconnected control socket under its ownership lock.
  Live sockets, ordinary files and symlinks are preserved.
- Mounts carry the account UUID as their filesystem source. A matching mount can
  be detached after the kernel reports ENOTCONN, then mounted by the new manager.
  A live or foreign mount is never intentionally displaced.
  Status requires an owned session and the matching account source, preventing
  a disconnected or foreign same-type mount from appearing connected.
- Read-only flush/fsync calls succeed for valid file handles. Missing extended
  attributes return ENODATA and their list is empty, avoiding misleading warnings
  for routine desktop probes. These are not implementations of writable fsync or
  per-file cloud status badges.
- Successful Microsoft token refresh emits a generic diagnostic after the rotated
  credentials have been persisted. Tokens and account identifiers are not logged.

## Evidence

- 42 default workspace tests passed.
- Seven explicit kernel-FUSE tests passed, including actual termination of a separate
  synthetic mount process followed by recovery and verified file bytes.
  Another fixture verifies that a second account cannot claim the first account's
  mount, then successfully retries after the original mount is removed.
- The executable smoke test passed exclusive ownership, SIGKILL/socket recovery,
  status and graceful SIGTERM cleanup.
- The database migration test retained the original node and completed cursor.
- A synthetic 20,000-file library with one shortcut completed discovery with fewer
  than 2,000 SQLite VM instructions and zero full-scan steps. The bound measures
  database work, not machine-dependent wall-clock speed.
- Strict Clippy and formatting checks passed for the Rust workspace.
- The local observation script's synthetic checks exclude account identities and
  provider details and do not report an absent service as ready.

These results do not establish a finished OneDrive release, successful overnight
operation, a full-machine power-loss test, or working write support. Follow the
complete [product acceptance plan](product-milestones.md).
