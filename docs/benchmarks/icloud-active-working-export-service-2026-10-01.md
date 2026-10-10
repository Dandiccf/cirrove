# Active working-file recovery through the service and CLI

The local `recovery-working` socket request returns bounded metadata pages;
`export-working` selects an exact working UUID/generation and starts an ExportLocal
job. Separate working receipts preserve sealed-save protocol compatibility. The
CLI opts in with `--active`; absent that flag it retains the offline owner-lock
workflow. Ambiguous offline-state/active/socket options are refused.

The engine copies privately, validates through the captured WriteControl and
publishes outside the journal lock. Blocking stages retain the global export
permit, so cancellation of their async waiter cannot admit another copy early.
The source/verified object retains journal ownership. Account/job cancellation
propagates to copying and publication. The receipt must match operation identity,
working UUID, generation, selected size, destination, digest shape and successful
state; no absent/foreign receipt or disappeared job is treated as success.

## Evidence

The existing real FUSE recovery fixture now includes a Python application holding
an unsealed writable descriptor open. It tests metadata listing and recovery
through both socket and CLI, compares exact copied bytes, refuses stale generation
and mount destinations, checks unchanged working metadata/upload queue, observes
no cloud-created file, and reads the original mounted stream afterward.

Independent review found a CLI edge: empty label selects the sole account in the
manager, but exact empty-label polling missed its job. The kernel test using that
selection failed before the correction, exit 101; after matching the unique job
under the resolved single-account semantics it passed, one actual FUSE test,
0.42 seconds. Logs:
`.local-state/icloud-active-working-service-red.log` and
`.local-state/icloud-active-working-service-green.log`.

Sealed/offline/active working contracts also passed; 18 working export tests
include cursor advancement across clean rows, explicit CLI mode, exact receipt
rejection and the core mutation races. Log:
`.local-state/icloud-active-working-service-contracts.log`.
These are synthetic local recovery checks, not live Apple acceptance or a latency
claim. Desktop selection and installed acceptance remain separate work.

## Complete check

Full `scripts/check.sh` passed, exit 0, 2026-10-01 05:26:51–05:34:27 UTC,
including formatting, clippy, workspace/script tests, real FUSE scenarios and
ledger checks. The extended held-working-file socket/CLI fixture ran again as
part of the full suite. Manifest/log:
`.local-state/icloud-active-working-service-check-2026-10-01/`; private TMPDIR and
SQLITE_TMPDIR `/var/tmp/cirrove-active-working-service-check-9q5at0my` on btrfs.
The existing rustdoc warning remains. Desktop changes in this commit only supply
None for the new optional receipt in fixtures; no UI behavior changed and native
window scenarios were not rerun. Installed state and access grants are unchanged.
