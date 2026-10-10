# iCloud replacement body interruption: registered recovery check

Registered before live execution, 2026-10-01. Extend the completed create-body
experiment to replacement. One fresh Cirrove-owned folder and small original are
created and independently hash-verified first. Queue a 64 MiB + 17-byte
position-dependent replacement using the regular working-copy/sealed-journal
path. Bind the developer-only stream boundary to this operation's exact staging
name and size. Stall after 8 MiB yielded to the HTTP body, then exit 86 without
returning from the worker. Yielded bytes do not prove server acceptance.

A fresh process must verify the original completed receipt, pending operation
identity, full local new-generation digest, and allocated/no-receipt Stage
checkpoint. The inspection guard accepts the Stage envelope only, refusing
Handoff/ambiguous envelopes and all mutating worker calls. Before and after
read-only inspection/reconciliation the owned folder must contain only the exact
original, independently verified against its old revision and bytes. Expect
`verify_required` on reopening, then `pending` after proving the unfinished stage
uncommitted, with old transport state cleared and no replay during inspection.

Export and verify the preserved new generation before permitting a fresh upload
attempt. Then expect `uploaded`, a new staging/document identity, the full remote
new-generation hash, exactly one visible final item, and the original exact ID in
recoverable Trash. The ordinary handoff provider also verifies the old Trash
content before returning completion; this arm independently checks old content
before retry and exact Trash identity afterward, not a separate final Trash-body
hash oracle. No permanent deletion, other account or personal file mutation.

One pair, 1200 s per process. Register binary hash, command, PID, deadline, private
disk-backed TMPDIR/SQLITE_TMPDIR and filesystem before start. No concurrent build
or measurement. Start recovery only after exit 86 plus a valid incomplete-body
marker. Retain all failed fixtures and stop rather than silently replaying them.
No mount, installation or regular daemon change. No speed/RSS claim.

Prediction: the incomplete Stage has not moved the original. Reconciliation can
safely discard the allocated transport and retry the same local operation with a
fresh document ID. Only the later completed handoff moves the original to Trash.
This is one controlled replacement boundary; concurrent edits during retry,
registration-acknowledgement loss, quota/session failures and installed acceptance
remain separate gates.

## First attempt: local fixture setup failure

Run `6ef6b96c-b28d-46cc-bb39-a8f64fb0a453` exited 1 before reaching the body
boundary (76.018 s; no recovery process started). The small original was uploaded
and independently verified. The helper then passed all 64 MiB + 17 bytes to one
`write_working` call, which correctly refuses calls over 8 MiB. Read-only journal
inspection found one completed 32-byte original and an empty dirty working copy;
there was no sealed replacement row or prepared/interrupted marker. No replacement
upload or Trash step had begun. This is not evidence about stream recovery.

A regression test with 8 MiB + 17 bytes reproduced the same `invalid upload
intent` failure. The helper now writes 1 MiB chunks with exact offset/short-write
checks before sealing. The regression verifies the complete sealed bytes and
SHA-256; the existing ownership/refusal test remains in place. Ordinary FUSE
writes already use bounded calls; production journal limits were not changed.
The failed fixture and manifest remain retained.

A new attempt with a fresh original/folder is registered with unchanged boundary,
size, endpoints and deadlines. It will not reuse or resume the failed fixture.

## Corrected live result

Run `57cc2180-ce75-46be-b6c3-bbcc12a1d5a7` passed, 2026-10-01
02:01:41–02:08:04 UTC, 382.858 s including setup. Binary SHA-256:
`b5458d260f6f06b95bc916c3b040fdd1bc166e1a839fd1ab7e47d1cd0166ec7c`.
The [manifest](icloud-replace-stream-recovery-57cc2180-ce75-46be-b6c3-bbcc12a1d5a7-live.json)
records interruption exit 86 and fresh recovery exit 0. Exactly 8,388,608 of
67,108,881 bytes were yielded to the replacement body's transport at interruption;
server acceptance of that prefix remains unknown.

The new process recovered the original completed receipt, the full new local
generation and the allocated/no-receipt Stage checkpoint. One inspection and one
reconciliation proved the Stage uncommitted, with zero prohibited replay attempts.
Before and after that guarded phase, the exact original was the only visible
owned file and its old revision/content independently matched. The operation
returned to `pending`, retaining its new bytes and clearing only the old transport
checkpoint. Local export passed; a separate post-run full reread confirmed its
67,108,881 bytes and SHA-256 against the prepared payload (`export-audit.json`).

A subsequent normal worker attempt finished `uploaded`, using a fresh document
identity. Independent full remote hashing matched the new varied-content payload;
exactly one final file was visible in the owned folder. The original exact ID was
confirmed in Trash. The normal handoff provider checked original Trash bytes
before reporting completion; no additional independent final Trash-body hash
oracle was run. This validates this controlled staging-body interruption, not all
replacement race/failure boundaries or installed behavior.

All source/export/journal/receipt evidence remains at
`.local-state/icloud-account-replace-stream-recovery-57cc2180-ce75-46be-b6c3-bbcc12a1d5a7/`;
logs and supervisor records are at
`.local-state/icloud-replace-stream-recovery-57cc2180-ce75-46be-b6c3-bbcc12a1d5a7/`.
The failed predecessor remains a separate retained artifact. Temporary storage
was disk-backed btrfs; nothing compiled or measured concurrently. No regular
service, mount or installation was changed.

The phase log also shows repeated roughly 30-second root-folder observations
within handoff verification. This is diagnostic evidence for inspecting whether
exact known-folder metadata can preserve those guards without repeatedly listing
the root; it is not a controlled performance comparison or a relaxed guard.

Complete `scripts/check.sh` passed after the live attempts, 2026-10-01
02:09:54–02:15:57 UTC, including formatting, clippy, workspace/feature/kernel,
script, translation, ledger and documentation checks. Manifest/log:
`.local-state/icloud-replace-stream-check-2026-10-01/`; temporary storage was
btrfs. No GUI changed, so native-window scenarios were not rerun. The failed
large-write regression was shown red before its fix and both replacement-queue
tests passed afterward. The Stage-only guard tests also pass.

Follow-up caution: the current handoff parent scan checks both exact folder
identity/parent/name/kind and sibling-name uniqueness (`HandoffPlan::folder_present`).
An exact-folder response alone does not establish sibling-name uniqueness. A
replacement for that scan needs an explicit identity/conflict contract and tests,
not a mechanical substitution that silently drops a check.
