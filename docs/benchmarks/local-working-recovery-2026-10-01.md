# Offline working-byte recovery

Synthetic functional validation, 2026-10-01; no cloud mutations, credentials or
installed daemon changes. This closes the offline CLI working-byte export slice,
not active-account export, desktop recovery or full iCloud write readiness.

The actual CLI regression `offline_cli_exports_unsealed_bytes_without_sealing_or_replaying`
first failed because `export-working` did not exist. It now copies an unsealed
working version from a disabled synthetic iCloud account, with HOME/state defaults
removed, and verifies exact bytes and unchanged database bytes. Existing sealed
export tests remain unchanged and pass.

A second red test changed the mutable source during export: the initial helper
published it despite that change. Working-byte copy now captures size/mtime/ctime
and refuses unexpected changes before publication. This guard supplements the
exclusive journal lease, which prevents cooperating workers from changing it;
it is not protection against an adversarial same-user process that bypasses locks
and alters the file after the final observation.

Other tests cover actual/recorded size mismatch after simulated interrupted
write, stale generation, held owner lock during progress, private permissions,
SHA-256 receipt, cancellation, existing destinations, state/mount destinations,
symlinked source, enabled account refusal, clean-record pagination and an empty
unlinked working file. The read-only SQLite open never seals, migrates, retries or
resolves. A working receipt hashes actual recovered bytes, not a prior immutable
checksum; partial application writes remain partial and are not advertised as a
complete document. In-memory editor buffers are outside this recovery path.

An initial targeted parallel run of the unchanged `local_export` suite returned
transient `Busy` while opening its isolated journal; a serial diagnostic run
passed that suite, and the next normal parallel run passed all eight cases.
The initial new size assertion also used 21 instead of the fixture's actual 22
bytes; it was corrected to the literal's length. These failed results are retained
in this account of validation rather than treated as successful checks.

Targeted result: 8 existing sealed-export tests and 5 working-export tests passed.
Full project check is recorded below after completion. No performance measurement
or real crash/power-loss claim is made; interrupted metadata is synthetic here.

Complete `scripts/check.sh` passed, 2026-10-01 02:48:01–02:54:48 UTC,
including default parallel workspace/feature tests, synthetic kernel mounts,
formatting, clippy, scripts, translations, ledger and documentation. The existing
and new export suites both passed in that run. Manifest/log:
`.local-state/local-working-recovery-check-2026-10-01/`; private TMPDIR and
SQLITE_TMPDIR were on btrfs. No concurrent measurement ran. No GUI changed, so
window scenarios were not rerun. These experimental branch changes have not been
installed into the regular service.
