# Preserve Strata while integrating iCloud

This reconciliation brings the single existing Cirrove integration commit
`c60127fb0b1026c6e09862cad632d60cad91de5d` onto the iCloud feasibility source.
Its common ancestor is `d345767f10842ede332ff3bae63e3dedc258e117`; no dependency
updates or unrelated Strata-repository changes are included. This is prepared
source, not a new installed or measured result.

The refreshed patch was prepared against the current dirty feasibility source,
including native-import watch/remount checks and the owned package Trash probe.
Those changes are part of the scratch baseline and are not repeated or reverted
by this additive patch. No builds, tests or live actions ran during preparation.

## Conflict resolution

- Keep both `engine::cached_status` and `engine::native_import` modules.
- Preserve recovery, native-import and current capability changes while adding
  `paths-cached: 1`, `PathState.can_pin`, metadata-only status resolution and the
  existing writable namespace-overlay path.
- Preserve the full existing desktop recovery/import documentation; append the
  Strata section rather than choosing one side of the append conflict.
- Place `--strata-only` dispatch before normal developer-install handling, while
  retaining current absolute/relative `CARGO_TARGET_DIR` normalization and all
  package/developer exclusivity checks for daemon installation.
- Import the original helper, icons, scoped installer/uninstaller, tests and
  isolated screenshot harness. No companion Strata binary or default association
  is changed by this source reconciliation.

The helper deliberately requires `paths-cached: 1`; advertising ordinary `paths`
is insufficient and must not trigger a fallback that hydrates native exports.
Native package containers remain ineligible for pin actions. Unknown cached
metadata is refused, not fetched to decorate a menu or badge.

## Validation to run after the current exclusive check

Run the existing `cached_status` Rust tests (provider methods panic if called),
`scripts/test-strata-provider.py`, and the whole `scripts/check.sh`. Then use
`scripts/validate-strata.py` with the companion Strata checkout and a fresh private
synthetic output directory. This preparation did not run builds, tests, the UI
harness or installers. Retain no-provider-call, direct/inherited pin, stale reply,
mount disappearance and unrelated/mixed-selection boundaries.

## Installed-artifact preservation checklist

Before any later deployment:

1. Check all recorded measurement windows and package/developer exclusivity.
   Inventory/hash the actual installed Cirrove and provider-enabled Strata
   binaries and the existing managed provider directory; retain the installation
   marker. Do not assume the checkout describes the installed process.
2. Preserve `~/.config/strata/providers/cirrove/` (or its XDG location), companion
   Strata binary, Strata configuration and current default file-manager
   association. The normal Cirrove installer does not need to replace them.
3. If an explicit provider refresh becomes necessary, use the managed installer:
   it verifies owned-file hashes and refuses user edits/unknown files. Do not
   force-overwrite or recursively remove the directory. Confirm any existing
   provider socket selection before changing it.
4. Deploy only at the agreed installed-acceptance point. Verify that the running
   daemon advertises `paths-cached: 1`, conditional menus appear only for eligible
   mounts, inherited pins remain distinct, and kept/fetching badges refresh
   without clearing/flicker. Ordinary on-demand files must stay unbadged.
5. Recheck that iCloud access mode, pending edits and existing other-provider
   connections survived, and that Strata/default associations did not change.
   Preserve rollback evidence; journal schema compatibility is a separate gate.

## Source integration checks

The refreshed patch is now applied to the authoritative iCloud worktree. All
11 `scripts/test-strata-provider.py` tests passed. The dedicated-target
`cargo test -p cirrove-service --lib cached_status --locked` ran both expected
tests and passed (324 other tests filtered out). The fixtures panic if provider
methods are called and cover inherited pin ancestry and cold generated exports.
Artifact: `.local-state/icloud-access-strata-reconcile-cached-2026-10-01/`.
These checks establish source compatibility only; full checks, isolated UI
acceptance and installed verification remain pending. No installation changed.
