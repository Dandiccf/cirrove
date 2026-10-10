# Registered full-device recovery validation

Question: can both a retained sealed save and newer dirty working bytes be
exported from a genuinely full local device without changing journal queues?
Prediction: read-only recovery needs destination storage but no free source
blocks; both exports should preserve exact source identities, bytes and hashes.

## Controlled arm

Use a fresh Arch VM overlay backed by the existing stopped fixture image, with
QEMU restricted networking, no shared host folders and no provider access. Attach
only a new, serial-identified 64 MiB raw disk for the full-device fixture. Verify
serial, exact size, no partitions and no mounts before formatting ext4 with zero
reserved blocks. Use a new private child directory; retain lost+found unchanged.

Prebuild the exact working_files test binary, record its SHA-256, transfer it and
verify its hash, ABI and exact test name before formatting. Run only
`a_genuinely_full_filesystem_explains_what_to_free`, ignored/exact/single-threaded,
with a 120-second deadline. Disk-backed TMPDIR, SQLITE_TMPDIR, exports and logs
are on the separate guest root filesystem. No compilation or second measurement
runs concurrently. Record PID, command, filesystem, timestamps and result.

The test independently models acknowledged working writes, verifies retained
bytes before exporting, and requires an actual ENOSPC probe immediately before
each export. Both receipts, sizes, hashes and unchanged journal records must
match. Restoring space happens only after both export attempts. A failed arm is
retained, not replaced in the report. The host/base image is never filled or
modified; preserve the overlay and private image/logs after guest shutdown.

## Scope

This is a correctness arm, not a performance result. It covers the first actual
failing write/truncate/seal path on this ext4 fixture, not every possible partial
write or installed GUI lifecycle. Real iCloud account quota is not filled.

Status: arm 1 failed during boot; arm 2 passed the controlled core recovery test.
See results below; installed GUI and broader failures remain unverified.

Arm 1 registered at 2026-10-01T06:12:02.580699+00:00.
Private runtime evidence: `/var/tmp/cirrove-enospc-20261001-arm1`. Runner SHA-256:
`ef3ee6fb54dbfd563555da2d93e0a36ad0ba0b09f04313f457f145ea3d178f91`.
Selected executable: `/home/dandiccf/Work/cirrove/.local-state/worktrees/icloud-feasibility/.target-icloud-feasibility/debug/deps/working_files-d2b9bee3c2a689c8`.
The runner records its exact command, executable hash and process IDs before execution.

Arm 1 failed during VM boot: firmware reported no bootable device. No test
or filesystem formatting ran. Screenshot and boot diagnosis are retained with
the arm; the exact owned QEMU was stopped through its registered QMP socket.
Base metadata remained unchanged. This is not a failed recovery assertion.

Arm 2 changes only explicit root-drive boot priority (`bootindex=1`). The
fixture remains a fresh 64 MiB disk on a new overlay. Prediction: selecting
the actual system disk permits guest boot; this hypothesis is not yet verified.

## Observed result

Arm 2 passed on 2026-10-01, 06:16:08 UTC (one exact test, exit 0).
The first rejected operation was `write_working`, reported as `DeviceFull`.
The fixture retained 55,092,224 ballast bytes on its 64 MiB ext4 device.
Independent probes returned ENOSPC before both exports. The sealed eight-byte
save and distinct generation-2 eight-byte working version exported with exact
receipt identities, sizes and SHA-256 values; journal records stayed unchanged.

The guest used separate ext4 devices for source and export/TMPDIR/SQLITE_TMPDIR.
QEMU exited 0 and the backing image's size, inode and mtime remained unchanged.
No ordinary host daemon or cloud data was changed. The first failed boot and
successful arm remain preserved; [machine-readable evidence](icloud-full-device-export-2026-10-01.json)
records both. This proves the bounded recovery-core case, not arbitrary partial
writes, all disk failures, or installed recovery UX.

## Complete repository validation

`scripts/check.sh` passed on 2026-10-01, 06:36:17–06:44:13 UTC (exit 0),
including formatting, clippy, workspace/feature tests, real kernel mounts, scripts
and documentation. Manifest/log: `.local-state/icloud-readonly-recovery-check-3-2026-10-01/`.
Both temporary-directory variables used private btrfs storage. Native window
acceptance is recorded separately in the read-only recovery benchmark. This is
worktree validation; the ordinary installed daemon was not replaced.
