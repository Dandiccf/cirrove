# Owned account write context — 2026-09-29

This is a synthetic regression record, not a performance or live-provider claim.

The ordinary manager constructed its write provider before Engine owned the
account or opened its index. Its remount path discarded factory errors and
could create a read-only mount for a write-granted account. A published old
WriteControl also retained the previous journal across the attempted reopen.

The corrected manager provides WriteContext only after acquiring Engine
ownership. It shares the exact journal and checkpoint vault with the filesystem
and workers. iCloud selects its sealed upload checkpoint vault; other providers
retain DesktopVault. Production iCloud writes remain disabled until the router
and acceptance work is complete.

The existing real_manager_mounts_writable_only_for_an_account_with_a_write_grant
synthetic kernel test now checks both grant modes and the following regressions:

- Before the ordering fix, the factory failed its assertion that the account
  index already existed. With the fix it observes the existing index, held
  account owner lock, accessible shared journal, and refusal of a second journal
  owner. The mounted mkdir and account read-only behavior pass.
- After a synthetic ejection, the factory deliberately fails. Reinstating the
  old error-to-None fallback made the test fail because the manager silently
  mounted read-only. The corrected path reports mount unavailability; when the
  factory recovers, the mount returns and a fresh mkdir succeeds.

The test cleans up the manager and kernel session before asserting the remount
result. No real account, credential or provider request is used. The test passed
with the corrected code. Private logs:
`.local-state/icloud-manager-context-negative.log`,
`.local-state/icloud-manager-context-positive.log`,
`.local-state/icloud-manager-remount-negative.log`,
`.local-state/icloud-manager-remount-positive.log`.

Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` finished with
exit 0 and `all checks passed`: formatting, clippy, workspace and iCloud tests,
real synthetic FUSE scenarios, scripts, ledger and documentation. The window
scenarios were not run because this change does not affect their UI. Full log:
`.local-state/icloud-manager-context-full-check.log`.

The running real-account daemon was neither installed nor restarted. This closes
the manager context handoff prerequisite; the production iCloud operation router,
write grant and remaining live acceptance gates are still open.
