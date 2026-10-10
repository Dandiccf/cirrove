# Mounted iCloud GiB acceptance arm — 2026-10-01

Registered before the live run. Only a new Cirrove-owned validation folder may
be mutated; the regular installed account and mount stay unchanged.

## Question and prediction

Can the actual account router and FUSE path create 1,073,741,841 bytes, replace
that file with 1,074,790,429 bytes, independently verify both remote SHA-256
values, retain the predecessor's exact identity in recoverable Trash, and read
the entire replacement through a fresh mount?

Prediction: the existing streamed upload/read paths preserve bounded buffering
and exact content at this size; the 8 GiB journal budget admits working and sealed
copies. The 900-second transfer/receipt deadlines may expose a slow-link limit.
This is a functional arm, not a performance comparison. One success establishes
only this size/workflow under the recorded conditions, not broad reliability.

## Method and endpoint

Feature-gated `cirrove-icloud-mounted-write-probe --account-mounted-large RUN_UUID 1024`.
The original 65/66 MiB entry point and budget remain unchanged. The new size option
is explicitly bounded to 65..2048 MiB before session load or cloud setup.
The application writes and reads 64 KiB blocks, never a GiB-sized allocation.

Register command, fresh run UUID, binary SHA-256, revision/dirty state, private
disk-backed TMPDIR/SQLITE_TMPDIR, filesystem and PID before starting. Require at
least 16 GiB host free space. No other validation or compilation runs concurrently.
Overall deadline 3600 seconds; retain every partial local/cloud fixture on failure.
An exact isolated mount may be detached after the process finishes; nothing is
recursively removed. Existing personal files and previously failed fixtures are
never targets. No permanent cloud deletion is used.

Pass requires all existing `passed.json` endpoints, exit zero and detached mount:
create/replace digests, original in Trash, fresh-mount full read. A timeout,
permission/quota error or uncertain upload is a failed arm with retained evidence.
The supervisor records terminal status and exit code in a tracked JSON artifact.

Synthetic size-boundary test: passed (one executed test). Live outcome: passed.

## Outcome

Run `57332ed3-c6ef-4763-9dd6-df7ff95b9455` passed all registered endpoints with
exit zero and its mount detached. [Tracked manifest and phase observations](icloud-mounted-gib-57332ed3-c6ef-4763-9dd6-df7ff95b9455-live.json)
record the binary, process, filesystem and exact sizes. Binary SHA-256:
`bbe56e3b2ae69098471b34872b3463a075f555705c81023c934e2e84293ed023`.
Elapsed wall time was 1849.802 seconds (about 30m50s). The regular installation
was not restarted, replaced or made writable.

The two content-upload phases took 235.824 and 235.551 seconds. Across the entire
validation process there were 12 verification-body downloads (27.876–28.191 s),
three Trash-body downloads (28.168–28.557 s), 272 download lookups, 629 folder
metadata observations and 11 root metadata observations. These counts include
independent verification and ownership guards. Their sums are not a decomposition
of product latency and this single arm supports no general throughput claim.
They motivate a separately controlled investigation of request amplification.

The payload uses one repeated byte per generation, with different bytes between
versions. It detects wrong versions, truncation and corruption, but does not test
reordered equal-valued chunks within a generation. Varied-content repetitions,
in-flight interruption and larger sizes remain open. No RSS endpoint was
registered, so this is not a measured bounded-memory result.

The CLI size extension changes the validation harness, not normal account write
permissions. Ordinary iCloud writes remain disabled pending the remaining gates.
Full `scripts/check.sh` passed after the live arm, 2026-10-01
00:47:25–00:53:29 UTC, including formatting, clippy, workspace/feature/kernel,
script, translation, ledger and documentation checks. Its private manifest/log
are retained under `.local-state/icloud-gib-account-check-2026-10-01/`; temporary
files were disk-backed btrfs `/var/tmp`, using the worktree's own Cargo target.
No build or second validation ran during the live arm. No GUI behavior changed
in this step; native-window scenarios were not rerun.

## Follow-up identified from code inspection

Ordinary `ICloudDrive::read_range` currently performs parent metadata lookup,
download-representation lookup and a final metadata check for each range.
Correction after inspecting the separate hooks: package artifacts use
`staged_content_session`; `open_read_session` had no iCloud override. The existing
provider-neutral `ReadSession::read_window` interface permits larger streamed
windows into private staging, published only after complete validation; ordinary
iCloud files do not yet use it. This is a concrete candidate for reducing request
amplification while keeping before/after revision checks.

Any follow-up comparison must register its own fresh-cache arms and use varied
content. The mounted validator's `View` currently forwards `read_range` but not
`open_read_session`; it must forward the latter with the same owned-item checks,
or an adapter session change would not be exercised by this validator. No cached
signed URL alone may be treated as a content-version guarantee. Performance and
interruption acceptance remain open.
