# iCloud in-flight content upload: registered process-recovery check

Registered before live execution, 2026-10-01. Question: after abrupt process loss
while streaming a create's body, can the regular account router retain the sealed
local generation, prove the visible create is uncommitted, and retry with a fresh
identity without duplicate visible files?

Use one new Cirrove-owned folder and a 64 MiB + 17-byte position-dependent fixture.
The developer-only binary installs a process-local body boundary for its exact
file name/size. After at least 8 MiB has been yielded to reqwest's body, the next
read remains pending. An awaiting task records only counts and then exits 86
without returning from the pinned worker or orderly journal shutdown. This
controls the unfinished HTTP-body boundary; bytes yielded are not proof of how
many bytes Apple accepted. Normal binaries contain no boundary/observer code.
No network settings, other clients, installed daemon or real user files change.

Before the stream, the worker must have saved the allocation checkpoint. A fresh
process must observe `verify_required`, verify the whole local payload hash and
an allocated slot with no saved upload receipt. Registration requires a persisted
receipt, so it cannot have started from that retained checkpoint. A read-only
guard allows only inspection and reconciliation of this exact operation/request
and allocated checkpoint; any attempted begin, body transfer, final registration
or other step fails the arm. The owned remote folder must be empty before/after
this guarded phase. Expect one inspection and one reconciliation, `pending`
(proved uncommitted), preserved local payload, and cleared old transport state.

Export that pending generation to a fresh local non-FUSE file and verify its
receipt/hash without changing the operation. Only then permit a normal worker to
retry. Expect `uploaded`, a different document identity from the interrupted
allocation, an independent complete remote digest and exactly one visible owned
file. No other cloud mutation or deletion is allowed. Keep all artifacts,
including failures and any unregistered remote allocation. This does not claim
the server has reclaimed its incomplete upload-slot bytes.

One interruption/recovery pair, 1200 s each and one measurement at a time; no
compilation during the live pair. Record commands, binary SHA-256, process IDs,
private disk-backed TMPDIR/SQLITE_TMPDIR and filesystem before starting. The
supervisor starts recovery only after exact exit 86 and a valid interruption
record. Stop on failure; never replay an unsuccessful fixture automatically.

Prediction: the allocated/no-receipt checkpoint distinguishes this case from a
lost registration acknowledgement. Reconciliation should authorize a fresh
attempt without reusing the interrupted transport, and both local export and
remote digest should match. This is one controlled create boundary, not proof of
all network failures, atomic replacements, quota/session failures or installed
file-manager acceptance. No speed or RSS comparison is registered.

## Live result

Run `1a2f15e5-83ca-4b43-92dc-5f895030007c` passed, 2026-10-01
01:41:21–01:43:18 UTC, 117.291 s total. Binary SHA-256:
`81c7f0886ac92fdf3870379e5c109cdd99f4ae6b8db66d732064040f5805dfbe`.
The [manifest](icloud-stream-recovery-1a2f15e5-83ca-4b43-92dc-5f895030007c-live.json)
records both process IDs, commands and terminal results: interruption exit 86;
fresh recovery exit 0. Exactly 8,388,608 of 67,108,881 body bytes had been yielded
at the boundary. Remote acceptance of that prefix remains unknown.

The new process recovered `verify_required`, the complete local generation with
its expected hash, and the allocated checkpoint with no receipt. One inspection
and one reconciliation through the regular account router proved `uncommitted`.
The guard counted zero prohibited attempts. The owned folder was empty before
and after this phase. The worker cleared the old transport checkpoint and returned
the same operation to `pending`; it did not reuse the uncertain stream.

The pending local generation was exported and verified without changing that
operation. A subsequent regular worker attempt completed with a different
allocated document identity. Independent complete remote SHA-256 matched the
position-dependent source and the owned folder contained exactly that one file.
The final journal state was `uploaded`. The manifest's
`allocated_checkpoint_retained` refers to recovery preflight, before the verified
uncommitted transition deliberately cleared it.

Private evidence is retained in
`.local-state/icloud-account-stream-recovery-1a2f15e5-83ca-4b43-92dc-5f895030007c/`
(including export, journal, encrypted credentials and receipts); supervisor logs
are under `.local-state/icloud-stream-recovery-1a2f15e5-83ca-4b43-92dc-5f895030007c/`.
Temporary storage was btrfs under `/var/tmp`. Nothing compiled or measured in
parallel. No regular service restart, installation, FUSE mount or personal file
mutation occurred. This is one create-stream interruption; it does not close
in-flight replacement, registration-acknowledgement loss, concurrent mutation,
server cleanup of unregistered slots or all transport-failure gates.

Post-run validation: complete `scripts/check.sh` passed 2026-10-01
01:44:58–01:51:23 UTC, including formatting, clippy, workspace/feature/kernel,
script, translation, ledger and documentation checks. Manifest/log:
`.local-state/icloud-stream-recovery-check-2026-10-01/`; temporary storage was
btrfs. An additional all-targets iCloud write-probe clippy run passed after fixing
four existing test-only `unwrap()` calls to carry explicit fixture expectations.
The boundary/transparent-reader tests and checkpoint-envelope test passed.
An independent post-run reread of the exported local file confirmed all
67,108,881 bytes and its complete SHA-256 against `prepared.json`; its private
receipt is `export-audit.json` beside the retained export. No GUI changed;
display scenarios were not rerun. No installed service changed.
