# Cirrove delivery preservation and Strata reconciliation audit

Read-only inspection of the installed system and commit diffs on 2026-10-01.
No install, restart, build, test, cloud request, cherry-pick or branch modification
was performed. This document is a plan, not evidence that reconciliation passed.

## Observed installation

- Normal `cirroved.service` is a user unit at
  `/home/dandiccf/.config/systemd/user/cirroved.service`, active PID 3206061,
  executing `/home/dandiccf/.local/bin/cirroved` with the ordinary state/socket.
- No pacman `cirrove`, `cirrove-desktop` or `cirrove-dolphin` package was installed.
- Running executable and installed daemon SHA-256 both:
  `c0ade46af37ba2ef799d6323d0a8141106046f72d40ffd61c75ac53c7e7cf560`.
- Installed CLI SHA-256:
  `88e00b3852b40352aceba98025b51f3b659dbdc246a781baf526548404fb2dc8`.
- Installed desktop SHA-256:
  `a12b96041e9262036e710b9edf386212378845695c0ad0d6a10d35e695bcada2`.
- These match the retained feasibility install/build manifests under
  `.local-state/icloud-service-validation/`, recording source `65760d3` and
  September 25 delivery. The current feasibility release binaries also still
  have these hashes: `--no-build` now would reinstall old code.
- Strata is the default `inode/directory` handler:
  `io.github.lgse.Strata.desktop`, invoking `~/.local/bin/strata %U`.
  Its installed binary SHA-256 is
  `31dd823f880f11f44bb7aecdbf9e3225125f98d5a0d4376b8d77ccf167987a22`.
- No Strata process was running during inspection. With actual
  `XDG_CONFIG_HOME=/home/dandiccf/.config`, no `strata/providers` or `actions`
  directory was present. Thus a globally enabled Cirrove provider is not
  substantiated by this inspection; isolated demonstrations are separate.
- The feasibility installer does not touch the Strata binary, settings, launcher,
  default association or provider directory. It does replace Cirrove binaries,
  unit, artwork, Nautilus extension and optionally Dolphin plugins, then restarts
  the daemon/tray and quits Nautilus.

## Active measurement: hard installation stop

At inspection the combined application arm
`599c285d-68cb-4913-992f-f924fe373dfc` was active, runner PID 960881 and probe PID
960907; its FUSE mount was present. Its local `run.json` reports running and its
preregistration is in `docs/benchmarks/icloud-real-applications-acceptance-2026-10-01.md`.
No restart or installation may overlap it. Recheck its completion and mount
shutdown immediately before eventual delivery; this document does not close it.
The ordinary OneDrive and both Google mounts were also present and must survive.
Public benchmark JSONs inspected had no active state flags or live PID references;
that does not supersede the separately registered live arm.

## Commit and conflict plan

The inspected feasibility HEAD is `3301fb3`; package-validator work is uncommitted.
Strata integration is the single commit `c60127f` on its separate worktree and is
not an ancestor of feasibility. Its required `paths-cached` verb/capability and
helper files are absent from feasibility. A daemon built solely from this branch
would cause an opt-in Strata helper to suppress badges/actions, since it deliberately
refuses ordinary `paths` as a fallback.

After the package checkpoint, a cherry-pick of `c60127f` into an isolated delivery
branch is the appropriate bounded starting point. Do not replace whole files from
Strata's older checkout. Diff inspection suggests these reconciliation points;
no cherry-pick/apply trial was performed, so textual conflicts are predictions.

