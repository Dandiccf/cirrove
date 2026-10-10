# Native backup-first saves and schema19

Registered before tests. Question: can a native canonical archive be renamed to a local backup, promoted or restored without losing either stream or confusing provider identity?

Prediction: the retained-listing regression fails with the old canonical-name comparison during an explicit backup gap; the narrow comparison fix permits only the exact provider artifact with its original name and parent. All backup journal tests then pass: rollback, occupied destination, transaction failure, cancellation, stale selection after acknowledgment, promotion, retained descriptors, read-only exports, and atomic schema18-to19 migration preserving dirty bytes.

Synthetic tests only, private disk-backed temp and isolated Cargo target. No real journal migration, daemon restart or cloud operations. Schema19 is required because the real isolated validation journal already reached18. Existing running installations remain untouched. The mounted integration is a separate subsequent check.

Results pending.

## Observed journal results

`native-backup-listing-before` failed its one executed regression at the expected namespace-overlay unwrap with Corrupt (exit101). After restoring the narrow name comparison, `native-backup-gap-fixed` executed all8 backup tests and passed (exit0), including interrupted schema18-to19 migration and exact-byte read-only recovery. These are synthetic journal results; mounted backup-first application behavior remains unverified.

## Mounted smoke and registered dispatch counterexample

The first actual synthetic FUSE smoke executed one test and passed: backup/rollback, promotion through three pending saves, held old descriptor writes, backup reopen/unlink and suffix reuse, and fresh mount reconstruction. This initial pass alone does not prove test sensitivity. Before claiming dispatch coverage, disable only rename_native_backup dispatch; prediction: first canonical-to-backup rename fails Unsupported. Restore source then run all native archive kernel scenarios. No live provider or installed state is involved.

The dispatch counterexample `native-backup-dispatch-before` failed at the first canonical-to-backup rename (native_archive_edit.rs:699) with Unsupported; its one executed test failed as predicted. Source was restored byte-for-byte. `native-backup-kernel-restored` then ran all6 native archive FUSE tests, with zero ignored, and passed. This includes direct atomic replacement, in-place writes, open/path truncate, temporary lifecycle, pending-save chain reconstruction and the new backup-first workflow. Real Apple editor acceptance remains open.
