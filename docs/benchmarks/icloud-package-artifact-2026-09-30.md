# Complete iCloud package artifact staging

The range investigation established that the sampled Pages package ignores a
subrange request. This arm exercises a new complete-download primitive, with a
caller-owned private disk sink and no whole-document allocation. It is a step
toward mounted package reads, not a mounted package implementation or write gate.

The primitive checks exact source identity, document ID, parent, kind, ETag and
logical size before and after the transfer. Only a Package representation is
accepted. Its actual byte count and SHA-256 are returned only after complete
HTTP 200 identity transfer and the final source check. Content-Range, partial or
encoded bodies, truncation, zero-length bodies, budget overrun and cancellation
refuse the receipt. Chunks sent to the sink are at most 64 KiB. On failure the
caller must not publish any staged data. The caller chooses the byte budget;
there is no claim of a new Apple service capacity.

A synthetic regression cancelled from the final sink write. Without explicit
in-loop/final cancellation checks it returned a receipt and the test failed
(exit 101). After correction it refuses publication. Other tests cover complete
and chunked HTTP responses, declared and undeclared size bounds, malformed or
partial bodies and source identity/revision/parent/size changes. These tests do
not prove live mid-transfer revision changes or mounted cache behavior.

## Registered live arm

Question: can the sampled native Pages package be downloaded completely into
private disk staging, despite rejecting ranges, with its source revision stable
and an independently reread local digest matching the streamed digest?
Prediction: the full GET succeeds; the artifact size may differ from the listing
size and must not be forced to match it. Matching size, if observed, would not
make future generated representations safe for ranged reads.

Use the existing isolated session of successful fixture
`21d05f56-70c4-47a8-a5e3-577ab68994ac`. Select the first FILE with extension
`pages` under the fixed Apple Pages container, as in the preceding bounded
probes. Record only the selected identity/version in a mode-0600 private receipt,
never names, provider bodies or credentials in public output. The private artifact
contains user document data and is retained in its mode-0700 run directory. It is
not attached, printed, extracted, edited or uploaded. No cloud mutation occurs.

Budget 64 MiB for this arm; transfer deadline 300 seconds, outer deadline 600
seconds. Record command, binary hash, process ID, private disk-backed TMPDIR and
terminal status before interpretation. No competing compilation or measurement.
The normal daemon, connection, mounts and write gate remain untouched. Independent
post-transfer verification streams the local file with a 64 KiB buffer and checks
size and SHA-256. This validates disk receipt consistency, not Apple's package
format or application-level fidelity. Full mounted integration remains required.

## Live result

Arm `ca805072-452b-458a-86df-b4e773e9a5da` passed in 9.537 seconds. The source
listing reported 21,957,877 bytes; the complete package artifact contained
21,759,419 bytes, a difference of 198,458 bytes. The source revision/identity
checks passed on both sides, and an independent disk reread matched both the
streamed size and SHA-256. No contents were printed or changed in iCloud.

This proves that using the listing size for the downloaded representation would
be wrong for this sample, not merely that the server ignores Range. The primitive
can deliver a complete staged artifact with its own size. It is not yet wired
into the normal mount, has not been checked in an Apple application, and this
single arm does not establish stable latency, memory measurements or all-package
reliability. The test file remains private and is not attached to this report.
The full `CARGO_TARGET_DIR=.target-icloud-feasibility scripts/check.sh` passed
with exit 0, including workspace/feature tests, real-kernel FUSE tests, scripts,
ledger and docs. The existing rustdoc link warning remains. Graphical window
scenarios and installed-daemon acceptance were not run.

## Separate post-run local archive inspection

After the registered transfer ended, Python's standard zipfile reader inspected
only the retained local artifact. It contains a valid ZIP directory with 56
entries and 53 files. Their uncompressed sizes sum to 21,957,877 bytes, exactly
the source listing size; the compressed payloads total 21,748,219 bytes, plus
ZIP framing for the 21,759,419-byte downloaded artifact. All entries passed CRC
validation under a 64 MiB uncompressed / 10,000-entry inspection bound. No entry
names or contents were printed and nothing was extracted.

This explains this sample's size discrepancy: the listing counts uncompressed
package contents, whereas the downloaded entity is an archive. This is a
post-run observation, not the preregistered endpoint or an Apple application
open/save test. It supports presenting the archive with its actual size; it does
not establish native package editing or permission/share preservation.

## Next mounted integration boundary

The source FILE identity and logical size must remain distinct from the generated
archive's byte identity. Package classification must use provider representation
metadata, cached by account/collection/item/revision, rather than treating every
`.pages` suffix as proof. Do not add an unbounded eager lookup per root file.
A representation entry must publish its actual size and digest-bound content
version only after complete source-checked staging. Network awaits stay outside
metadata transactions and filesystem locks; cancellation must leave prior visible
metadata and valid cached bytes intact.

The shared `children_for_node`/`staged_content` publication mechanism already
handles generated Google representations, but its `Arc<[u8]>` staging handoff
would require a whole-artifact allocation. The new package primitive deliberately
uses the existing `ReadWindowSink` chunk contract instead. Mounted integration
must connect that private disk staging to the common cache before publishing
representation metadata, preserve scope identity, and validate restart/offline
reads. The successful transport arm does not close those acceptance requirements.
