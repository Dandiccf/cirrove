# Isolated iCloud mounted replacement using the sealed session, 2026-09-29

## Registered before live execution

Question: can the feature-gated two-ID replacement use the encrypted,
keyring-backed Apple session for both the general Create staging adapter and
the conditional Trash/rename handoff, then reconcile its journal in a fresh
process? It previously retained an in-memory snapshot for those phases.

Arm A creates a new UUID-named test root under the isolated
`iCloudGuiValidation` account. A separate FUSE process creates and fsyncs a
small file and creates an empty sibling folder. Require final exact-ID
upload/folder receipts and independent full-byte verification. Arm B runs
`--replace` for that fixture, altering only the generated file. Require a
durable `HandoffComplete` with a new exact ID and full bytes under the same
name, the old exact ID with its original bytes in recoverable Trash, and no
extra visible staged item. Arm C runs `--resume-after-replace` in a fresh
process, reads through FUSE and independently rechecks the IDs, complete
bytes and journal without another cloud mutation.

Prediction: all three arms pass because the previous snapshot-backed fixture
passed this flow, while the sealed adapters have passed mounted Create and
independent mutation tests. A failure or uncertain result stops the sequence;
no operation is blindly repeated. Only the new Cirrove-owned fixture may
change. The normal account, installed daemon and other mounts remain
untouched and read-only. Endpoint: process exit, exact journal/remote IDs,
complete SHA-256, recoverable old ID, fresh-process read and mount shutdown.
One sequence gives functional evidence only, not timeout or concurrent-edit
reliability. Each arm has a private manifest with command, binary SHA-256,
PID, expected duration and disk-backed private `TMPDIR`/`SQLITE_TMPDIR`
registered before execution. No concurrent measurement or compilation runs.

## Initial observation and diagnostic registration

Arm A passed on owned fixture `37d5c6fe-ee91-4937-aa53-28096bbf191e`:
the mounted file and folder matched independent iCloud listing, exact journal
IDs and full file bytes. Arm B exited 1 with "mounted upload requires review".
Read-only reopening of the private journal found the original Create uploaded,
but the Replace row in `failed` with no upload checkpoint, no remote receipt,
zero transferred bytes and one failed attempt. Its target item ID and expected
ETag match the original receipt. The mount was absent after exit. No Arm C
was run and the failed Replace must not be blindly retried. The private arm
manifests are in
`.local-state/icloud-sealed-replace-control-e78ef8ca-d84b-4e10-af97-2aa82c1e24eb`,
with binary SHA-256
`931469c5cda3f5803ba461460439a50981720a60a48d5389b52e0e6ce6dedfd9`
and btrfs-backed temporary directories.

Diagnostic question registered before further execution: did the failure
occur in the journal ownership guard, the sealed replacement constructor, or
the mutation-free `begin_upload` preparation? Add a read-only feature-gated
inspection command for this exact retained fixture; it may parse its journal,
construct the adapter and call mutation-free preparation, but must not mount,
enqueue, send an Apple write or print item IDs, credentials, URLs or raw
provider bodies. Prediction: the result isolates a local validation mismatch
before any remote mutation. Preserve the failed row and original cloud file.

The read-only diagnostic exited before checkpoint preparation with
"replacement journal guard or constructor refused". Reopening the journal
showed the source and Replace intent share account and scope, exact item ID,
ETag, parent, valid digest and supported sizes. Source inspection found the
local mismatch: the fixture passed its projected root with `parent_id=None`
to the replacement constructor, which correctly requires the real iCloud
parent ID. The first arm's upload adapter had already used a separate node
with that parent. This is a local fixture error, not evidence that Apple
rejected the sealed-session replacement. The correction uses
`FOLDER::com.apple.CloudDocs::root` as the owned test folder's parent only
when constructing the replacement adapter.

Registered before a new diagnostic and live retry: rerun only the read-only
`--inspect-replace` command on the retained failed row to prove adapter
construction and mutation-free checkpoint preparation now pass. Do not
re-enqueue that failed row. If diagnostic passes, create a **new** UUID-owned
fixture and run fresh Create, Replace and fresh-process remount arms as
specified above. Prediction: all three new arms pass. Any uncertain outcome
stops the sequence; the original failed fixture and journal remain untouched.

The same read-only diagnostic passed after the correction: both adapter
construction and mutation-free checkpoint preparation succeeded. Its private
manifest is `diagnostic-after-fix.json` next to the failed arm manifests,
with binary SHA-256
`08dec79140710657db06092863c34895940f4809278769bf895eb74a6db1ef6a`.
The diagnostic was observed failing before the correction and passing after
it; no cloud mutation was sent by either diagnostic invocation.

