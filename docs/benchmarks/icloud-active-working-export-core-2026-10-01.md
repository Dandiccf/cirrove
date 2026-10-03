# Active working-file export: coherent local snapshot core

Question: can unsealed working bytes be recovered while their journal remains
active without holding its mutex across file copying or publishing mixed versions?
Prediction: select under the owner lock, copy to private unpublished storage,
validate the exact generation under the same journal lock, then publish the
independent copy. Any normal byte mutation before validation must reject it.

## Contract

`working_export_source` selects an eligible dirty/unlinked UUID and generation,
opens a private nofollow descriptor, binds account/scope/journal identity and
retains a cloned journal-owner descriptor. `prepare_copy` copies and hashes bounded
chunks outside the mutex into an unpublished temporary file. Source length and
timestamps must remain stable during copying. `verify_working_export` performs
only journal reads under the short lock and consumes the prepared object. Only its
verified result can publish. The no-clobber publication and directory sync happen
outside the journal lock. The owner lease remains held through publication.

Writes and truncates persist a new generation before changing working bytes.
Consequently an identical-byte rewrite between copy completion and validation is
still refused. A later edit after successful validation may coexist with the
independent copy; the receipt identifies the selected older generation, never
claims to be the current latest version. No export operation seals a save, queues
an upload, calls a provider or changes the journal.

Existing sealed/offline export uses the same extracted prepare/publish steps and
retains component-wise nofollow destination traversal, FUSE refusal, private-state
refusal, anchored parent identity checks, cancellation and no overwrite.

## Tests and negative control

Nine deterministic active-core tests cover clean publication with unchanged
journal records, writes before/during/after copying, identical-byte rewrite and
truncate between copy and validation, edits after successful validation, retained
unlinked bytes, cross-journal refusal, owner lease retention, cancellation,
existing destination and parent replacement. Existing offline working and sealed
export tests also passed. Evidence:
`.local-state/icloud-active-working-core-green.log`.

Removing only the final generation comparison makes
`active_working_export_refuses_edits_between_staging_and_verification` fail
(exit 101). Restoring the check preserves the intended refusal. Negative log:
`.local-state/icloud-active-working-core-red.log`.
An independent read-only review found no blocking defect in generation ownership,
lock lifetime or destination protection.

This is the internal recovery core. Active working-file list/socket/CLI/desktop
wiring is still separate work. It is not a live iCloud reliability result and
does not enable writable accounts or alter the installed service.

The first complete check stopped at clippy: the two planned Writeback wrappers
had no production callers yet. They were deferred to the daemon integration
instead of suppressing dead-code warnings. The failed manifest/log remain in
`.local-state/icloud-active-working-core-check-2026-10-01/`.

## Complete check

The corrected complete `scripts/check.sh` passed, exit 0, 2026-10-01
05:15:44–05:22:54 UTC. Formatting, clippy, workspace/script tests, real FUSE
scenarios and ledger checks passed. Manifest/log remain in
`.local-state/icloud-active-working-core-check-2-2026-10-01/`; TMPDIR and
SQLITE_TMPDIR used `/var/tmp/cirrove-active-working-core-check-qaq5w79n` on btrfs.
The existing rustdoc link warning remains. No desktop behavior changed, so native
window scenarios were not rerun. The installed service was not changed.
