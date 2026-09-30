# Retained iCloud replacement audit — 2026-09-30

Registered before execution. Question: what does the current remote state show
for the retained legacy replacement e83673ff-dfdf-452d-8284-90c2cf1c8c7c?

One read-only arm uses `--inspect-replace-phase` against the existing fixture.
It opens the existing journal and sealed checkpoint, lists the owned folder and
queries the original item in Trash. The inspected branch returns before creating
any upload worker. No write, retry, rename or journal acknowledgement is requested.

Prediction: the retained checkpoint remains awaiting verification, with the old
item in Trash and a staged replacement in the owned folder. These observations
cannot prove that the legacy install request was never sent, nor authorize an
automatic replay. Name presence alone is not identity or content verification.

The private manifest records the command, binary hash, PID, disk temporary paths
and terminal result. No concurrent compilation or other live measurement. A
failed authentication or inspection is recorded without restarting a writer.

## Observed result

The arm completed successfully in 68.464 seconds. The checkpoint remains in the
handoff phase. The original exact ID is absent from the owned folder and present
in Trash; the expected staged name is present. The recovery name is absent.
No worker ran and no remote mutation or acknowledgement was requested.

This confirms the retained partial state, not recovery success. A future recovery
operation must validate the exact staged ID and payload digest from the sealed
plan, preserve the recoverable original, and distinguish an explicit user
resolution from replay of the uncertain historical command. This audit supplies
no authority to rename or infer a successful receipt from a matching name.

Public result: [audit result](icloud-legacy-recovery-audit-live-2026-09-30.json).
Private manifest and log:
`.local-state/icloud-legacy-recovery-audit-44a08a66-8f37-4b8d-9f91-bc65c4af0a91/`.
