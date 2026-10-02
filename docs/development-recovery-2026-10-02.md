# Cirrove development recovery

The interrupted work is consolidated in the main checkout on
`work/resume-icloud-20261002`, based on local iCloud head `c15f7ee`. The old
`aur/prepare-for-later` checkout was not the current development line. This
record identifies the active source, preserved work, installed build and next
validation steps as of 2 October 2026.

## Source and remote state

The local `main` branch was 127 commits behind `origin/main`; it has been
fast-forwarded to `d345767`. The local iCloud branch contains that mainline and
232 additional commits, including 70 commits beyond its remote branch. Draft
[PR 86](https://github.com/Dandiccf/cirrove/pull/86) still points to `7e2d257`.
Its green CI result does not validate the newer local commits.

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

## Installed system

The installed daemon is the previously validated allocated-space hotfix from
`fafa909`, with SHA-256
`cae39ed7013ea2f001afebb4ec819b72fe3aaa6092512873527996d2d4257321`.
[Its installation record](benchmarks/allocated-space-2026-10-02.md) is included
here. It is not the newer iCloud development build.

The local status socket reported three mounted accounts: OneDrive ready and
two Google accounts requiring sign-in. All reported zero failed uploads and
zero stuck changes. The Google authentication state predates recovery. No
package installation was detected alongside the developer install. Recovery
has not restarted the daemon or changed accounts, credentials or cloud files.

Cirrove's Strata adapter is retained. The upstream
[Strata draft PR 1385](https://github.com/lgse/strata/pull/1385) is open; its later
metadata-policy check passed. Its fresh implementation tests were interrupted
by the previously requested container cleanup, so that policy result is not
implementation validation.

## Resumed work and remaining gates

The recovered admission diagnostics now pass the complete `scripts/check.sh`
command (exit 0), including both new privacy/phase regressions, the allocated-space
kernel regression, synthetic workspace and feature-gated iCloud tests, actual
FUSE fixtures, script checks, Dolphin plugin tests and acceptance ledger. One
existing Rustdoc warning remains for the internal `Writeback::retry_stuck` link.
Desktop window scenarios were not run. Neither synthetic tests nor the existing
remote CI close a real-provider release gate.

The next iCloud issue is the unconfirmed Numbers replacement admission in
[the owned-fixture record](benchmarks/icloud-iwork-next-owned-fixtures-2026-10-02.json).
A read-only audit of its stopped local state confirms two completed imports, two
completed package publications, no incomplete queue rows and no replacement row.
The cached package, revision and parent match its receipt. Exact
provider-node lookup and local archive capture passed separately; they do not
establish mounted selection or admission. Inspect the retained journal and
identify the failing admission phase before any new registered submission.
Do not replay the earlier cloud mutation merely because it lacked confirmation.

Independent replacement-content verification, retained-original proof, Apple
Numbers reopen, DATA representation acceptance, Keynote fidelity and installed
iCloud transitions remain open. Normal iCloud connections still default to
read-only; the unfinished writable development build has not been installed.
Google account reauthentication and review of the separate AUR and dependency
PRs are additional outstanding work.
