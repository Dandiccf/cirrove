# Local bootstrap for the preregistered public native import

This feature-only helper prepares the single run
`ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`; it does not start a daemon, contact Apple,
copy an archive, verify session freshness or perform an import. It has not yet
been executed. Synthetic tests are prepared, not reported as passing.

Invoke the newly built `cirrove-icloud-mounted-write-probe` with:

```text
--public-native-bootstrap ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc
```

The fixed destination is `/var/tmp/cirrove-public-native-ec7f82e1` with private
`state`, `mount` and `staging` children and `control.sock`. An existing destination
(including a failed preparation) is refused. No cleanup or retry occurs. Preserve
all artifacts and any fresh keyring entry after a failure; inspect before deciding
on a separately preregistered attempt.

The source is the current worktree's isolated `iCloudGuiValidation` account from
`.local-state/icloud-gui-connect-validation/state`. It must be enabled, read-only,
and iCloud, with username/subject and remaining identity fields matching the
retained ac9 owned-source account. This check occurs before session load/save.
Its snapshot is loaded in memory and re-sealed under fresh account and
credential UUIDs. No credentials are printed, serialized in settings, or supplied
as arguments. The helper only loads the source vault; saving occurs exclusively
in the fresh vault. A running source daemon can refresh concurrently, so a coherent
source load can still fail; failure is retained rather than retried automatically.

The fresh account is enabled/read-write, uses label `iCloudPublicNativeValidation`,
a 64 MiB cache, and 60-second polling. iCloud account validation requires the real
CloudDocs root: this is **not** a folder-confined mount. The experiment must direct
its sole import to `Cirrove Package Validation ac9e5456-bd10-4b7d-9215-21bbb85dde69`
with name `Cirrove Public Import ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc.pages`.

`bootstrap-started.json` records run, helper PID, binary SHA-256, timestamp and
non-secret paths and the verified btrfs filesystem. The helper runs `findmnt`
on its claimed directory and refuses RAM-backed, unknown or failed detection
before keyring access; it uses the same strict btrfs contract as the retained
owned-package experiment. `bootstrap-ready.json` exists only after fresh session storage
and atomic, non-overwriting settings publication followed by `Settings::load`
validation. Require the ready receipt before starting a daemon. The prepared
settings inode is retained under `accounts.prepared.json` as well as `accounts.json`.
These contain ordinary private account metadata, not session material.

The parent live runner remains responsible for checking btrfs/non-tmpfs storage,
recording its own full command, binaries, exact child PIDs and bounded measurement
manifest, privately staging/rehashing the archive outside state/mounts, starting
and gracefully stopping only the isolated daemon, and checking remote parent/name
before a single public CLI submission. This helper is not a measurement result.

## Focused synthetic tests and negative controls

Filter: `validation::icloud_account::public_bootstrap::tests` (eight tests).
No test accesses a keyring or cloud service. To demonstrate sensitivity, remove
fresh ID assignment and expect `bootstrap_clone_has_independent_ids_and_preserves_source`
to fail; replace `hard_link` publication with overwriting `rename` and expect
`bootstrap_atomic_publication_refuses_existing_settings` to fail. Restore each
change before the green run. These controls have not been executed here.

## Synthetic validation results

Seven focused tests passed at 10:28:26 UTC. Removing fresh account/credential ID
assignment failed the isolation assertion at 10:29:21. Replacing no-overwrite
hard-link publication with overwriting rename failed the existing-settings guard
at 10:29:58. Both changes were restored; all seven tests passed again at
10:30:29. These tests performed no keyring or cloud access.
