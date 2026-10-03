# Local recovery desktop validation — 2026-10-01

The desktop now selects an immutable retained generation, chooses a local output,
starts the existing service export and shows progress plus explicit completion.
No new provider mutation is introduced. Read-only accounts do not offer this
control; active writable journals are required. Version selection reads at most
200 recent local saves. Unknown, unsealed and acknowledged states are excluded.

## Evidence and limits

- Two pure presentation tests cover eligible states, legacy missing IDs, and exact
  completion matching (operation, size, destination, digest shape and job kind).
  Negative control: replacing the receipt size equality with an unconditional
  size acceptance caused the receipt test to fail; restored code passes.
- All 13 native GTK scenarios passed. The added socket fixture tests bounded
  history selection without dispatching cloud actions, and progress with a valid
  receipt, a missing receipt, a missing job and a stopped job. These are synthetic
  service responses, not proof that a real file was exported by the GUI.
- Actual filesystem/service/CLI export integrity is separately covered by
  [the preceding export validation](local-recovery-export-2026-10-01.md).
- The platform save chooser is used, but automatic tests enter the destination at
  the controller boundary; interactive portal/overwrite-dialog acceptance remains
  an installed-desktop gate. Service no-overwrite checks remain authoritative.
- [Versions screenshot](local-recovery-picker-2026-10-01.png) was captured by GTK
  from synthetic data and visually inspected. No personal account or content.
- Initial window-test compilation used incorrect harness member names, corrected
  before execution. First screenshot attempt used a path relative to the crate
  working directory and failed to save; the absolute output path passed. Neither
  failure was a product error. GTK's existing accessibility-bus warning remained.
- German catalogue contains all 256 messages translated.

The regular daemon was not restarted or replaced. Unsealed working bytes,
unmounted accounts, reopening export progress after an app restart and
per-operation cloud conflict resolution remain separate work.

Full `scripts/check.sh` passed, 2026-09-30 23:40:48–23:46:05 UTC,
including formatting, clippy, workspace, feature, kernel, script, translation,
ledger and documentation checks. Private manifest/log:
`.local-state/local-recovery-desktop-check-2026-10-01/`. The run used the
worktree's separate Cargo target and private disk-backed btrfs `/var/tmp` paths
for both TMPDIR and SQLITE_TMPDIR. Native windows were tested separately as
reported above. This is checkout validation; installed acceptance remains open.
