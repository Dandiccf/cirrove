# iCloud reauthentication durability prerequisite

Ordinary reauthentication must preserve the existing access mode and report an
error if durable settings completion fails. A validated Apple session may not be
saved into an account that changed during validation. This change does not enable
ordinary iCloud writes or change read-only connection defaults.

## Evidence

Negative control 1 restored swallowed persistence failure: the new test failed at
its requirement that unsuccessful settings persistence cannot report successful
sign-in (exit 101). Negative control 2 omitted the full-account equality check:
the stale-account test failed (exit 101). Controls were restored before running
the account suite. Logs:

- `.local-state/icloud-reauth-persistence-red.log`
- `.local-state/icloud-reauth-stale-red.log`
- `.local-state/icloud-reauth-green.log`

Rollback remains best effort if the underlying storage continues failing. The
operation reports failure and leaves the restoration marker when rollback also
fails; this is not a guarantee that a failing disk can persist rollback.

All 21 account tests passed after restoring both fixes. 

Full `scripts/check.sh` passed on 2026-10-01, 06:01:31–06:10:35 UTC
(exit 0), with the storage and reauthentication changes together. Evidence:
`.local-state/icloud-storage-reauth-check-2026-10-01/`. No installed daemon
or account access grant was changed. Rustdoc reports an existing broken private
Writeback link; the required check completed successfully.
