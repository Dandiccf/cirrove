# Consecutive atomic editor saves through an iCloud conflict

Registered before the live arm, 2026-10-01. Question: can an isolated writable
mount retain two consecutive atomic editor saves after a competing iCloud edit,
rescue the newest local bytes after restart, and clean only the two confirmed
editor temporary identities after the rescue receipt?

Use a fresh UUID-owned validation folder and the ordinary account router. Create
and verify the original and first editor temporary. The existing one-shot owned
hook edits the exact original between replacement preparation and commit; the
normal adapter must report Conflict. Upload and independently hash a second
editor temporary with different bytes, then atomically replace the still-local
name. Require the second replacement to remain Pending with no checkpoint behind
the first conflict. A held intermediate descriptor must still read its original
bytes across that second replace.

Restart the isolated mount, invoke keep-both for the first refused save, then
require the newest local content in the rescue path and the independently edited
cloud content at the original name. Both replacement saves resolve together;
neither resolution fabricates a replacement receipt. Supersede only never-attempted
cleanup rows; append two exact-ID conditional cleanup operations after the rescue
Create. Require the final independent rescue digest, both temporary IDs in Trash,
unchanged competing original, and both named versions after another remount.
An independent read-only journal audit must verify all retained save digests,
distinct current/cloud owners, the two cleanup targets, backward prerequisites
and no incomplete queue reservation.

Prediction: a joint local transaction rescues the latest stream, retains every
older local stream and restores only the oldest cloud victim's visible alias.
Reverse-order release of detached intermediate bindings makes each temporary
cleanup own its correct provider ID. No cleanup is eligible before the rescue is
confirmed. Invalid/attempted/external dependencies remain refused. This arm is
functional evidence, not repeatability or arbitrary editor/namespace acceptance.
Provider-internal staging cleanup and ordinary write release gates remain open.

One live arm, 1800-second deadline, recorded command/PID/binary SHA and private
btrfs TMPDIR/SQLITE_TMPDIR before execution. No concurrent compilation or other
measurement. Only new Cirrove-owned items are mutated; no permanent deletion,
regular-service restart, installed access-mode change or other-account mutation.
Retain failed arms and all private evidence. Confirm the isolated mount is detached.

## Synthetic validation before live execution

The two-replacement journal regression failed with the previous rescue logic
(`Stale`) and passes with the extension. Three-link coverage additionally checks
newer unsealed bytes, preserved older working streams, transactional rollback of
all owners/cleanups, and refusal of attempted cleanup before dirty-byte sealing.
All namespace replacement tests passed (20 passed, one subprocess helper ignored).

The first kernel fixture stopped in a helper that refused every Conflict, including
the deliberate one. After targeting only the second temporary's upload, the true
negative control with the old rescue code failed (keep-both returned zero).
The same actual FUSE scenario passes with the extension, including open original
and intermediate descriptors, both visible versions, receipt-gated cleanup and
remount. Logs remain under `.local-state/icloud-chain-rescue-*.log`.


## Live result

Run `603d7183-48e7-479e-92e2-f76c08ab130d` passed, 04:32:01–04:38:55 UTC,
413.653 seconds including setup and restarts. Binary SHA-256
`8551ef5d8c088ef365ac2432daa846134795d0b4443a885c95be4bfaedd90ee7`.
The [live manifest](icloud-mounted-competing-chain-603d7183-48e7-479e-92e2-f76c08ab130d-live.json)
records exit 0 and detached isolated mount. The normal adapter refused the actual
competing edit; the second replacement remained untouched behind it. Keep-both
after restart uploaded the newest local bytes under a distinct ID. Full remote
hashes and both mounted paths matched after another remount. Both editor temporary
IDs were independently found in Trash, and the original competing version remained
unchanged. No normal service or installed access mode changed.

The independent [read-only journal audit](icloud-mounted-competing-chain-603d7183-48e7-479e-92e2-f76c08ab130d-journal.json)
passed integrity_check, reread and hashed both superseded payloads, verified that
the rescue uses the newer stream, and found one owner per visible cloud version.
Both original cleanup rows are Resolved without receipts or attempts; both appended
cleanups are Applied and depend on the rescue Create. Only the two exact temporary
IDs are cleanup targets. All prerequisites point backward and no queue reservation
is incomplete. Earlier detached victims keep distinct local identities.

Private evidence remains in
`.local-state/icloud-account-mounted-competing-chain-603d7183-48e7-479e-92e2-f76c08ab130d/`
and `.local-state/icloud-competing-chain-603d7183-48e7-479e-92e2-f76c08ab130d/`.
The audit runner is `.local-state/audit-icloud-competing-chain.py`.
No performance or repeatability claim is made from this one arm.

## Final validation

The complete `scripts/check.sh` passed with exit 0 on 2026-10-01,
04:40:16–04:46:42 UTC, including formatting, clippy, workspace and script tests,
real FUSE scenarios, and the ledger. The new consecutive-atomic-save FUSE
regression passed in this full run. Private manifest and log remain under
`.local-state/icloud-atomic-chain-check-2026-10-01/`; both temporary-directory
variables pointed to `/var/tmp/cirrove-atomic-chain-check-_k4rwu3a` on btrfs.
The existing rustdoc link warning for `filesystem::writeback` remains. No UI
behavior changed, so native window scenarios were not rerun. This result does
not constitute installed acceptance; the ordinary daemon was not replaced.