The fresh retry's Create arm passed on owned fixture
`b35425ac-72a4-461c-b854-64431e9d1f9e`. Its Replace arm reached the
900-second validator timeout and exited 1. A read-only journal inspection at
the end found the original Create still `uploaded`, while Replace was
`verify_required` after 11 failed checks, with a durable checkpoint and all
37 replacement bytes transferred but no final remote receipt. No remount arm
was run and no replacement retry is permitted from this evidence. The test
mount was absent after exit. The private manifests are in
`.local-state/icloud-sealed-replace-retry-b892dc10-83a0-4d5b-a026-8e25b34b329d`;
both used binary SHA-256
`08dec79140710657db06092863c34895940f4809278769bf895eb74a6db1ef6a`
and btrfs-backed private temporary directories. Correction to the manifest:
Arm B's recorded expected window was 360 seconds, but the validator code sets
900 seconds for `--replace`. The actual timeout followed the code's 900-second
limit. No timing or reliability conclusion is drawn from these arms.

Registered before further diagnosis: inspect only the saved checkpoint's
**phase name** and the presence of its nested slot/receipt, then compare the
original exact ID, any staged reservation and the recovery item in independent
iCloud listings. Do not print checkpoint material, item IDs, signed URLs,
credentials or raw provider bodies. This read-only inspection must not start
the worker or requeue the uncertain Replace. Prediction: the phase and exact
item-presence booleans will distinguish a staged upload still awaiting handoff
from a handoff whose result was lost. Preserve the fixture and journal.

The first read-only phase inspection failed while decoding the saved outer
checkpoint. A corrected diagnostic printed only the length before parsing:
the exact `upload/<operation>` Secret Service value exists but is **zero
bytes**. It printed no secret, item ID or response body and performed no
cloud mutation. The phase and remote staged/old-item state remain unproven.
Its private manifests are `diagnostic-phase.json` and
`diagnostic-phase-length.json` beside the retry arms; the latter records
binary SHA-256
`64287b60d0a9be9dfb109bba8377ec693bc700daa89f986b7b8e080d388c107d`.
This reproduces the desktop keyring's previously observed large-value loss
for a second class of iCloud state. The worker correctly retained
`verify_required` rather than resending an operation whose checkpoint it
could no longer read.

Next implementation question: can the shared transfer worker retain iCloud
upload checkpoints in authenticated per-operation files, with only short
keys in Secret Service, using the same sealed-storage scheme as the iCloud
account session? The key must be scoped to account and operation; a missing
or zero key must never be treated as a safe retry. Synthetic crash/readback
checks precede a new owned live fixture. The existing uncertain fixture is
not retried or cleaned up.

The isolated mounted writer now supplies a sealed upload-checkpoint vault to
the shared transfer worker. Its files live under the fixture's private state
directory and use a separate authenticated domain from account sessions.
The existing account session file format and associated data are unchanged.
A synthetic test first reproduced the observed empty return for a large
direct-keyring value; the same checkpoint then survived sealing, short-key
keyring storage and a newly constructed vault instance. Copying its sealed
file into a different account's directory was rejected by authentication.
This proves local retention under that synthetic failure model only; it does
not resolve the retained uncertain remote replacement or establish Apple
write reliability.

Registered before another live arm: after the full local check, create a
third new UUID-named owned fixture and run Create, Replace, then a separate
fresh-process remount with the sealed checkpoint vault. The Replace process
has a 900-second validator bound; each private manifest must record that
actual bound and a disk-backed temporary directory. Prediction: if the
earlier 900-second delay was caused solely by the zero-byte checkpoint,
Replace will reach a durable handoff and the remount will verify both IDs,
original bytes in recoverable Trash and complete new bytes. If it becomes
uncertain again, stop and inspect without retrying. The two earlier fixtures
remain untouched.

## New owned live fixture

The three arms passed for fresh owned fixture
`216724cf-3466-47d3-9f13-ce09c9ae9f83`. Mounted Create produced
confirmed file and folder receipts with full bytes under independent iCloud
listing. Mounted Replace then completed its two-ID handoff: the journal and
independent listing confirmed the new ID and complete replacement bytes,
and the old exact ID with original bytes remained recoverable in Trash.
The separately started remount verified the resulting file, both IDs and
journal without issuing a second mutation. Each process shut down its test
mount. Private manifests `arm-a.json`, `arm-b.json` and `arm-c.json` are in
`.local-state/icloud-sealed-replace-final-955d5304-0d11-431b-a9b6-4b53071078ce/`;
they record the binary SHA-256, PID, expected window and btrfs-backed
temporary directories. Arm B's manifest records the actual 900-second
bound. No compilation ran during the arms.

This is one successful sequence after the storage correction. It establishes
that the sealed-session mounted replacement can complete and survive a
fresh process on an owned small fixture; it does not establish timeout,
large-file or concurrent-editor reliability. The previous uncertain
`b35425ac-72a4-461c-b854-64431e9d1f9e` fixture remains `verify_required`
and was not retried or cleaned up. Ordinary iCloud connections remain
read-only.
