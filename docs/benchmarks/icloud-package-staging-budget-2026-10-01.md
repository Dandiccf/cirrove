# Native iCloud package staging ownership

Question registered before the tests: can artifacts evicted from the two-entry
provider cache remain on disk through active read sessions without being counted?
Prediction: cache length and transfer concurrency do not bound those retained
files. Admission must remain reserved until the final file owner, including a
blocking task whose async waiter was cancelled, releases it.

## Implementation boundary

Each configured provider instance has four file reservations. Every archive is
already limited to the configured per-artifact byte limit before each write.
The resulting bound is four times that limit for archive data in this provider's
private staging, separate from the ordinary block cache. This is a conservative
whole-file reservation, not a claim that all four archives consume the limit.
Filesystem allocation overhead and other provider instances are not counted.

Admission first uses a free reservation; on pressure it drops cache ownership and
tries again. Active readers retain their files. If all reservations remain held,
new staging reports Unavailable rather than waiting indefinitely or invalidating
a reader. Retrying after a reader closes can proceed. The anonymous file and its
reservation share one Arc; bounded blocking writes and reads retain that Arc even
when their async waiters are cancelled. Errors and final-owner drop close the file
and release the reservation without a directory cleanup traversal.

## Regression evidence

The synthetic test `staging_reservation_outlives_cache_eviction_and_cancelled_waiters`
uses the production allocation path. Four artifacts retained by read
sessions deny a fifth allocation even after admission clears the two-entry cache;
existing bytes remain readable. A running
blocking task retains the last file after its session is dropped and its task is
aborted. Only its actual completion permits another allocation. Final drops return
all four permits and leave no named files.

A negative control removed lifetime ownership after production admission. The same
test failed at the fifth allocation (exit 101); restoring ownership passed.
The negative log is `.local-state/icloud-package-budget-red.log`.
These are local resource-ownership checks, not live provider reliability or a
measured host disk-capacity claim. Native writes and installed acceptance remain
open.

## Complete check

`scripts/check.sh` passed (exit 0), 2026-10-01 04:51:19–04:59:52 UTC,
including formatting, clippy, workspace/script tests, real FUSE scenarios and
ledger checks. Manifest and log remain in
`.local-state/icloud-package-budget-check-2026-10-01/`. TMPDIR and SQLITE_TMPDIR
used `/var/tmp/cirrove-package-budget-check-updhmayj` on btrfs. The existing
rustdoc `filesystem::writeback` link warning remains. No desktop behavior changed;
window scenarios were not rerun and the ordinary installation was not modified.

Independent read-only review found no blocking issue. Hash verification can retain
a reservation until its blocking pass finishes after cancellation; it cannot
exceed the reservation bound. Host-wide low-disk and multi-instance acceptance
remain separate gates.
