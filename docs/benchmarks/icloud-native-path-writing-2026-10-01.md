# Native archive pathname write admission

Question: does the original generated archive inode admit an explicitly writable
account into a typed local native stream, preserve already-open immutable readers,
and seal a typed replacement without granting writes to previews or other scopes?

Register before execution: run `cargo test -p cirrove-service --lib native_path_`;
then the real synthetic kernel arm
`real_native_archive_first_open_in_place_fsync_retains_old_reader_and_reopens_local`
in writable_session with --ignored. This drives actual open/read/write/fsync,
held-original reads after synthetic handoff and offline provider, local reopen,
and read-only refusal. The receipt is injected: this is filesystem proof, not
Apple protocol or application fidelity proof.

Prediction: first writable original-artifact handle must use the new local UUID,
while the unread original reader must retain its exact immutable session. Remove
the native snapshot read branch as a negative control: the held-old kernel read
must fail once the provider has removed the original and gone offline. Restore
source before acceptance. Reviewer requested separate in-flight read barrier and
pathname truncate/O_TRUNC coverage; those remain pending, not implied by handle
truncate. Atomic temp/rename saves and clean retirement remain separate gates.

No real account or installed daemon is involved. Use one exclusive run at a time,
private disk-backed TMPDIR and isolated target directory. Full check required
before commit.

Initial four native_path tests passed. Removed an unused test-only glob import
reported by the compiler. Added reviewer-requested coverage: a deterministic
already-started Remote token barrier and separate actual-kernel pathname truncate
and initial O_TRUNC arms. These kernel cases must run serially. Before acceptance,
remove the reader drain to prove its dedicated barrier test rejects early return.

All five native_path tests passed with the new in-flight barrier case. Bypassing
the reader drain failed immediately at "native snapshot pin returned while an
earlier remote read still held its token". Restored the exact source before
starting the three serial actual-kernel synthetic archive-write cases.

All three serial actual-kernel tests passed: first writable open with handle
truncate, pathname truncate before writable open, and initial O_TRUNC. Each
preserved the unread original reader after injected publication/offline source,
and kept new local reads and RO refusal correct. This is synthetic receipt proof.
Now run the registered snapshot-branch negative; additionally remove only native
O_TRUNC forwarding and restore old package refusal for pathname truncate as
separate controls. Expected failures are held-reader read, nonzero size after
O_TRUNC and refused pathname truncate, respectively. Restore after each.

All registered kernel controls failed where intended: without snapshot selection,
held.read_to_end returned EIO after the old provider identity was removed; without
O_TRUNC forwarding, the admitted stream retained131210 bytes instead of zero;
restoring the old package guard made actual pathname os.truncate fail with
EOPNOTSUPP. The exact production sources were restored after each control. No
faulted binary touched a real account. Re-run all three kernel cases together
on restored source, then complete repository validation before commit.

All three kernel cases passed again on restored source (2.59 seconds for the
fixture group, not a live latency measurement). The upcoming full check covers
shared-service regressions; no Apple application/open or installed write claim
follows from these synthetic results.

The first full check stopped at clippy enum_variant_names in the three-arm
kernel fixture. Renamed FirstEdit variants to Handle/Path/Open; behavior and
assertions unchanged. Public Stage recovery will be integrated and tested before
the next complete check; no failed full check is counted as passing.

Full `scripts/check.sh` passed in arm `native-path-recovery-fullcheck`: format,
clippy, workspace and iCloud feature tests, actual-kernel mounts, script tests,
Strata/Dolphin integration checks and acceptance ledger. Existing rustdoc link
warning for `Writeback::retry_stuck` remains. Window scenarios are not included.
No installation or real-account mutation occurred.
