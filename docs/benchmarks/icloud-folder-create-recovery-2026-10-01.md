# Folder creation: prove unsent retries without replaying uncertain writes

Registered before execution on 2026-10-01.

Question: can an explicit retry recover a folder-create preflight refusal while
never sending a second POST for an uncertain dispatched operation? Prediction:
a bound durable NotSent checkpoint permits router Sent-to-Prepared recovery;
MayHaveSent, missing evidence and incompatible legacy evidence cannot authorize
replay. A successful create retains its verified identity.

## Arms and endpoint

Synthetic localhost adapter fixtures, isolated journals and vaults only; no saved
Apple session, real provider calls or personal data. Dedicated worktree target.
Private btrfs TMPDIR and SQLITE_TMPDIR. Run all arms sequentially.

Negative controls (one source reversal at a time, restored immediately afterward):

1. Treat NotSent reconciliation as Indeterminate: the preflight-storage-refusal
   retry test must fail at its Uncommitted assertion.
2. Omit MayHaveSent persistence: the lost-response test must fail at its
   Indeterminate assertion.
3. Omit durable router Sent-to-Prepared reset: explicit retry must fail rather
   than satisfy the Prepared/post-once assertions.
4. Omit probe operation-aware preparation: fixture checkpoint assertion must fail.

Restored arms: six adapter recovery tests, three router/worker tests, and the
probe preparation test. Require executed test counts, zero POST before explicit
retry, exactly one POST after recovery, binding rejection and no replay on lost
response. Finish with scripts/check.sh, including feature and kernel tests.

This is deterministic recovery evidence, not real Apple quota, arbitrary network
failure or performance acceptance. Cancellation at each persistence boundary,
POST-507 and an error after Created persistence remain separately unproven.

Status: focused arms passed; full repository check passed.

## Focused results

All four negative controls failed at the expected behavioral assertion:
NotSent/Uncommitted classification; MayHaveSent/Indeterminate classification;
router retry remained VerifyRequired rather than Pending without durable reset;
probe preparation omitted its checkpoint. Restored code passed all six adapter,
three worker/router, and one feature-probe preparation tests. Executed counts
were checked; no compile failure was counted as a behavioral negative.

Manifests and logs are retained under `.local-state/folder-create-<arm>-2026-10-01/`,
with arms `adapter-unsent-red`, `adapter-dispatch-red`, `adapter-green`,
`router-reset-red`, `router-green`, `probe-red`, `probe-green`. Each manifest
records exact command, start/end, process, source SHA-256 and private btrfs
TMPDIR/SQLITE_TMPDIR. Runs were sequential, 06:45:52–06:48:54 UTC.

## Complete repository check

`scripts/check.sh` passed on 2026-10-01, 06:52:26–07:00:24 UTC (exit 0),
including workspace/feature tests, real kernel mounts, script checks and docs.
The native window suite passed separately as recorded above. Private manifest
and log: `.local-state/icloud-folder-delete-check-2026-10-01/`; temporary storage
was private btrfs. Ordinary installed services and cloud data were untouched.
