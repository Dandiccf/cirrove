# Read-only verifier for the public native-import arm

Feature-only command, prepared but not executed:

```text
cirrove-icloud-mounted-write-probe --public-native-verify ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc
```

After the public CLI succeeds, first retain its job receipt and gracefully stop
only the isolated public daemon. The helper opens its actual journal with the
read-only `RecoveryJournal`; the exclusive journal-owner lease remains held
through verification. It refuses an active writer, any journal with more than one
upload, and any row not fully Uploaded with an exact package completion receipt.
This is intentionally not a restart/recovery/replay helper.

The account/state/label come from the fixed private bootstrap location and ready
receipt, with Apple username/subject (and tenant/Graph identity fields) equal
to the retained ac9 source account. The bootstrap now applies this same guard
**before** loading/saving session material, so another GUI account cannot start
this arm. The exact imported item ID and revision come exclusively from the actual
saved public journal row. Guards bind account/provider/collection, Create intent,
owned parent, fixed destination, raw source length/hash, archive root and freshly
computed semantic identity. An ordinary file, replacement, uncertain upload,
missing semantic receipt or mismatched identity is refused before cloud access.

The old ac9 source plan, original synthetic source archive, and account metadata
are read without altering any old receipts or plans. A fresh attempt directory
under the public bootstrap root retains manifest and results on every invocation,
so verification can be repeated without submitting an import again. The verifier
never allocates, uploads, registers, removes or replays a cloud operation.

A freshly reconstructed session lists the owned parent and requires the original
synthetic source to retain its exact metadata. It finds the journal item by exact
ID, refuses duplicate IDs/names, binds its revision before and after a bounded
package download, then compares decompressed content using explicit source and
destination archive roots. It subsequently reuses `verify_bound`: normal iCloud
read-provider/FUSE view, offline remount, and fresh-provider refetch, each compared
to the independently verified canonical archive. Final metadata is checked again.
These are isolated read-only verification mounts, not a claim that the stopped
public daemon's original mount was exercised. The parent separately verifies
publication/visibility on that public mount before shutdown.

The existing owned-package manifest requires btrfs, records binary hash/PID and
has a 900-second bound; dispatch applies the same 900-second timeout. Its existing
FUSE harness and cleanup behavior are reused. On timeout retain artifacts and
inspect exact recorded mounts/processes before any follow-up. The bootstrap,
ordinary installed service and default accounts must not be restarted or replaced.
Native Apple application open remains an explicit separate check.

Three synthetic tests are prepared under
`validation::icloud_account::owned_package::public_verify::tests`.
They cover exact receipt binding against ten wrong/uncertain cases, duplicate and
changed remote metadata, and rejection of an unregistered run before I/O.
Meaningful negative control: remove the `row.state == Uploaded` condition and
expect receipt guard arm 1 to fail; remove the duplicate-ID condition and expect
the metadata test to fail. Restore before green validation. No tests/builds/cloud
calls have been run by the author of this helper.
