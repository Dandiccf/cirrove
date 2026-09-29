# Durable read-only install preflight — 2026-09-29

Registered before live execution. Question: can an interrupted long read-only
preflight resume after restart without being confused with a sent rename?

Scope: one fresh Cirrove-owned fixture, created by the isolated mounted probe.
No ordinary connection, existing user file or retained legacy fixture is changed.
No permanent deletion. The former version stays recoverable in Trash.

Arms, sequentially using one binary and fixture:

1. A: create the owned source through FUSE and verify remotely.
2. B: replace it through FUSE. On `install read-only preflight: start`, send
   SIGINT to this arm's recorded process ID, before an install request is logged.
   Save the interruption log and existing journal/checkpoint. If the boundary
   cannot be caught, record that failure; do not label it a successful fault test.
3. C: new process `--finish-replace RUN_UUID`, without another application save.
   Wait for the existing journal upload, independently check exact IDs and bytes.
4. D: new process `--resume-after-replace RUN_UUID` reads the mounted replacement.

Prediction: B retains InspectInstall, with no install request; C rechecks both
versions and sends the remaining rename once, then publishes both journal IDs;
D reads the new bytes. Old version-1 InstallNew checkpoints remain ambiguous and
are not upgraded from metadata observations. This does not test a request still
executing remotely or make Apple's rename conditional.

Each private manifest records command, binary SHA-256, PID, expected duration,
disk-backed TMPDIR and SQLITE_TMPDIR. No concurrent compilation or measurement.
One run per arm: no spread, latency improvement or general reliability claim.

Synthetic evidence before live work: the restart-state test failed without the
InspectInstall recovery branch and passed with it. The loopback HTTP test dropped
a stalled preflight after observing only retrieveItemDetailsInFolders. Worker
coverage verifies checkpoint persistence for both two and four commit steps.

Private evidence: `.local-state/icloud-install-preflight-restart-d81f6b6f-a5ab-423a-bb14-c3ace9c01a3a/`.

## First live attempt: incomplete

Binary `f1476c63a95712dd0a10d68becb7e0c2e643e50c17c512268ec3bf0716fe14c0`,
fixture `ed0d3518-d84a-45bc-84c4-398f7c874208`. A passed. B reached the new
read-only install phase and was interrupted by recorded PID; its log contains
no install request. MoveOld postflight took 127.7 seconds; total commit 152.8.
C successfully inspected the saved state in 63.1 seconds, then failed before
any install request. The replacement journal row is Failed, retains a checkpoint,
and has no remote success receipt. D was not run. This is not a completed
recovery result.

The adapter factory still rebuilt its source from the active metadata index on
every call. After refresh removed the old ID now in Trash, it could no longer
construct the adapter for the next commit. The next revision stores both original
and parent Node snapshots in outer checkpoint version 2, validates their binding
to account, operation, request and handoff plan, and restores from that sealed
checkpoint. Legacy checkpoints retain conservative behavior. The failed fixture
is retained; it is not manually rewritten or renamed to manufacture a pass.

## Registered follow-up: checkpoint-restored source

Before rerunning, repeat A–D on a fresh owned fixture with outer checkpoint v2
and the same interruption boundary. Prediction: C can construct both the next
commit and its receipt from the checkpoint even after the active index loses the
old ID. No legacy fixture or checkpoint is changed. A successful result must
include C's independent full-byte/two-ID verification and D's fresh mounted read.
Use the new binary consistently for all four follow-up arms; record its hash in
each manifest. No timing comparison is inferred between the failed and corrected
runs. Private follow-up evidence: `.local-state/icloud-install-preflight-restored-source-9fe48188-3ba3-48df-a595-20fe0a45fdfc/`.

## Follow-up observed: passed for this fixture

All four arms used binary SHA-256
`0efc0dd1d27d64e28b5d382b14aad08e7eeb707d3b8652233b4d17ae8636ede7`
and btrfs temporary directories. Recorded PIDs: 1094317, 1096845, 1101319,
1105976. Fresh fixture: `63a341b6-33f7-4c72-b1f4-aed55166d8e1`.
A created and independently verified the source. B stopped at the recorded
read-only preflight boundary, exited 1 after SIGINT, and logged no install request.
C reopened the same upload without another application save. Its full inspection
succeeded in 61.8 seconds; the remaining rename took 2.3 seconds and full receipt
verification 125.1 seconds. The commit completed in 127.5 seconds, uploads were
confirmed at 190.9 seconds, and independent listing, Trash presence and byte
checks completed at 221.4 seconds. C exited successfully. D mounted in another
new process, read the replacement through FUSE and passed independent checks.

The final read-only journal audit found both uploads `uploaded` with receipts.
The replacement retains exactly one failed attempt from the intentional
interruption. The original ID has zero rows in both the nodes and observed
metadata tables; the successful receipt no longer depends on that active entry.
A separate audit of the first failed fixture also found its original absent from
both tables, confirming the factory's missing-index failure mode.

Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed on the
follow-up source before its live build: workspace and iCloud tests, kernel mounts,
scripts and ledger. Display-dependent window scenarios were not run. Private
full-check log: `.local-state/icloud-preflight-full-check.log`.

This proves one controlled interruption and restart with checkpoint v2. It does
not settle ambiguous legacy checkpoints, an in-flight Apple rename, repeated
reliability, broad account routing, zero-byte/large files or concurrent editors.
The two earlier partial fixtures remain retained, not silently repaired. No
ordinary daemon was installed or restarted; normal iCloud stays read-only.
