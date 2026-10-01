# One-shot allocation in the durable upload worker

Question registered before integration tests: can a provider allocate a native
package identity once, after durable checkpoint publication, without repeating
allocation when the response or process is lost?

The new Allocate step may originate only in a fresh begin call. Its sealed
checkpoint and journal binding must precede the allocation callback. Cancellation
is checked after persistence and by the remote-call wrapper. Recovery inspects the
saved state; it cannot authorize Allocate, nor can a Prepared inspection or a
second Allocate response. An adapter must leave an armed checkpoint without a
known allocated identity uncertain unless independent evidence proves absence.

Arms: synthetic successful allocation; vault refusal before and after persistence;
cancellation during persistence; lost allocation response; recovered Allocate;
Prepared-to-Allocate escalation; repeated Allocate; and a killed child followed by
a fresh worker. No cloud calls. Run one test process at a time, with disk-backed
TMPDIR and SQLITE_TMPDIR and the dedicated worktree Cargo target.

Prediction: normal completion calls allocation once after durable publication;
all ambiguous recovery paths call it zero additional times. A proven failure
before persistence may safely begin again. Endpoints are callback counters,
checkpoint/journal evidence and worker state, not elapsed-time performance.
Negative controls must demonstrate that allowing recovered allocation or moving
the callback before checkpoint publication makes the relevant regression fail.

This worker contract alone does not implement native editing or enable mounted
package writes. Archive semantic validation and exact remote-ID readback belong
to the package adapter/admission path. Installed acceptance remains separate.

Integration evidence: the initial complete allocation test target passed at
09:27:36 UTC (six tests plus its ignored crash-child helper). In the separate
negative control, allowing recovery to authorize allocation failed at
09:28:02 UTC with `allocation replayed` from the exclusive synthetic allocation
marker. This establishes that the provenance gate prevents a second allocation,
rather than merely checking a harmless enum value. The production guard was
restored immediately before the next test run.
Artifacts: `.local-state/icloud-access-package-allocation-integration-2026-10-01/`
and `.local-state/icloud-access-allocation-replay-negative-2026-10-01/`.

The restored provenance guard passed the full `scripts/check.sh` at09:43:26 UTC,
including allocation tests and their actual child-process interruption arm.
Combined run: `.local-state/icloud-access-package-journal-allocation-fullcheck2-2026-10-01/`.
