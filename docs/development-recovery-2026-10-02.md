# Cirrove development recovery

The interrupted work is consolidated in the main checkout on
`work/resume-icloud-20261002`, based on local iCloud head `c15f7ee`. The old
`aur/prepare-for-later` checkout was not the current development line. This
record identifies the active source, preserved work, installed build and next
validation steps as of 2 October 2026.

## Source and remote state

At recovery, local `main` was 127 commits behind `origin/main` and was
fast-forwarded to `d345767`. The recovered iCloud branch contained that mainline
and 232 additional commits, including 70 beyond its remote branch. Draft
[PR 86](https://github.com/Dandiccf/cirrove/pull/86) still points to `7e2d257`.
Its green CI result does not validate the newer local commits. The recovered
iCloud head is `bca480c`; later public changes described below have advanced
local `main` and `origin/main` to `f3e76ae` without changing that iCloud head.

The recovered checkout includes the uncommitted static replacement-admission
diagnostics, their regression tests, the allocated-space correction and its
documentation, and the latest Numbers validation record. The known Clippy
failure in the diagnostic test used `result.err().expect(...)`; recovery changes
it to `expect_err(...)`. The complete contributor check is registered in
[the recovery artifact](benchmarks/session-recovery-2026-10-02.json).

The AUR documentation changes and staged public-key removal are saved separately
in `refs/archive/session-recovery-20261002/aur-stash`. They have not been mixed
into the iCloud changes. The original iCloud index and working changes are saved
in `refs/archive/session-recovery-20261002/icloud-stash`.

## Previous agent work

There were 65 registered worktrees. Their HEADs are retained under
`refs/archive/session-recovery-20261002/`, with private staged and unstaged
patches, untracked source files and inventories in
`.local-state/session-recovery-20261002/`. Local build symlinks are excluded from
Git status. The recovered checkout uses its own Cargo target on Storage, with
TMPDIR and SQLITE_TMPDIR on ext4.

Three Cirrove worker transcripts contain delivered results: static admission
diagnostics, native temporary-stream recovery, and a bounded read-only provider
lookup helper. These contributions were already incorporated or exercised in
the recent iCloud work. The transient `/tmp` patch/helper files disappeared;
the source and evidence retained in Git and private state are the recovery base.

The older scratch worktrees contain integrated code or earlier variants. A
function-name comparison found their added functions in the consolidated tree,
except two earlier access-policy test names, whose assertions were superseded
by explicit-policy and invalid-identity tests. This comparison is an inventory
aid, not proof of behavioral equivalence. Earlier Google negative-control edits
remain isolated; they are not production fixes. The Strata integration has
evolved in the iCloud tree, while the paged-payload memory prototype remains a
separate unmerged experiment.

No previous Cirrove worker process was observed. Orca was not running, so it
provided no live worker registry. Two private manifests still said `running`
despite absent recorded PIDs; their status is now `interrupted`, with the original
manifests backed up. No successful result is inferred from an interrupted run.
Historical worktrees and benchmark data are retained in place. The protected-state
rules prohibit moving the nested evidence trees; `fuser -m` also reports the
filesystem containing the old `/var/tmp` worktrees as in use, so the required
cleanup guard does not authorize their relocation. No recursive deletion or
relocation was performed during recovery.

## Published release and installed system

[Canary 1](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.1)
was published on 2 October 2026 from `a9e2429`. All eleven downloaded assets
matched their checksums, and all nine package attestations were verified.
This prerelease contains OneDrive and Google Drive development work; it does
not contain the recovered iCloud branch. The regular release remains `v0.1.0`.
[PR 92](https://github.com/Dandiccf/cirrove/pull/92) merged the Canary
documentation at `d9f16a2`, with all seven CI jobs passing.

The subsequent requested installation replaced the developer build with the
published Arch core, desktop and Dolphin packages. Known home installation
files were backed up and removed through the supported switch workflow.
The daemon runs from `/usr/bin/cirroved` with the packaged user unit, and all
three packages passed their file-integrity checks. Installation verification
reported three restored mounts, OneDrive ready, two Google accounts still
requiring their pre-existing reauthentication, zero failed uploads and zero
stuck changes. Accounts, credentials and cloud files were preserved.

The earlier [allocated-space hotfix](benchmarks/allocated-space-2026-10-02.md)
was therefore superseded by the exact published packages. Canary 1 still has
the virtual `st_blocks`/`du` display regression; its correction is present in
the recovered development branch. No iCloud development daemon was installed.

The stale home icon cache was repaired and the user confirmed Files badges
worked. [PR 93](https://github.com/Dandiccf/cirrove/pull/93), merged at
`f3e76ae`, makes developer installation and package switching refresh that
cache even without a theme index, and uses the system Python path when
refreshing Files. Three real GTK regressions failed before the fix and passed
after it; the full contributor check and all seven CI jobs passed. The Canary
installation notes now point to the corrected switch script. Its tag and all
release assets are unchanged, and this script fix did not restart the daemon.

The publication, package installation and icon repair are recorded privately
under `.local-state/session-recovery-20261002/` and
`.local-state/package-install-canary-20261002/`. These later installation
actions are separate from the original recovery, which did not restart the
daemon or change installed accounts.

Cirrove's Strata adapter is retained. The upstream
[Strata draft PR 1385](https://github.com/lgse/strata/pull/1385) is open; its later
metadata-policy check passed. Its fresh implementation tests were interrupted
by the previously requested container cleanup, so that policy result is not
implementation validation.

## Resumed work and remaining gates

The recovery's complete `scripts/check.sh` command passed (exit 0), including
both new privacy/phase regressions, the allocated-space
kernel regression, synthetic workspace and feature-gated iCloud tests, actual
FUSE fixtures, script checks, Dolphin plugin tests and acceptance ledger. One
existing Rustdoc warning remains for the internal `Writeback::retry_stuck` link.
Desktop window scenarios were not run. Neither synthetic tests nor the existing
remote CI close a real-provider release gate.

The live Numbers replacement remains unconfirmed in
[the owned-fixture record](benchmarks/icloud-iwork-next-owned-fixtures-2026-10-02.json).
A read-only audit of its stopped local state confirms two completed imports, two
completed package publications, no incomplete queue rows and no replacement row.
The cached package, revision and parent match its receipt. Exact
provider-node lookup and local archive capture passed separately. The
[registered parent-admission regression](benchmarks/icloud-native-parent-admission-2026-10-02.json)
then reproduced selected-document refusal when an acknowledged parent folder
retained its local namespace identity. A scoped parent-identity correction
passed the native replacement tests, including changed-parent and dirty-ancestor
refusals. This fixes the matching defect synthetically; the original generic
diagnostic does not establish the historical failure phase, and no live Numbers
replacement was replayed. The complete `scripts/check.sh` for this correction passed, including the
three new regressions and actual kernel fixtures. Desktop window scenarios
were not run.

After the full check, the next step is a fresh registered isolated live
confirmation, which requires explicit task authorization for its cloud mutation.
Verify current selection and retained operations before submission, then exact
new content and the recoverable original before Apple reopen. Do not replay the
earlier request merely because it lacked confirmation, migrate installed state
or restart the installed daemon for this isolated validation.

Independent replacement-content verification, retained-original proof, Apple
Numbers reopen, DATA representation acceptance, Keynote fidelity and installed
iCloud transitions remain open. Normal iCloud connections still default to
read-only; the unfinished writable development build has not been installed.
Google account reauthentication and review of the separate AUR and dependency
PRs are additional outstanding work.
