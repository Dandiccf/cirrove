# Native local temporary lifecycle validation

Registered before execution after commit 7ea7bd7 and fullcheck native-v2-readback-fullcheck passed.

Question: can native archive editors rename and abandon local temporary streams while open descriptors and recoverable bytes remain intact, without creating cloud operations?

Prediction: removing each admission/publication/retirement guard fails its corresponding exact synthetic regression. Restored code passes the actual FUSE pending-chain test, retirement with retained unlinked bytes, and read-only recovery export.

Serial arms use /tmp/cirrove-temp-lifecycle-redgreen-20261001.py, the existing run-icloud-access-arm.py, disk-backed private temporary storage and the isolated worktree target. Each arm records source hashes and process IDs before execution. No daemon restart, cloud mutation, installation or cleanup is involved.

Negative controls: absent-victim routing; linked-to-unlinked projection transition; retirement exclusion of unlinked temporaries; exact selected-name comparison. The kernel test uses its qualified module name, --exact and --ignored, and the harness requires exactly one executed test per arm. Each deliberate source fault is restored before the next arm, including on unexpected failure. Three final restored tests cover kernel, retirement and temporary-stream recovery.

These tests establish local synthetic behavior, not live Apple application acceptance. Results pending.

## Observed result

Run `.local-state/temp-lifecycle-rg-2977b9a300bd/run.json` completed all four negative controls at the expected assertion locations. Restored kernel, retirement and temporary-stream recovery tests each executed one test with zero ignored and passed. Source hashes match the pre-control backups. No provider calls or installed daemon changes. The separate temporary authority/collision/SQL rollback regression is checked in the follow-up journal test group.

`native-temp-lifecycle-journal` executed both temporary lifecycle tests and passed, including stale selected names, foreign-account/canonical refusal, destination collision and injected SQL rollback.