| File / functions | Reconciliation |
| --- | --- |
| `scripts/install-developer.sh`, immediately after `repo=` | Both branches insert here. Keep the early `--strata-only` delegation AND current `CARGO_TARGET_DIR` normalization. Keep every current release/Dolphin path using `target_dir`; reverting to `repo/target` could install stale artifacts. |
| `docs/desktop.md`, end of file | Both branches append at the former end. Keep the current unconfirmed-cloud-changes documentation and add the Strata preview section; do not overwrite either. |
| `engine.rs`: `path_states_resolved`, `pin_covering`, module declaration | Strata adds cached-mode wrappers and additive `can_pin`. These functions are otherwise unchanged since the Strata base. Preserve current `cached_recovery_name`, RO recovery journal ownership, flush/shutdown and recovery changes elsewhere. Do not replace recovery name lookup with an await-capable status method. |
| `manager.rs`: `path_states` | Add `path_states_mode` and its ordinary wrapper. Current matching function is unchanged; preserve current write context/factory, RO recovery routing, local_recovery status and engine ownership elsewhere. Strata adds no write worker to RO accounts. |
| `filesystem/lifecycle.rs`: `resolve_visible_path` | Add mode wrapper and cached node/children lookup. Existing current changes are in `belongs_to` and recovery/export helpers, outside this method. Preserve those. The cached lookup must remain read-only and must not invoke selected-write admission or provider metadata classification. |
| `lib.rs`: `PathState`, `Capabilities::current`, `serve_managed` paths branch | Add defaulted `can_pin`, `paths-cached:1`, and dispatch to manager mode. Keep all current export/recovery requests and capabilities, including export-save. Capability edits are near current additions and require inspection even if Git merges cleanly. |
| `scripts/check.sh` | Add the Strata Python test beside existing script checks. Retain current feature-gated iCloud clippy/tests and real-mount checks; do not use the older full file. |

New cached-status module/tests, helper/artwork, installer, Strata documentation and
validation scripts can be added intact initially. They do not overlap the owned
PACKAGE modules. The new generic write-admission hook has a default implementation,
so the old NoNetwork fixture should still compile; explicitly make that hook panic
in the cached-status fixture to demonstrate that status does not enter the new
metadata-only write classification path.

## Finite validation and delivery gates

1. Finish the active exclusive arm and checkpoint current package work. Create an
   isolated delivery branch/worktree; cherry-pick `c60127f` and resolve the above
   seams while retaining current admission, recovery and target-dir behavior.
2. Use a unique Cargo target directory. Execute the two cached-status no-network
   tests and helper/install tests. Add one bounded writable-overlay case covering
   a retained natural name and a missing indexed ancestor; assert no read-provider
   or write-admission call. Recheck package `can_pin=false` and inherited/direct pins.
3. Run `scripts/check.sh` fully before committing. Re-run the isolated companion
   Strata list/context UI scenario against the reconciled daemon protocol, retaining
   screenshot artifacts. Do not switch defaults or replace the installed Strata
   executable for this validation.
4. Reconcile documentation that still says the preview was not published against
   the already-authorized published PR evidence; do not guess a current upstream
   release/API status from the old text. No upstream contact is needed to merge
   the local Cirrove capability.
5. After exclusive runs close, build fresh release binaries from the reviewed
   delivery commit. Record source/diff, binary hashes, private disk-backed temp
   directory and install manifest. Verify those hashes differ appropriately from
   the old September 25 artifacts before using `--no-build`.
6. Apply the standard developer installer only once those checks and the active-run
   audit pass. Preserve Strata/default associations. Confirm the running executable
   hash, existing mounts/account identity, new cached capability, ordinary read-only
   account modes, local recovery and explicit iCloud opt-in behavior afterwards.
   Installing an opt-in Strata provider is a separate concrete action; its absence
   now is not grounds to overwrite defaults or globally install fallback actions.

Cherry-picking is sufficient to bring the local Cirrove implementation into the
candidate, not sufficient to claim installed integration or full native iCloud
editing. The separate provider-enabled Strata build/API remains a compatibility
prerequisite, and the ordinary installed workflow still needs its own acceptance.

## Subsequent measurement state

Arm C became terminal with exit0 at 08:43:57 UTC. The subsequent owned-package
mount arm also ended exit0 at 08:49:03 UTC. This supersedes the active-arm
observation above, not the remaining reconciliation/build/install requirements.
The full project check started at 08:49:25 UTC; do not install while it runs.
