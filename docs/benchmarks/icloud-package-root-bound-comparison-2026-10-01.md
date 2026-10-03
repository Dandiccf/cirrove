# Explicit package-root binding for native import verification

Apple's owned import can name the enclosing archive directory after the new
Drive document, while the source archive uses the original document name.
The existing strict semantic comparator correctly treats those different entry
paths as unequal. It remains unchanged for callers that require exact paths.

A separate feature-only `compare_package_archives_with_roots` API takes the
expected source and imported root names explicitly. Each must be a single safe
path component. The service obtains these names from the retained source entry's
`display_name()` and the immutable plan's destination name, after independent
allocated identity and owned-folder/source checks. No name is inferred from the
archive or derived from an unverified returned prefix.

Both archives first pass the existing bounded ZIP/receipt checks. Every entry
must lie under its exact expected root; a top-level entry with that exact name
must be a directory. The comparison removes exactly that one directory prefix
in memory. Explicit root directory entries remain represented, so omitting one
on only one side does not silently pass. Empty packages without file content are
refused. Extra roots, similarly prefixed roots, unwrapped entries, traversal,
duplicates and unsafe expected roots fail. Nested paths, entry types, lengths
and decompressed hashes still have to match exactly. No file is extracted or
rewritten. Original archive hashes remain reported separately.

Six prepared tests cover expected wrapper renaming (with old strict comparison
still failing), wrong/unsafe roots, extra roots and unwrapped entries, inner
path/content/root-directory changes, duplicate/traversal paths, and a file
masquerading as the root. The positive fixture also changes order, timestamps
and compression. A negative control that removes the explicit root mapping
must fail the positive test; inferring roots rather than binding the supplied
names must fail the wrong-root test. No tests/builds/live operations were run by
the implementing agent. This comparator does not establish native Pages UI
validity or authorize another import; existing read-only verification can use a
fresh retained attempt without replaying any cloud mutation.

Parent validation: with explicit root mapping removed, the targeted acceptance
test failed `package contents differ` (08:44:09–08:44:14 UTC). Restoring the
implementation passed all 12 semantic tests (08:44:35–08:44:37 UTC). This covers
expected root changes while retaining strict paths/content inside each root.
