# Observed generated-package source changes

Question: after the engine observes an iCloud package revision or identity change,
do warm cached child lookup and listing refresh or refuse appropriately while
preserving a complete offline snapshot on availability failures? No claim of
remote freshness without an observed change.

Registered arms: first apply store schema8 and engine regressions without the
engine fix; `observed_package_source_refreshes_warm_children_for_all_publication_routes`
should fail returning archive-v1 instead of archive-v2. Then apply the engine
fix and run all observed-package-source and existing stale-package tests. Run
store source-binding tests, then remove source-CAS and retained trusted-directory
predicates independently to demonstrate their tests fail. Restore each guard.

A separate migration review found legacy classification alone was granting
retention to providers that did not opt in. Apply the opt-out regression before
its fix; migrated delta/reset positive child lookup must expose stale archive-v1
without the fix, and archive-v2 afterward. Untrusted classification must not grant
retention. Preserve legacy iCloud missing/reclassified-source refusal.

Use one run at a time, disk-backed private TMPDIR/SQLITE_TMPDIR and the worktree's
dedicated Storage target. These are synthetic correctness regressions, not live
latency measurements. No installed metadata or daemon is changed.

The first engine control failed as predicted: positive cached lookup returned
archive-v1 rather than archive-v2 after direct source publication (route0).
Applied the engine source-binding fix, then added the opt-out migration
regression without its retention fix for the next registered negative.

The opt-out control also failed as predicted: after migrated delta publication
(reset=false), cached positive lookup returned archive-v1 instead of archive-v2.
Applied the trusted-source-only retention predicate and now run the complete
observed-source engine group, including migration and offline fallback.

All six corrected observed-source engine tests passed, including direct, parent
and delta publication routes, migrated opt-out delta/reset, legacy absence and
reclassification, unchanged content-tag reuse and availability fallback. Future
run manifests additionally hash all store/core sources; the initial three engine
arms used the previous manifest source list and did not include store hashes.
Their tested source order is documented above; this is a provenance limitation,
not a missing test execution.

The initial store test build found one old direct publish() test call missing the
new optional source argument; supplied None for that ordinary-directory fixture.
All six store source tests then passed. Removing the source-CAS made its regression
publish the stale snapshot instead of reporting SourceChanged. Removing trusted
snapshot retention made its delta/reset regression fail. Restored both sources.

Additional registered legacy control: remove only historical classification from
the engine opt-in decision (keep identity checks). The migrated missing-source
regression should then return cached children instead of refusing. Restore and
rerun all six observed-source tests and existing stale-package tests afterward.

The legacy control failed at the intended assertion: "legacy disappearance
returned old archive". Restored the exact opt-in classification check.

Restored-source service retry group passed all11 tests. The full store suite
found a stale future-version refusal fixture: version8 is now the implemented
metadata schema, so expecting its rejection is wrong. Advanced only that
future-version fixture to9 and rerun the full store suite. Historical migration
inputs and ordering/preserved-data assertions remain unchanged.

The corrected full store suite passed, including integration tests and lookup
syscall tests. All11 service package-retry tests passed on restored guards.
Starting full scripts/check.sh before committing this metadata schema8 change.

Full `scripts/check.sh` passed from 2026-10-01T19:15:30.759344+00:00 through
2026-10-01T19:25:10.355719+00:00: format/clippy, workspace/features, kernel mounts, scripts,
Strata/Dolphin, ledger and docs. Existing retry_stuck rustdoc link warning remains.
Display-dependent window scenarios were not run. No live/installed metadata
migration was performed; all schema8 evidence here is synthetic.
