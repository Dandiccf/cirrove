# Preserve iCloud app-owned container protection

Question: does provider metadata retain APP_CONTAINER / APP_LIBRARY protection
through the normal listing projection, and can a mutation preflight mistake those
raw types for ordinary folders? Prediction: the old broad folder predicate permits
both mistakes; preserving explicit metadata and requiring raw FOLDER at mutation
boundaries must refuse them while read traversal remains available.

## Scope

The protected-folder flag represents a conservative app-container policy, not a
claim that every child is a native document bundle. Both app-owned types remain
browsable folders. Only FILE-identified confirmed package nodes use generated
archive children, so this protection does not redirect app-folder listings into
archive downloads. Ordinary FOLDER stays ordinary, including nested directories.
Shared filesystem/router ancestor guards already consume the protected flag.

The listing regression uses the same identity/name with each raw kind. HTTP
fixtures accept only read listing endpoints through a nested folder to an ordinary
file. Independent upload-parent and current/legacy replacement handoff fixtures
exercise raw-type rejection. No Apple account or installed service is changed.

## Negative control

Removing the new projection flag and restoring broad mutation-folder predicates
makes the `app_` regressions fail (exit 101). The mapping assertion sees an
unprotected APP_CONTAINER; the legacy handoff continues to an extra listing
instead of refusing its app-owned parent. Evidence remains in
`.local-state/icloud-app-container-red.log`. With protection restored all 95
cirrove-icloud tests passed (`.local-state/icloud-app-container-green.log`).

The existing synthetic FUSE package test separately checks ancestor protection
for mkdir, file create/overwrite/rename/delete and package removal while child
listing stays readable. It is a shared-layer test, not a single end-to-end raw
Apple metadata-to-FUSE bridge or proof of live native document behavior.

Unknown FILE package types, native document editing, fresh metadata races beyond
observed preconditions, installed acceptance and normal writable grants remain
open. No extension heuristic or app-container name alone authorizes writes.

## Direct mutation adapters

Folder create parent/receipt, folder rename source/receipt, folder move
source/destination and its second observation, and file-move destination now
require raw FOLDER. Folder Trash already required this and was left unchanged.
Six new HTTP refusal/reconciliation tests and an ordinary-folder positive control
exercise these boundaries. Restoring broad folder predicates makes the new
rejection tests fail (exit 101), recorded in
`.local-state/icloud-folder-guard-red.log`; guarded `app_` tests passed in
`.local-state/icloud-folder-guard-green.log`. A current precondition observation
cannot make an undocumented remote API transactional; no atomic cross-client
namespace guarantee is claimed.

The existing FUSE ancestry-protection scenario passed with one test executed:
`.local-state/icloud-app-container-existing-fuse.log`. This confirms the shared
flag consumer independently of the raw metadata HTTP tests.

## Complete validation

Full `scripts/check.sh` passed (exit 0), 2026-10-01 05:05:14–05:12:00 UTC:
formatting, clippy, workspace/script tests, real FUSE scenarios and ledger checks.
Manifest/log: `.local-state/icloud-container-check-2026-10-01/`; TMPDIR and
SQLITE_TMPDIR were `/var/tmp/cirrove-container-check-zvuppbup` on btrfs.
The existing rustdoc link warning remains. No UI behavior changed, so native
window scenarios were not rerun. A separate read-only code review found no
material issue with folder traversal or ordinary-file behavior. The ordinary
installation and account grants remain unchanged.
