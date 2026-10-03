# Mounted iCloud zero-byte replacement and refill

Registered before invocation on September 30, 2026. Run `21d05f56-70c4-47a8-a5e3-577ab68994ac`.

Question: can the normal account router and shared FUSE writer truncate a known
file to zero bytes and subsequently replace that empty file with new contents,
retaining both exact predecessors in recoverable Trash?

## Implementation and synthetic controls

The normal create adapter now supports the independently validated zero-byte
response/registration contract. The replacement adapter, handoff constructor and
handoff inspections are extended to permit zero-sized originals and staged files.
All upper bounds, ID/ETag/digest/parent/name guards remain. A zero-byte Trash
backup requires an explicit numeric zero size, matching empty digest, unique exact
identity, valid ETag and recovery path. It skips content download only for size
zero and still requires a second unchanged Trash observation.

Two new tests cover zero-sized source/target checkpoints and zero-sized handoff
checkpoint restore. Both failed before the change (two failures, one existing
transport test passed), then all three passed. Corrupt request sizes remain
rejected. The tests do not establish Apple's live behavior.

## Arms and prediction

Use `--account-mounted-empty-replace RUN_UUID` with a newly created owned remote
root, disabled isolated account/state, regular ICloudWriteProvider and the existing
receipt-based authorization wrapper. No installed service or user-owned target
is changed. A separate Python process creates and fsyncs known nonempty bytes;
wait for Uploaded and independently verify its digest. Then O_TRUNC/fsync to zero,
wait for Uploaded, verify zero size/empty digest and old exact ID in Trash, and
read EOF through the mount. Then write known nonempty replacement bytes over the
empty file; wait for Uploaded, verify those bytes and the zero-byte predecessor's
exact Trash identity. Shut down, reopen the components and mount, and read the
final bytes again. Retain all fixtures and journals.

Prediction: both replacements complete with different current IDs and verified
recoverable predecessors. Apple may instead omit or alter zero-byte Trash metadata;
such a result must stop with retained state, never relax recovery identity based
on a name match or automatically replay an uncertain attempt.

One functional arm; no timing/reliability claim. Allow 1800 seconds, record binary
hash, command, PID, disk-backed TMPDIR/SQLITE_TMPDIR and findmnt before invocation.
No competing compilation or cloud measurement. Ordinary iCloud write grants
remain disabled. This arm does not cover concurrent actors, large files or the
installed daemon.

## Result

The arm finished successfully (exit 0) after 924.709 seconds. The regular
account router and real FUSE mount created a nonempty file, truncated it to zero,
and refilled it. Independent digest observations matched each version; both
exact predecessor IDs were present in recoverable Trash. Empty reads returned EOF.
After shutting down and reopening the mount, an external Python process read the
final expected bytes. The final `passed.json` records all five endpoints as true.
An exact findmnt lookup observed `fuse.cirrove` during the run and no mount after
successful shutdown. The installed service was not changed.

The authoritative lifecycle/result is
`icloud-mounted-empty-replace-live-2026-09-30.json` and its private manifest.
This is one functional arm, not a reliability or performance acceptance result.
The two replacement postflights took 129.7/126.2 seconds, install preflights
64.3/61.7 seconds, and final receipt checks 128.6/128.0 seconds. These observations
identify expensive verification work; they do not establish its cause or spread
under controlled repeated performance arms.

Full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed
(exit 0): formatting, clippy, workspace and feature tests, real kernel mounts,
script checks, ledger and documentation. The log is retained at
`.local-state/icloud-empty-replace-full-check.log`. Desktop window scenarios
are outside this script and were not run for this change.

## Follow-up questions identified while the live arm runs

Large-file work must address more than the scattered 32 MiB validation bounds:
ICloudReadSession has a 30-second HTTP client timeout, the shared streamed-upload
worker permits 900 seconds, and WriteContext currently opens a 64 MiB journal
budget. Verification streams bounded chunks but uses the same short HTTP client.
Changing only MAX_FILE would therefore not establish useful large-file support.

Replacement latency also requires controlled investigation. Current Trash
verification retrieves the complete Trash folder twice around content inspection.
The existing direct-item lookup uses retrieveItemDetails, and the public
[rclone transport source](https://github.com/rclone/rclone/blob/master/backend/iclouddrive/api/drive.go)
uses that endpoint for exact IDs as well. A future read-only experiment can test
whether it supplies sufficient recoverability metadata for a known owned Trash
ID; it must not replace full membership/revision checks on assumption. No rclone
runtime dependency is proposed. No competing live query was run during this arm.
