# iCloud handoff across a process boundary — 2026-09-27

Registered before the live run. This is an isolated crash-recovery experiment,
not a general writable-mount gate. Only a new Cirrove-owned folder and two
files are eligible. The normal iCloud service remains read-only; no existing
user file or other client state is touched.

Question: Can a second process recover a handoff after the first has moved
the old item to a unique recovery name, using a private, fsynced checkpoint
that binds account, collection, exact item IDs, names and full content hashes?
The second process must not infer identity from a path or replay a request
whose outcome is uncertain.

Arm A: create and independently verify the two items; write the checkpoint
with phase `prepared`; fsync `old_rename_pending` before the first rename;
verify both exact IDs and bytes under recovery/staging names; fsync phase
`old_at_recovery`; exit intentionally. Arm B: a new process loads the private
checkpoint and the same Cirrove-owned sealed session, inspects both exact IDs
and full hashes, fsyncs `new_rename_pending`, moves the staged item to the
vacated original name, verifies both IDs/hashes again and fsyncs `complete`.
No comparative arm. Any inconsistent or pending-unconfirmed state must stop
without replay.

Prediction: the second process will observe `old_at_recovery` and complete
the handoff, retaining both versions. This only validates the process boundary
at that one phase. It does not prove recovery from a lost response, a power
failure during fsync, an upload interrupted before its ID was checkpointed,
concurrent remote edits, or atomic namespace replacement.

The private record is `.local-state/icloud-durable-handoff-validation/record.json`.
Separate ignored start/resume manifests register command, binary hash, PID,
expected duration and disk-backed `TMPDIR`/`SQLITE_TMPDIR` before each run.
No tokens, signed URLs, raw provider bodies or file contents are recorded.

## Observed first run

The first process completed in 187.2 seconds and saved a private `0600`
checkpoint with phase `old_at_recovery`; it then exited intentionally. The
second process completed in 45.2 seconds using the same binary hash and
account-bound sealed session. It read the checkpoint, re-listed the exact
folder and both item IDs, checked their full content hashes, moved the staged
item to the vacated original name, verified both IDs and hashes again, and
synced phase `complete`. No item was deleted, and no temporary checkpoint
file remained. These are one run per arm without a within-arm spread.

The process boundary is proven only **after** a verified first rename. The
trial does not yet cover a response lost between Apple's commit and the
checkpoint update, nor an interrupted upload before the staged ID is known.
It does not make the rename pair atomic or conditional on a remote revision.
