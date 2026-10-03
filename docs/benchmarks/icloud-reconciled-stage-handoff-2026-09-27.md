# Handoff from a reconciled iCloud staging upload — 2026-09-27

Registered before the live run. The prior experiment left two Cirrove-owned
files in its isolated folder and a private `complete` staged-registration
checkpoint. This follow-up mutates only those exact fixtures; it does not
create another upload or touch the normal read-only service.

Question: Can the remotely discovered ID from a lost-registration response
feed the separately fsynced two-ID handoff plan, without depending on a
filename as the durable item identity or uploading the staged bytes again?

One arm: load the account-bound completed registration checkpoint; re-list
the exact folder, verify both item IDs and full hashes, build a handoff plan
from the bound IDs and fsync `prepared`. Then fsync each pending rename before
sending it, verify both IDs/hashes at recovery/staging, and finally at
recovery/target, and fsync `complete`. No comparative arm. A restart after
plan creation should be possible through the existing pending-state
reconciler, but this run does not inject a crash there.

Prediction: the original ID remains under a unique recovery name, the
registered staged ID reaches the original name and both full hashes remain.
Any changed identity or content must stop before a rename, with no retry of
an uncertain request. This remains non-atomic and non-conditional and does
not validate concurrent remote edits or normal mounted writeback.

The start manifest is written under
`.local-state/icloud-lost-registration-receipt-validation/`
before the process starts, with command, binary SHA-256, PID, expected
duration and disk-backed private temporary directory. No credentials,
signed URLs, receipts, raw responses or file contents are recorded.

## Observed first run

The process completed in 88.2 seconds. It loaded the completed staging
record, verified the exact folder and both full hashes, fsynced a separate
account-bound handoff plan, then moved the old ID to recovery and the
registered staged ID to the original name. Each pending phase was fsynced
before its request, and each resulting phase was checked by exact IDs and
full hashes. Both private `0600` records ended at `complete`; no temporary
checkpoint file remained. The process did not upload another file. This is
one run with no within-arm spread. A crash during this linked sequence was
not injected; the earlier trials separately covered both lost rename
receipts, but not every interleaving with registration recovery.
