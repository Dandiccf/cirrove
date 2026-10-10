# Virtual allocated-space correction — 2026-10-02

## Contract and scope

A mounted inode owns no backing allocation. Report zero `st_blocks` while
preserving `st_size`. Actual cache blocks, metadata and edit spools are counted
at their private backing paths. Pinned/cached state is not inferred from zero
blocks. Apparent-size tools intentionally continue to sum remote sizes.

The hotfix is based on installed source `65760d3`, not the unfinished iCloud
write branch. The runtime change is confined to FUSE attribute construction.
It adds no cache lookup, provider operation or migration.

## Controlled regression

The synthetic kernel test
`real_virtual_allocations_do_not_duplicate_cloud_or_cached_bytes` was first run
against the original size-derived block calculation in the iCloud development
tree. It failed: a 3 GiB online-only file reported 6,291,456 allocated 512-byte
blocks instead of zero. After the correction the same test passed.

The installed-version hotfix then passed the full `scripts/check.sh` command
(exit 0, 05:17:51–05:25:09 UTC). This includes actual FUSE kernel tests.
The new fixture checks logical lengths, online-only content, downloaded content,
a materialised pin, shared aliases and GNU `du` output. It asserts no content
reads from metadata/usage scans and nonzero backing-cache allocation. The
existing offline writable-pin fixture also checks that a pending local edit
preserves length without a duplicate mounted allocation.

Checks used a dedicated Cargo target on Storage, with incremental compilation
and debug symbols disabled. TMPDIR and SQLITE_TMPDIR were on Storage ext4.
The only documentation warning was the existing unresolved intra-doc link.

Before installation, one metadata-only sample from each of three live mounts
reported nonzero allocated blocks. One other pre-existing Google entry refused
stat; no contents were read and no cloud mutations were performed.
## Installed verification

The release daemon built from runtime commit `fafa909` was installed through
`scripts/install-developer.sh --no-build` at 05:27:39 UTC. CLI, desktop and tray
binaries were reused byte-identically; Strata and the default file-manager
association were also verified unchanged. The running daemon SHA-256 is
`cae39ed7013ea2f001afebb4ec819b72fe3aaa6092512873527996d2d4257321`.

All three original account IDs and mounts were retained. The same three live
metadata samples changed from 1, 1 and 5,740 allocated blocks respectively to
zero, with unchanged logical lengths. No file content was read for this check.
OneDrive remained ready. Both Google accounts required sign-in before and after
the update; this change does not resolve their existing authentication state.
All three retained zero failed uploads and zero stuck changes.

No claim is made that every file manager totals allocated rather than logical
sizes. Apparent-size views intentionally remain unchanged. Existing scan results
need a refresh. This verifies the common filesystem attributes, not screenshots
of every application.
