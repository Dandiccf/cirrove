# Disabled-account recovery validation — 2026-10-01

The new offline CLI path lists and exports retained sealed versions without a
service, credentials or a mount. It holds the account operation/owner locks and
opens the existing journal under its exclusive owner lock with a read-only SQLite
connection. It does not call normal `UploadJournal::open`, which performs real
recovery transitions, migrations, sealing and collection.

## Evidence

Eight local-export tests pass, including three added cases:

- Offline journal reads refuse a missing journal without creating it, a live
  owner, wrong account, unknown schema and symlinked journal/database/object
  paths. An interrupted `uploading` record remains `uploading`; main database
  bytes are unchanged after listing/export. The immutable payload is verified.
- Account-level recovery refuses enabled and still-owned accounts, holds the
  owner against new mounts, refuses state/mount targets and supports bounded
  sequence pagination. Configuration bytes remain unchanged.
- Actual `cirrove recovery-saves` and `cirrove export-save --offline` subprocesses
  return metadata and exact exported bytes from a synthetic disabled account,
  with no daemon or credentials. Unusual names round-trip through JSON.

Negative control: temporarily opening the journal through its normal recovery
path first made the state-preservation test fail (`VerifyRequired` instead of
`Uploading`). Restoring the pure reader passed all eight tests.

Initial implementation attempted SQLite NOFOLLOW through `/proc/self/fd`; SQLite
refused that alias. The corrected connection uses the normal pathname with
NOFOLLOW and compares directory/database inode identities with held descriptors
before accepting records. Payload reads stay anchored to the held directory.
Initial fixture calls to private settings methods and a clippy item-order error
were corrected before the full check.

## Limits

Current schema only, configured disabled accounts only, sealed generations only.
No automatic worker restart and no graphical offline picker yet. No throughput,
large-file or real-account claim is made. The regular installation is untouched.

## Integration corrections

Three additional regressions were executed and failed before their fixes:

- The actual offline CLI failed with `HOME is not set` despite an explicit
  `--state`, because `unwrap_or(state_dir()?)` evaluated the fallback eagerly.
  Both listing and export now work with HOME/XDG_STATE_HOME removed.
- Desktop eligibility rejected the active service's historical `verifyrequired`
  spelling. It now accepts both that spelling and `verify_required` without
  admitting unknown states.
- The actual writeback history returned sequences 1000..993 from a 1002-record
  journal instead of 1002..995. Its old ascending query silently capped the scan
  at 1000 before reversing. A descending SQL LIMIT query now selects the newest
  records directly, bounds output to 200, and preserves an empty zero-limit query.

These changes affect active recovery as well as offline recovery. The targeted
regressions passed after the fixes; the older complete check (2026-09-30
23:54:25–2026-10-01 00:01:43 UTC) had passed before these additions and is not
claimed as their validation. The full repository repeat passed, 2026-10-01 00:05:00–00:11:35 UTC:
formatting, clippy, workspace/feature/kernel tests, scripts, translations, ledger
and documentation. Manifest/log are retained under
`.local-state/local-recovery-offline-check-repeat-2026-10-01/`; both TMPDIR and
SQLITE_TMPDIR used private disk-backed btrfs `/var/tmp`, with the separate
worktree Cargo target. All 13 native window scenarios also passed after the
corrections. The regular installed daemon remains untouched.
