# Regular iCloud writer through an isolated FUSE mount

Preregistered before any invocation of `--account-mounted` on 2026-09-30.

Question: do actual kernel create/truncate/fsync/read calls reach the regular
account router and complete a two-ID iCloud replacement, with correct bytes after
unmounting and remounting? The earlier worker arms did not exercise a filesystem.

## Protocol and prediction

`cirrove-icloud-mounted-write-probe --account-mounted RUN_UUID` creates a new
private account state and a fresh Cirrove validation folder. A subtree mount
exposes only that folder. Its metadata retains the actual iCloud parent for the
normal writer's ancestry validation. The write account still describes the real
iCloud root, while Engine's filesystem view starts at the owned fixture folder.
The installed daemon and ordinary account settings are untouched.

A validation-only authorization wrapper delegates protocol behavior unchanged to
`ICloudWriteProvider`. It admits only the fixture root and confirmed in-tree
receipts in this isolated journal, binding scope and item type. Pending uploads,
foreign receipts, foreign destinations and mutation of the fixture root are
refused. Old confirmed identities remain available for handoff reconciliation.
The read view likewise exposes only those identities. This wrapper supplies no
alternative upload, replacement or namespace protocol.

A separate Python process creates known nonempty text through FUSE with O_EXCL,
writes, fsyncs, closes and reads it back. The parent waits for durable Uploaded
state and independently verifies the exact remote revision/digest. Another
process truncates and replaces that same file through FUSE, then fsyncs and reads
its new bytes. The parent awaits the real writer, independently verifies the new
ID/digest and finds the old exact ID in Trash. It shuts down/unmounts the session,
reopens all local components, remounts, and uses a fresh process to read the final
bytes. The second session is then shut down. Private state and fixtures remain.

Prediction: the regular writer will pass this create/replacement/remount path;
subtree root ancestry, namespace ownership publication or shutdown ordering may
expose integration failures absent from the direct-worker arms. No performance
improvement is predicted. Earlier replacement protocols took about eight minutes.
Allow 20 minutes externally; the per-transfer polling deadline is 15 minutes.
Normal worker reconciliation remains active; ambiguous handoffs cannot authorize
blind mutation replay. Failures stop the script, shut down the session when the
process remains alive, and preserve its journal for investigation.

Register binary SHA-256, command, PID, revision/dirty state, run UUID, expected
duration and disk-backed TMPDIR/SQLITE_TMPDIR before launch in a private manifest.
Assert the filesystem with findmnt. No parallel compilation or measurement.
Record status in `icloud-account-router-live-2026-09-30.json`. One pass supports
this bounded case only; repeat on fresh IDs before a repeatability claim.

Excluded: ordinary installed-daemon acceptance, mounted folder operations,
combined move+rename, empty/large files, other applications' atomic saves,
concurrent editors and process termination mid-handoff. These remain open gates.

## Results

Arm A `e1178e50-e40f-4f50-995f-d90ee4d7701d` exited 0 after **524.468 seconds**.
Its private manifest and log are under `.local-state/icloud-account-mounted-arm-e1178e50-e40f-4f50-995f-d90ee4d7701d/`.
The kernel mount was independently observed as `fuse.cirrove`. Create/fsync/read,
remote digest verification, truncate/replace/fsync/read, independent new-ID and
Trash verification, shutdown, remount and a fresh-process read all passed.
The normal account router executed both uploads with its normal context and
workers; the wrapper supplied only fixture authorization. Phase times were
5.1, 18.3, 127.4, 63.4, 2.5 and 125.8 seconds for old preflight, old request,
old postflight, install preflight, install request and install receipt.
This is one bounded mounted arm, with no within-arm spread yet. It does not close
the excluded gates or establish installed-daemon acceptance. Synthetic ownership tests pass, including reopen. Removing the
in-tree receipt filter makes the foreign-receipt exclusion assertion fail
(exit 101); the production guard is restored before building a live binary.

## Shared shutdown regression found by the repository gate

The full repository check after the combined-relocation work failed the existing
synthetic kernel test `real_unlinked_handles_keep_their_bytes_and_do_not_upload_later_writes`
at shutdown. The remote assertions and retained unlinked bytes had already
passed. A targeted repeat first reproduced the same failure at arm 11; a
temporary diagnostic reproduced it at arm 31 and identified projection
publication, not journal sealing, as the failed stage. These are observations of
a scheduling-dependent failure, not estimates of its rate.

`seal_all` copied the working-file list, released the journal lock, then called
`publish` for each copied file. Concurrent completion can validly retire a clean
working descriptor. `publish` demands a surviving working descriptor and returns
EIO in that case, even though shutdown only needs the current namespace view.
The correction keeps fsync/sealing of every dirty file under the journal lock,
then refreshes the current projection once. Actual sealing errors still return
EIO. Temporary diagnostics were removed. The existing kernel test exercises the
old-handle writes, independent replacement, absence of later old-stream uploads
and shutdown; repeat and full-gate results follow below.

After the correction, 100 sequential executions of that exact existing kernel
test passed. Each invocation ran one test rather than filtering all tests out.
The full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh`
rerun then completed with exit 0, including the existing failing kernel test,
workspace and feature tests, remaining kernel mounts, scripts, ledger and docs.
The pre-existing rustdoc link warning remains; display-dependent window scenarios
are outside this command. The first failed run remains recorded above.
