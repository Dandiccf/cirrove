# Retained iCloud replacement: read-only audit — 2026-09-29

Registered before running. Question: what state is now visible for the retained
phase-split fixture e83673ff-dfdf-452d-8284-90c2cf1c8c7c?

One read-only arm runs `--inspect-replace-phase` with the current probe binary.
It opens the existing journal and sealed checkpoint and lists the owned folder
and Trash. No worker or mount starts and no cloud mutation is requested.
The manifest records binary hash, PID, command and disk-backed temporary paths.

Prediction: the checkpoint remains in handoff, the original is absent from the
owned folder and present in Trash, and the staging name remains present. This
metadata-only observation does not verify bytes, authorize rename replay or
prove that no previously sent request remains in flight. There is one arm and
no timing comparison or reliability claim.

Endpoint: successful diagnostic output or an explicit failure. The saved upload
and local payload must remain available for subsequent recovery work.

Private evidence: `.local-state/icloud-retained-replacement-audit-b78235a6-9b5a-4760-9519-ab37fb9301fd/`.

## Observed

The diagnostic exited successfully. The saved outer checkpoint was 2621 bytes
and its inner checkpoint 1137 bytes. It reported handoff, original absent from
the owned folder, staging name present, recovery name absent, original exact ID
present in Trash. No upload worker started. This confirms the metadata prediction;
it does not verify the staged bytes or make an uncertain rename safe to replay.

The current worker still uses 125 seconds for checkpoint inspection, while the
registered commit-deadline run observed a complete old-backup verification taking
128.1 seconds. Provider-specific inspection deadlines are therefore needed as well
as commit deadlines; they do not resolve the ambiguous InstallNew checkpoint.

## Inspection deadline regression

`provider_inspection_deadline_preserves_uncertain_upload_without_replay` first
retains a checkpoint after a commit timeout, then stalls inspection with a 10 ms
provider limit. Reconciliation explicitly remains uncertain. With the old fixed
125-second inspection limit the test failed at its 2-second outer deadline;
with provider deadline dispatch it passed. The complete transfer test binary
executed all 22 tests successfully. The test checks retained checkpoint identity,
local bytes, no fabricated receipt, one begin call, and later successful resume.

The isolated mounted iCloud fixture now grants replacement inspection the same
300-second bound as replacement commit. Other providers retain 125 seconds by
default. Reconciliation's existing 15-minute bound is unchanged. These are
synthetic worker results; the retained live fixture was only inspected, not
repaired, and its ambiguous InstallNew phase remains unresolved.

Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` completed
successfully, including workspace, isolated iCloud, kernel mount, script and
ledger checks. The display-dependent window scenarios were not run. Rustdoc
reported one broken intra-doc link in the service documentation; the command
finished with exit 0 and `all checks passed`. No ordinary daemon was installed
or restarted for this isolated provider-deadline change.
