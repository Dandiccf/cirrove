# Versioned native package content identity

The feature-only `package_archive_semantic_identity` helper is intended for the
explicit durable PackageArchive upload qualifier/receipt. The read-only mounted
validator can record it immediately. It does not route uploads, enable writes,
or change the strict or explicit-root archive comparison contracts.

Inputs are an already opened private file, its independent raw size/SHA-256
receipt, an explicit expected enclosing root name, and a cancellation token.
The existing bounded ZIP parser verifies the raw receipt, archive safety and
expanded contents, then removes exactly the supplied root prefix in memory.
No root is inferred. Explicit root directory entries remain in the manifest as
an empty relative path, so root-entry presence still matters.

Output is the core `PackageSemanticIdentity` data type with version, SHA-256, entry count, regular
file count, and total expanded bytes. No paths, file contents, provider IDs or
URLs are returned. Serialization contains only those five fields. Deserialization
rejects unknown versions, unknown fields, malformed lowercase SHA-256 and counts
outside the current validator bounds. Account, operation, original item/ETag,
raw ZIP receipt and expected root remain separately required authority bindings;
this content identity does not replace any of them.

## Canonical version 1 byte framing

Hash the following byte stream with SHA-256; serialize the resulting 32 bytes
as 64 lowercase hexadecimal characters:

1. Exact ASCII domain `cirrove.package.semantic.identity` followed by one NUL.
2. Version `1`, entry count and regular-file count, each unsigned 32-bit big-endian.
3. Total expanded bytes, unsigned 64-bit big-endian.
4. For every entry in exact UTF-8 lexicographic order of its root-relative path:
   one ASCII type byte (`D` directory or `F` regular file); unsigned 32-bit
   big-endian UTF-8 path length; exactly that many path bytes; unsigned 64-bit
   big-endian decompressed length; exactly 32 binary bytes of the decompressed
   content SHA-256.

Directory content has zero length and the SHA-256 of empty input. There is no
Unicode normalization, case folding, path flattening or implicit-directory
insertion. No timestamps, compressed representation, ZIP ordering, enclosing
root name, remote identity or raw archive hash enter this semantic digest.
Every variable-length field is framed explicitly; content digests have fixed
binary width. Future changes to interpretation or framing require a new version.
Existing data must never silently acquire the new meaning.

Resource bounds remain 64 MiB raw archive, 64 MiB total expanded content, 10,000
entries, 16 MiB central directory and 1,024-byte names. Run the synchronous helper
in a blocking task, retaining operation/file ownership through cancellation.

## Prepared tests and meaningful negative controls

Five new tests cover identity equality after order/compression/timestamp/root
renaming; changes to inner paths or bytes; swapping empty file/directory kinds
while counts, lengths and content hashes stay equal; a literal v1 framing vector
plus domain/version separation; and invalid serialized identities, raw receipts,
roots and cancellation. The kind-swap test specifically fails if type tags are
omitted, without relying on a changed aggregate file count. Removing root binding
fails the renamed-root equality case; omitting path/content fields fails the
corresponding mutation case. The literal frame protects persisted format
compatibility independently of the entry-encoding helper.

No builds/tests/cloud operations were run by the implementing agent. No owned
package service or mounted-helper files are changed by this patch.
