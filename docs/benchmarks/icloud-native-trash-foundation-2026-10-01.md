# Explicit native-document Trash: journal foundation

This is a prerequisite for the native iCloud Trash product path, not an enabled
user capability or a release result. No FUSE package guard is lifted. No live
mutation or validation was performed for this patch.

## Contract

`MutationIntent::TrashNativeDocument { before }` identifies the original native
container by account, collection, item, original parent/name and nonempty ETag.
The core requires a folder-shaped package without a shortcut target. This is a
single native document action, not recursive directory removal, and cannot target
the generated archive child. Only an exact-item `Removed` receipt acknowledges it.
The eventual native adapter must prove recoverable Trash; this foundation alone
does not authorize dispatch. Ordinary OneDrive/Google adapters and the ordinary
iCloud folder planner refuse it before remote requests. No filename extension
selects this action.

The existing mutation JSON body persists a new `trash_native_document` tag.
Existing request bodies and ordinary upload representations remain unchanged.
The queue reserves the exact item and original name slot through existing mutation
resource binding. The intent cannot chain from an earlier generation and is
terminal for later generations. Transaction entry validates native requests even
for internal callers and rejects bases, working files, namespace objects and
prerequisite barriers before any SQL mutation. Standalone native removal does not enter the
namespace-based ordinary stuck-removal discard path; product admission and its
specific recovery controls must be reviewed together before exposing the command.

## Deployment and rollback

Journal schema becomes 16. Every writable journal open upgrades to 16, including
ordinary-only journals with no native Trash row. Binaries capped at schema 15 will
therefore refuse **all** upgraded journals, not only iCloud/package accounts.
Do not roll back by changing `user_version` or deleting unfamiliar records.
Deployment must preserve journals and checkpoints and account for this one-way
format fence; recovery/export uses a compatible newer binary. Existing schema
14/15 read-only recovery stays non-migrating; schema16 read-only recovery can
export retained local uploads without changing the journal.

The tests compare the stored version against the previous source-level maximum;
they do not execute an older installed binary and are not an installed downgrade
acceptance result. No installed daemon, Strata integration, or account is changed.

## Prepared tests and negative controls

Tests are prepared, not run by the author of this patch:

- `native_trash_intent_requires_original_container_and_exact_removed_receipt`:
  reject ordinary files/folders, missing preconditions and mismatched receipts.
  Removing the package requirement must fail the ordinary-folder arm.
- `native_trash_schema16_preserves_pending_and_uncertain_intents_and_local_recovery`:
  upgrade an ordinary-only schema15 journal, round-trip pending/uncertain native
  requests through close/reopen, export retained ordinary bytes read-only and
  compare the database bytes. Removing the schema increment must fail its version
  assertion; replaying an interrupted attempt as Pending must fail state checking.
- `native_trash_is_terminal_and_cannot_chain_from_existing_generation`:
  no successor from/to a native removal and no cross-item receipt acceptance.
- `native_trash_refused_before_any_onedrive_request` and
  `native_trash_refused_before_any_google_request`: preparation, mutation and
  reconciliation return typed Unsupported with the synthetic HTTP server already
  closed. Removing refusal must not pass through a transport failure as success.
- `native_trash_internal_admission_refuses_invalid_shape_and_attachments_without_db_changes`:
  direct internal calls reject invalid package/precondition shapes and every
  attachment; SQLite total_changes plus object/working snapshots stay unchanged.
  Removing the transaction-entry native guard must fail the base/prerequisite
  admission arms (not just a later provider refusal).
- `ordinary_folder_plan_refuses_explicit_native_trash`: the native intent cannot
  fall into the ordinary folder plan.

Next integration must supply the purpose-isolated sealed native adapter,
read/write account and exact active writer admission, durable queue/job identity,
post-receipt metadata publication, and lifecycle/failure acceptance before any
user-facing entry point. This patch claims none of those gates complete.

## Parent validation

Four focused service tests passed (`native-trash-foundation-tests`), including
schema16 pending/uncertain persistence, retained-byte recovery and internal
admission. Removing the native transaction-entry guard caused the direct bypass
test to fail at its first arm (`native-trash-admission-negative`); the exact
guard was restored. Core intent plus OneDrive/Google early-refusal tests passed
with the Google test-support feature enabled (three tests total). An initial
standalone multi-package invocation without that feature did not compile an
existing Google native-export test; it is not counted as a passing check.

The native adapter was integrated afterward. Its initial ten-test run passed
two tests and refused eight during preparation: test tempdirs used default
permissions rather than the required private staging mode. Production staging
already creates directories with 0700. Fixture permissions are being corrected
without weakening the production guard; adapter acceptance remains open.

## Adapter validation correction

After explicit 0700 fixture directories, eight tests passed and three exposed
a real staging-file issue: tempfile's anonymous file followed umask and had
0644 rather than the required 0600. The new staging test failed its permission
assertion; recovery correctly refused the file. Production staging now sets
0600 before returning the empty file or writing cloud content. All eleven
adapter/storage tests then passed (`native-trash-private-file-fixed`).

A removed MayHaveSent dispatch guard initially did not fail the active-source
replay test: that test only retried preparation. It was strengthened to attempt
direct dispatch with the retained prepared ID. With the guard still removed,
the synthetic server observed a forbidden second request and the test failed
(`native-trash-replay-negative-strengthened`). The exact production guard was
restored and all eleven tests passed (`native-trash-replay-restored`).

Removing the independent pre-dispatch semantic comparison also failed the
changed-content test (`native-trash-semantic-negative`); production code was
restored unchanged. Workspace Clippy passed after correcting a test assignment
formatting lint. A broader feature-enabled all-targets Clippy invocation exposed
existing probe-test lint failures outside the normal check.sh matrix; it is not
reported as passing. The required complete script remains pending.
