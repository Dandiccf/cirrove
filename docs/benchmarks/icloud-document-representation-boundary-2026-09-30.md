# Native document representation boundary

The preceding metadata investigation showed that a FILE listing may provide a
package URL. The ordinary file integrity helper silently selected that URL.
A local HTTP regression reproduced acceptance of a package-only response with
the old helper (exit 101 at the package rejection assertion).

The transport now retains Data versus Package until the consumer chooses its
contract. Ordinary content verification, including replacement/Trash readback,
requires Data. Missing and ambiguous (both present) representations fail without
including URLs in the error. Existing exact read paths accept either distinct
representation but retain their exact length/range/revision checks. This does
not yet materialize a package with a different size; it is not a complete native
document implementation. Known empty ordinary files still use their existing
zero-byte path without content lookup. Package classification of zero-byte
metadata is not established by this change, and the normal write gate stays shut.

The focused HTTP tests pass after the change. They also check that Data and
Package remain distinguishable on reads and both enforce the Apple content-host
allowlist. No signed URL is logged or given a Debug implementation on the resolved
type. The final full repository validation passed; details are below.

## Read-only header arm (registered before execution)

Question: for one native document in each of Pages, Numbers and Keynote, does
HEAD advertise a representation length matching the listing and byte ranges?
Prediction: a package representation can have a different or absent length;
ordinary Numbers content may match. A successful HEAD does not prove range reads
or stable content. Select fixed Apple container identities and one native file
per container, now using Keynote's correct `key` extension. No recursive scan.

Use the successful isolated session from run
`21d05f56-70c4-47a8-a5e3-577ab68994ac`. Maximum four listings, three location
lookups and three HEAD requests. Never issue a content GET or read response
bodies from the content host. Record only aggregate schema, representation kind,
HTTP status, numeric lengths and boolean range/encoding indicators. No names,
item IDs, raw headers, URLs or tokens are output. Outer deadline 900 seconds;
record binary hash, process ID, disk TMPDIR and terminal status. Do not compile
or run another measurement during the arm. The installed daemon stays untouched.

## HEAD result and bounded range follow-up

Arm `ba7f5e4f-a3a0-479d-81a3-9d22a0ca763f` completed in 4.577 seconds.
Pages returned HTTP 400, Numbers and Keynote HTTP 501. The latter responses
advertised 17 bytes: those are error-response lengths, **not document sizes**.
HEAD supplies no usable evidence for read sizing in this arm. No content was
read. Keynote now selected a native `key` file and supplied a data representation.

Follow-up registered before execution: request `Range: bytes=0-0` with identity
encoding for the same fixed app sample selection. Predict data representations
support an exact 206 with total length; package behavior is unknown. Only consume
a response body when status is 206 and Content-Range declares exactly 0-0 with a
positive total; stop and reject any oversized chunk. Drop ignored-range/error
responses without consuming their body. Report numbers/booleans only, never the
byte. At most three content GET requests are added, each asking for one byte.
Keep the 900-second outer deadline and isolated disk TMPDIR. This is bounded
read-only access, no cloud mutation or installed-service change. A passing byte
range alone is not full artifact integrity or mutation acceptance.

## Range result

Arm `9d2a25a8-276d-46b3-8f70-2ce42c9c4206` completed in 7.938 seconds.
Numbers and Keynote returned exact one-byte HTTP 206 responses with identity
encoding. Their Content-Range totals equalled listing sizes (724,635 and
7,270,743 bytes). Pages returned HTTP 200 without Content-Range for the same
one-byte request; its body was dropped without consuming it. Only two validated
bytes were consumed and neither was logged. This is not a complete read of any
document and establishes no native write acceptance.

The observed package cannot use the existing exact-range reader: that reader
correctly refuses a full response to a subrange. Package reads need a separately
materialized artifact with verified source revision and actual artifact size,
then bounded local reads. The ordinary Data path can retain its current range
contract for these two sampled files; broader native formats remain open.

## Final validation

The initial full check stopped at clippy because the new test used `unwrap_err`.
The test now uses an explicit expectation. A fresh complete
`CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed with exit 0:
workspace and feature tests, actual FUSE tests, scripts, ledger and documentation.
The existing rustdoc link warning remains. Graphical window scenarios and an
installed-daemon test were not run. Both live arms are terminal; ordinary account
writes remain disabled. This is a transport correction plus bounded live
read evidence, not complete package support or release acceptance.
