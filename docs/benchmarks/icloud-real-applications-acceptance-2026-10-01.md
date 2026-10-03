# Actual application acceptance — isolated live evidence

Question: can ordinary writes in two independent newly owned iCloud subtrees survive
actual Neovim saves and Gio namespace operations, independent cloud digest checks,
recoverable deletion and remount?

This is a supporting account-router/FUSE fixture. It does **not** exercise installed
Settings opt-in, the ordinary desktop daemon, or GUI editor/file-manager clicks.
The historical A/B results and the newer combined admission arm are recorded
below. Keep installed-workflow release gates open.

## Registered arms and prediction

Run the same complete sequence twice, serially, using these distinct unused UUIDs:

- A: `d3d38625-7bb7-42f9-b14e-8fb008a6a2df`
- B: `75c4f011-89d6-454b-a550-d3fcb80ed19d`

Each run creates only `Cirrove Write Validation-<UUID>` at the validation account's
root. Every subsequent remote mutation is restricted by the existing journal-backed
`Owned`/`Guarded` adapter to identities descending from that new folder. A run's
launch manifest and run directory use create-new semantics; never reuse an uncertain
run, remove its evidence, or switch it to a user's existing folder.

Prediction: both arms complete with exact synthetic bytes and identities preserved.
Neovim `backupcopy=no` replaces the local inode and keeps an already-open old
file descriptor readable. The cloud backup identity may be coalesced by the journal;
`editor-lineage.json` records the actual upload/mutation receipts and any confirmed
backup IDs. Missing remote backup identity is an explicit unresolved lineage gate,
not evidence of atomic cloud publication.

## Exact execution path (after build and review)

The feature-gated binary is `cirrove-icloud-mounted-write-probe` with feature
`icloud-write-probe`. Build only when no exclusive measurement is running, with a
worktree-specific `CARGO_TARGET_DIR`. Its new option is:

```sh
cirrove_app_binary=/absolute/path/to/this-worktree-target/debug/cirrove-icloud-mounted-write-probe
"$cirrove_app_binary" --account-mounted-applications d3d38625-7bb7-42f9-b14e-8fb008a6a2df
"$cirrove_app_binary" --account-mounted-applications 75c4f011-89d6-454b-a550-d3fcb80ed19d
```

The source session remains the existing isolated `iCloudGuiValidation` fixture
under `.local-state/icloud-gui-connect-validation/state`; the normal daemon is not
stopped or reconfigured. A missing/expired isolated session fails without logging
credentials. Each arm records a launch manifest (PID, executable SHA256, command,
question and prediction) before creating its remote folder. Per-application manifests
record exact argv and executable SHA256 before spawning. Standard output/error from
applications are suppressed; only fixed synthetic content is supplied.

Actual child commands use argv, never a shell:

1. `/usr/bin/nvim --headless -u NONE -i NONE -n --cmd 'set noexrc nomodeline noloadplugins' -l <private/editor.lua> <mount/Account Router.txt> create`
2. Same command with final argument `replace`; Lua forces `writebackup`,
   `backupcopy=no`, backup directory `.`, fixed backup suffix, `fsync`, no swap,
   no persistent undo, no modelines and no exrc. It asserts changed inode, zero
   links on the held old descriptor, and exact original bytes through that descriptor.
3. `/usr/bin/gio mkdir <mount/Moved>`
4. `/usr/bin/gio rename <mount/Account Router.txt> Renamed.txt`
5. `/usr/bin/gio move --no-copy-fallback --no-target-directory <mount/Renamed.txt> <mount/Moved/Account Router.txt>`
6. `/usr/bin/gio save --create --private <mount/Account Router.txt>` with fixed
   synthetic bytes through stdin.
7. `/usr/bin/gio remove <mount/Account Router.txt>`.

Gio `remove` issues ordinary deletion through FUSE; Cirrove must place that exact
remote identity in **recoverable provider Trash**. This does not test Freedesktop
`gio trash` or its local `.Trash` implementation. No permanent-delete API is used.
Each child receives private XDG config/state/data/cache directories and a cleared
startup environment; `HOME` is not overridden. Application deadline is 120 seconds;
remote settlement deadline is 900 seconds per stage. Failures retain evidence and
do not retry/replay the application operation automatically.

## Endpoints and acceptance

Each arm must have:

- Actual Neovim process success for create and replacement, with the Lua inode and
  retained-descriptor assertions successful.
- Revision-bound independent SHA256 checks after original save, replacement,
  Gio relocation and Gio create; the verifier does not use cached mounted bytes.
- Complete bounded journal settlement, no unresolved failure/conflict/review;
  local superseded (`Resolved`) operations are distinguished from applied receipts.
- Exact original and deleted-file identity found in provider Trash.
- Exactly the expected owned child directory left at the fixture root, with no
  editor/Gio temporary files there.
- Isolated mount shutdown and a fresh mount reading the final synthetic bytes,
  followed by another independent revision-bound digest check.
- `application-journal.json`, `editor-lineage.json`, identity receipts,
  per-process manifests, and `passed.json` retained for both independent UUIDs.

Report both arm durations/results; one arm or a Python emulation is insufficient.
`passed.json` explicitly leaves `installed_settings_workflow`, `gui_clickthrough`,
`gio_trash_tested` and `atomic_cloud_publication_claimed` false. Review the editor
lineage rather than promoting local inode replacement to a cloud atomicity claim.

