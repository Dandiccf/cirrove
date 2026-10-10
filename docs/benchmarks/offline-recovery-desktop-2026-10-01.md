# Offline recovery in the native desktop

Synthetic local validation, 2026-10-01. Configured disabled/unmounted accounts
now offer a recovery picker without a daemon. It loads bounded pages of retained
sealed versions and dirty/unlinked working files on blocking workers. The account
UUID is checked independently of its label at selection and export. The copy
reacquires the account/journal leases; it cannot start a provider or replay work.
Existing active-account sealed export continues through the daemon.

The picker distinguishes complete saved versions from working bytes which may
contain an interrupted application write. Export has local progress, cancellation
and source-specific receipt checks; working-file completion does not claim a
prior sealed checksum. No writes are enabled by exposing recovery.

## Evidence

- Backend tests use a real private journal and export both source kinds, reject
  an account-ID mismatch and cancellation, and verify paging through 201 saves
  without repetition or omission.
- Native GTK test uses a valid synthetic read-only iCloud account, disabled and
  with no daemon socket. It clicks the recovery action, selects the real retained
  working file, exports through the controller and verifies destination bytes and
  unchanged database bytes. A stale generation produces an unconfirmed result
  and no destination. The platform chooser is not automated: tests supply the
  destination at the controller boundary.
- Negative control restoring the former active-only button conditions failed
  with `disabled accounts must offer local recovery`; restored code passes.
  Earlier test attempts queried the button before GTK mapped it. Waiting for
  mapping corrected the test, not the product.
- Visual inspection exposed an unnecessary Next page for a short complete
  working-file page. A regression first failed, then passed after a bounded
  one-ID lookahead was added. Only 200 working records are inspected per page;
  the lookahead ID is not consumed. Service coverage checks 201 working files.
- All 14 native scenarios passed twice. Final run: 03:07:33–03:07:47 UTC,
  `.local-state/offline-recovery-window-validated-2026-10-01/`. Earlier retained
  fixture attempts used a nonexistent `drive_name` field, then incomplete iCloud
  account metadata; the compiler/settings validator refused those fixtures.
  No production validation was relaxed. Existing GTK accessibility-bus warnings
  remain unrelated to this result.
- [Picker screenshot](offline-recovery-picker-2026-10-01.png) was rendered by GTK
  from synthetic data and visually inspected. German catalogue has all 265
  messages translated. `cirrove-core` becomes a direct desktop dependency for
  its existing cancellation token; no external package/version was added.

Temporary storage for native suites was private disk-backed btrfs. No regular
service, mount, installed package or file-manager setting was changed. Interactive
portal acceptance, installed delivery, removed-account recovery and active-mount
working-byte export remain open. Full iCloud readiness is not claimed.

Complete `scripts/check.sh` passed, 03:09:40–03:15:44 UTC, including formatting,
clippy, workspace/feature/kernel tests, scripts, translations, ledger and docs.
Manifest/log: `.local-state/offline-recovery-desktop-check-repeat-2026-10-01/`;
TMPDIR and SQLITE_TMPDIR were private btrfs directories. The first full run stopped
on a needless borrow in the new screenshot test; that test-only lint was fixed
before the complete rerun. The pre-existing service rustdoc link warning remains.
No ordinary service installation was changed. Native-window evidence above is
separate from this full check and includes a real local-file export.
