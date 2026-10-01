# Explicit package archive journal contract (prepared, not yet validated)

This code slice separates uploaded archive bytes from native package content and
remote logical metadata. It enables no normal mounted package writes. Existing
package ancestry/write guards and native artifact presentation stay unchanged.

`UploadIntent` continues to carry create identity or replacement preconditions.
The additive `UploadRepresentation` qualifier defaults to `FileBytes` for missing
legacy JSON fields and remains omitted when serializing ordinary records/checkpoints.
`PackageArchive` requires an explicit safe enclosing root and
a validated version-1 `PackageSemanticIdentity`. The core owns only the data type,
validation and version constant; iCloud's bounded ZIP identity generator remains
behind `write-probe`. No archive type is inferred from a filename or extension.

Request/record `size` and `sha256` are the exact sealed archive bytes. A package
completion separately binds independently verified semantic content to a remote
`Folder` with `package = true`, exact requested parent/name and content revision.
Remote logical size is retained honestly; it need not equal the archive size.
The package adapter must independently read back the exact allocated identity
and recompute content identity before it returns this receipt. This patch adds
no such production adapter and does not treat a registration response as proof.

The journal's package enqueue API is available only for tests or explicitly
compiled validators. It accepts Create with no working-file/namespace/base
binding. Its supplied identity is not an admission proof: a future production
caller must verify the exact sealed archive and semantic identity before enqueue.
The begin/allocation interfaces have no payload descriptor, so the adapter
cannot validate bytes before allocation through this interface. It must still
independently validate the sealed archive before sending the body.
Acknowledgement and receipt persistence are fenced by the current
attempt token. Ordinary acknowledgement, handoff, rescue and successors refuse
package-qualified rows. Native receipts do not publish an ordinary namespace
binding. Ordinary OneDrive, Google Drive and iCloud adapters refuse the qualifier
before remote operations, including the competing-edit validation wrapper.

SQLite user_version advances from 14 to 15 even though the new fields reside in
JSON. This matters: old binaries ignore unknown JSON fields, but refuse schema
15, preventing silent package-to-file reinterpretation. New writable opens
migrate old journals without rewriting old rows. Read-only recovery accepts 14
and 15 without migration; future versions are refused. Unknown representation
variants and identity versions are errors. Old ordinary checkpoints with a
missing qualifier continue to deserialize as FileBytes.

### Deployment and rollback consequence

Every writable journal open migrates to schema 15, including ordinary-only
OneDrive, Google Drive and iCloud journals with no package-qualified records.
Therefore an older installed binary whose opening gate rejects versions above
14 cannot reopen **any journal touched by the new writable service**, not only
package-test accounts. This is a deliberate downgrade barrier; it must be
considered before installation or switching developer binaries. Keep validation
journals isolated. Rollback requires a binary supporting schema 15 or an explicit,
reviewed migration/restoration procedure; do not manually lower user_version,
erase new fields, or replace a journal that retains unsent changes. The read-only
recovery path in this version supports both 14 and 15 without migrating either.

The prepared downgrade assertion checks the previous `version > 14` rule at
source level. No old executable was run against a migrated journal, and this
artifact does not claim an executed old-binary rollback test.


## Prepared validation (not executed by implementing agent)

Eight journal tests under `journal::representation::tests` cover legacy JSON,
schema/read-only compatibility, pending and uncertain package exact-byte recovery,
raw-versus-logical size, restart, malformed and
cross-kind receipts, attempt fencing, rescue/handoff/successor refusal, invalid
root/identity/Replace enqueue, and worker propagation through both streaming and
uncertain-result reconciliation. Both worker arms retain exact sealed bytes and
only one stream. Five new adapter tests refuse package requests; existing create
and mounted-replacement tests also exercise the refusal. The five semantic
identity tests are documented in the companion framing artifact.

Parent runs serialized validation with a private non-tmpfs TMPDIR and separate
Cargo target directory. Suggested focused commands (from the worktree):

```
cargo test -p cirrove-service --lib journal::representation::tests --locked
cargo test -p cirrove-service --lib package_archive_cannot_enter --locked
cargo test -p cirrove-onedrive --lib package_archive_is_refused --locked
cargo test -p cirrove-googledrive --lib package_archive_is_refused --locked
cargo test -p cirrove-icloud --features write-probe package_identity_ --locked
cargo test -p cirrove-icloud --features write-probe package_archive_cannot_enter --locked
```

Meaningful negative controls, applied individually and restored before green:

- Replace the worker's persisted representation propagation with FileBytes. The
  stream/reconciliation test must fail before acknowledging any upload.
- Remove semantic equality from `acknowledge_package`. The changed digest arm of
  the mismatch test must fail because the invalid receipt is accepted.
- Remove package refusal from ordinary acknowledgement. Its package row with an
  otherwise valid File receipt must be incorrectly accepted and fail the test.
- Remove the early package predecessor guard in `validate_mutation_base`. The
  successor refusal test must fail on an ordinary-looking RemoveFile successor.
- Remove the schema-15 migration. The upgrade test must detect version 14.
- Change an adapter's common gate back to request.validate(). Its refusal test
  must fail (a checkpoint/transport error is not the required Unsupported).

These are prepared controls, not reported results. Formatting and whitespace
checks alone do not establish behavior. There were no cloud calls or source
package mutations during implementation.

Parent integration on 2026-10-01: the first run exposed a fixture permission
error in seven tests that opened tempfile's existing root directly. The actual
package RO recovery test, using a private child directory, passed. Fixtures now
explicitly create mode0700 roots; production permission checks are unchanged.
All eight journal tests passed at 09:29:27 UTC. A negative control disabling
semantic equality failed the mismatch test at 09:29:53 UTC, demonstrating that
changed document contents cannot be acknowledged solely by matching identity.
The equality check was restored before further validation.
Artifacts: `.local-state/icloud-access-package-journal-{integration,private-fixture}-2026-10-01/`
and `.local-state/icloud-access-package-receipt-semantic-negative-2026-10-01/`.

After restoring semantic equality, all eight journal tests passed again at
09:30:15 UTC. The combined allocation/representation tree then started the full
`scripts/check.sh` at 09:30:25 UTC; its result remains separate from these focused
checks. Normal mounted package writes are still unavailable.

The first complete check ended at 09:33:00 UTC on a stale migration fixture:
actual schema15 versus expected14 in causal_edits. Audit found eight migration
test files whose current-version assertions also needed updating; future-version
preservation assertions now correctly expect16. No production gates were weakened.
All eight affected test targets passed in two serial runs ending09:34:00 and
09:34:47 UTC (`package-schema-migrations` and `package-schema-migrations-extra`).
A new full check follows because focused targets do not replace the required gate.

Complete `scripts/check.sh` passed 09:35:36–09:43:26 UTC: formatting, Clippy,
workspace and iCloud feature tests, real kernel mounts, scripts/ledger and docs.
Artifact `.local-state/icloud-access-package-journal-allocation-fullcheck2-2026-10-01/`.
This verifies the integrated representation/allocation checkpoint; the next real
PACKAGE adapter and installed-daemon acceptance are not covered by this result.
