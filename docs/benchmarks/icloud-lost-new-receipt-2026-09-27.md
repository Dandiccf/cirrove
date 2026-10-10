# iCloud new-rename response-loss experiment — 2026-09-27

Registered before the live run. The old-rename response-loss trial recovered
successfully. This trial tests the other request boundary. Both remote files
are new Cirrove-owned fixtures in a fresh isolated folder, and the normal
service remains read-only.

Question: Can a separate process verify that the staged file reached the
original name after its rename receipt was discarded, without replaying the
request or losing the recovery copy?

Arm A: create and hash-check both items, fsync an account-bound plan and
pending old-rename phase, move the old item to a unique recovery name, check
both IDs/hashes, fsync `old_at_recovery`, fsync `new_rename_pending`, send the
staged rename once and discard its response. Exit with the phase still pending.
Arm B: start a new process with the same binary and account. It must re-list
both exact IDs and read their full hashes. If and only if the old file remains
at recovery and the staged ID is at the original name, fsync `complete`.
It must not resend the pending rename. No comparative arm.

Prediction: the second process will observe the completed handoff and close
the pending checkpoint, leaving both versions. If either ID, name or hash
differs, it must stop without replay. This does not test a timed-out request
still in flight, an upload interrupted before its ID was recorded, a power
loss during fsync, concurrent remote edits or another account class.

The private record and per-process manifests live under
`.local-state/icloud-lost-new-receipt-validation/`. Each manifest records
command, binary hash, PID, expected duration and disk-backed private temp
filesystem before its process starts. No tokens, signed URLs, raw responses
or file contents are recorded.

## Observed first run

The first process exited normally after 154.1 seconds, leaving a private
`0600` record at `new_rename_pending`. A fresh process with the same binary
hash and account session completed in 33.3 seconds. It re-listed both exact
IDs, checked both full content hashes, found the staged ID at the original
name and the old ID at its recovery name, and fsynced `complete` without
resending the rename. Both versions remained and no temporary checkpoint
file remained. These are one run per arm without a within-arm spread. The
response was deliberately discarded *after* Apple replied; a request still
in flight remains untested.