Guard tests are prepared under
`validation::icloud_account::mounted::applications::tests`; no tests or live commands
were run while preparing this fixture. Existing `Owned` guard tests remain the
provider-dispatch authorization proof. Proposed negative controls: accept `..` in
`owned_path`, or omit run/account/package checks in `fixture_guard`; their corresponding
synthetic guard assertions must fail. For the eventual controlled live arm, removing
an editor write must fail the independent digest endpoint.

## Preparation results

The first compile found ambiguous `.into()` inside a JSON array in the launch
manifest; the literal now remains a string. That failed compilation is retained
as `icloud-access-application-stage-red-2026-10-01` and is not negative proof.
The second negative control deliberately mapped both editor-create and gio-create
to `gio-create`; its single test failed with `duplicate launch manifest`. After
restoring unique names, all three application guard tests passed (2026-10-01,
07:19 UTC). Logs/manifests are under `.local-state/icloud-access-application-*`.
No live cloud result is implied by these synthetic guards.

The feature binary build and warning-denying mounted-probe Clippy check passed.
The first Clippy wrapper invocation failed before launching a child (runner
NameError); the corrected second invocation passed.

Local disk shakedown: all seven exact Neovim/Gio stages passed on btrfs, with
changed editor inode, old-descriptor assertions and Gio move identity preserved.
Evidence: `/var/tmp/cirrove-local-app-shakedown-c423f1b262d2431d85cceded73df203f/completed.json`.
This is local command validation, not FUSE/cloud acceptance.

Live arm A started at 07:21:48 UTC with binary SHA256
`758f1b69118468c0b485e5d2b791a915e930ab0872003c3522e1ff7016e880b7`.
Supervisor PID 786959, child PID 786970; private TMPDIR/SQLITE_TMPDIR
`/var/tmp/cirrove-access-7tu8oylc` on btrfs. Manifest:
`.local-state/icloud-access-application-live-a-2026-10-01/run.json`.
Result remains pending until terminal evidence is recorded.

## Live arm A result

Arm A passed, exit 0, 07:21:48.003–07:27:56.431 UTC (368.4 seconds
wall time including setup/shutdown). All seven actual application processes
succeeded. Independent revision-bound digests, exact remote identity after
Gio move and remount, original/disposable identities with restore metadata in
Trash, and no remaining temporary root files passed. The editor lineage includes
a confirmed remote backup identity; atomic cloud publication remains unclaimed.
The isolated mount shut down successfully.

Arm B uses the exact same binary SHA256, not a rebuild. Source changes for the
separate cancellation fix and package parser were prepared after this binary was
built; runner source hashes describe current files, not a claim those changes are
in this binary. The retained build manifest and binary hash identify the tested
implementation. A single passing arm is not the two-arm acceptance endpoint.

Arm B started at 07:28:14.080 UTC, supervisor PID 798096 / child PID 798121,
private btrfs TMPDIR and SQLITE_TMPDIR `/var/tmp/cirrove-access-mcms5aau`.
Manifest `.local-state/icloud-access-application-live-b-2026-10-01/run.json`;
terminal result is still pending. Do not start compilations or another live arm
while this recorded process is active.

## Live arm B result and two-arm endpoint

Arm B passed, exit 0, 07:28:14.080–07:34:53.322 UTC (399.2 seconds
wall time). Both arms met all application/digest/identity/Trash/remount endpoints
and both observed the remote editor-backup identity. Wall times were 368.4 and
399.2 seconds, a 30.8-second range; two runs establish bounded functional evidence,
not a broad latency or reliability estimate. Both isolated mounts shut down.

The supporting two-arm application gate is satisfied. GUI click-through, ordinary
Settings workflow, installed acceptance and native-package editing remain open.
The validator uses Gio backend operations; it does not claim Freedesktop `gio trash`
or atomic cloud publication.

## Combined admission arm C — preregistration

Run `599c285d-68cb-4913-992f-f924fe373dfc` with binary SHA-256 `922147fb11583f416d3d331a21393c5d9974024a697d90c74b64746f3fe91419`,
built from the reviewed successor to commit3301fb3 (current package-validator
additions included). Command: `cirrove-icloud-mounted-write-probe
--account-mounted-applications 599c285d-68cb-4913-992f-f924fe373dfc`.
Prediction: all seven actual application operations, independent digests,
Trash identities, relocation and remount pass with the forwarded revision-bound
write-target admission active. Same owned synthetic subtree restrictions apply.
No installed Settings, graphical clicks, Freedesktop Trash, native-package
write refusal or installed recovery claim follows from this arm. Runner records
private disk TMPDIR/SQLITE_TMPDIR, binary, PIDs and timings before execution.
No build or second measurement may overlap. Stage deadlines remain those above.

Arm C passed, exit0, 08:37:07.480–08:43:57.577 UTC (410.1 seconds
wall time). All seven actual Neovim/Gio child operations succeeded. Exact
independent content checks, recoverable provider Trash, retained-descriptor
replacement and remounted read passed with the forwarded admission hook.
`passed.json` retains false installed-settings, GUI-clickthrough, gio-trash and
cloud-atomicity claims. One combined run supports this bounded path, not
repeatability or full installed release acceptance. No compile overlapped.
