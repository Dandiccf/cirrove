# Public native import acceptance: preregistration

Run identity: `ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`. Status: second attempt uploaded and independently verified; public job falsely reported a revision mismatch, correction pending.

Question: does the normal Cirrove daemon's public import request create one
owned Pages document, retain its exact identity through the upload journal, and
publish it into a warm mounted directory before reporting success?

Prediction: the public path produces a verified PACKAGE identical in semantic
contents to the previously created synthetic Pages source. A successful job is
insufficient alone: require independent exact-ID readback and mounted reads.

## Preconditions and bounds

Finish the full project check and synthetic socket-to-FUSE acceptance first. Use
a fresh isolated state directory, socket, mount and writable validation account;
never restart the installed daemon or change existing account registrations.
Reuse only the existing validation session through its established sealed-session
contract; do not print credentials or copy them into configuration.

The source is the verified 55,715-byte archive from owned source run
`ac9e5456-bd10-4b7d-9215-21bbb85dde69`, with its explicit root and semantic receipt.
Verify its retained raw hash/size and source provenance before submission. The
source must be staged as a private local regular file outside Cirrove state and
mounts, without changing archive contents.

Destination: `Cirrove Public Import ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc.pages` in that run's own
`Cirrove Package Validation ac9e5456-bd10-4b7d-9215-21bbb85dde69` folder. Confirm
that exact owned parent and vacant name. Submit once only. Unknown request or
allocation outcome means inspect retained operation; never resubmit a copy.

Record command, source commit/diff, binary hashes, private disk TMPDIR and
SQLITE_TMPDIR, filesystem, PID and duration before starting. No overlapping build
or measurement. Maximum observer20 minutes; source/expanded bounds64MiB. No
personal document, sharing changes, permanent deletion, or recursive cleanup.

## Required evidence

- Public CLI/socket returned job and durable operation IDs; admission alone did
  not report success.
- Exact journal identity, account, collection, parent, name and package semantics
  agree with the independently verified receipt.
- Warm directory shows new package after metadata publication; no content fetch
  is needed solely to show it.
- Normal FUSE read, offline retained read and fresh provider refetch agree in
  root-bound semantic contents. Raw archive representation is measured separately.
- Apple's Pages UI opens only the new owned document and shows the synthetic
  sentinel.
- Cancellation, daemon interruption and uncertain replies remain separate
  synthetic gates; this successful arm does not prove those live failure modes.

This create-only arm does not establish existing native-document replacement,
Numbers/Keynote compatibility, installed acceptance or full iCloud support.

## Synthetic public-route gate

Checkpoint `a176c77` passed the full project check at 10:22:29 UTC. The subsequent
public socket fixture first needed a compile-only correction from a nonexistent
Reconciliation variant to UploadError::Uncertain. Its normal scenario passed at
10:23:48, and the ignored real-FUSE scenario passed at 10:23:57 (one test each).
Both cover actual socket submission/status/stop, durable worker allocation and
stream, typed receipt, warm metadata publication, read-only/account refusal, and
stopping observation without discarding or replaying the retained upload.

A negative control replaced the metadata publication refresh with a direct
provider node fetch. At 10:24:42 the public scenario failed because the warm
metadata lacked the confirmed package. The production refresh was restored.
These synthetic providers make no Apple calls and do not establish native editor
compatibility. At this checkpoint the public live arm had not yet executed; its later outcome is recorded below.

Restored public socket and real-FUSE scenarios both passed at 10:25:14 UTC.
The desktop integration is a subsequent uncommitted change and has separate
model/window checks; it was not covered by checkpoint a176c77's fullcheck.

## Desktop preparation

Three model tests passed at 10:26:41 UTC after a compile-only moved-Rc closure
fix. The native GTK fake-socket dialog scenario passed at 10:27:31; its screenshot
showed clipped entry labels. Shorter labels with wrapped help and .pages chooser
support were then added. The updated scenario passed at 10:30:58; screenshot
`.local-state/icloud-access-native-import-window-layout-2026-10-01/dialog.png`
was visually inspected: all field labels are readable. The test submits one
explicit request and refuses a stale writable selection. It bypasses the file
portal itself, so it does not establish portal or installed GUI acceptance.

The additive account-binding capability prevents new GUI imports against older
daemons that would ignore the expected account ID. CLI label-only requests remain
supported. Dedicated public-route binding tests are separate from the screenshot.

All 21 native window scenarios passed at 10:33:52 UTC. Public binding tests
(including real FUSE and legacy CLI request decoding) passed at 10:33:11.
Removing only the expected-account rejection made the public test fail at
10:32:38; it was restored before that green run. The three desktop model tests
passed at 10:34:25. Removing the GUI's binding-capability requirement then failed
the old-daemon refusal assertion; it was restored.

Reviewed synthetic dialog: [native import screenshot](icloud-native-import-dialog-2026-10-01.png).
The screenshot is synthetic English/dark-mode evidence; German, light-mode and
portal click-through remain separate checks.

## Local preparation completed

Fresh account/credential bootstrap completed at 10:38:45 UTC without Apple calls.
The source identity was checked against the retained source account before its
session was cloned. The isolated root is `/var/tmp/cirrove-public-native-ec7f82e1`.
The live runner records binary hashes, source commit/diff hash, exact PIDs and
disk-backed temp directories before starting the separate daemon. It checks the
owned parent ID through the public paths API, warms that mounted folder, requires
a vacant destination, submits the public CLI once, then checks the successful job
receipt and the original mount before stopping only that daemon. No builds run
concurrently. Independent verification follows after its journal owner closes.

## First public live attempt: admission refused

The isolated daemon resolved the exact retained parent and warmed its two-entry
mounted directory; the new destination was absent. The single CLI request
started job ad641312-d979-4425-8e5d-e28a0aaf10e3, then reported admission not
confirmed. The daemon exited gracefully at 10:40:32 UTC. Read-only inspection
found zero uploads, zero write-queue rows and zero metadata-publication rows.
The audit and original logs remain under the public run directory. No retry has
been submitted and no successful public cloud import is claimed.

Code inspection identified a real adapter/admission mismatch: destination traversal
forces Engine::refresh_node on a child folder, while ICloudDrive::node deliberately
refuses every non-FILE ID except the root. The existing folder_metadata endpoint
can provide exact folder metadata, but that path is not exposed through node().
A real HTTP regression test and bounded folder-node implementation are next.
Do not bypass destination checks or infer success from the earlier owned validator.

## Folder observation correction and controlled second admission

The exact HTTP folder-node regression failed against the previous provider with
`Unavailable`. After adding the bounded ordinary CloudDocs folder observation,
all three folder-node tests passed. The full iCloud library suite with write-probe
then passed all 228 tests, including the four package Trash verification tests.
The Trash HTTP fixture initially failed because its anonymous tempfile lacked
private permissions; it now sets and checks mode 0600, without weakening the
production guard. The server wait is bounded and follows the refusal assertion.
Evidence: `icloud-access-native-folder-node-negative-2026-10-01`,
`icloud-access-native-folder-node-positive-2026-10-01`, and
`icloud-access-native-folder-and-trash-suite-2026-10-01` under `.local-state`.

A second admission attempt is preregistered for the same isolated account and
vacant synthetic destination. This is permitted only after independently checking
that the first daemon is stopped and all three durable operation tables are empty.
Original logs/manifests are preserved. Attempt 2 uses separate logs and manifest,
checks the already staged archive against the retained source hash, and repeats
exact parent identity and name-vacancy checks before one submission. Prediction:
the ordinary folder refresh now permits admission; upload and warm-mounted
publication must both complete before success is reported. Any retained operation
or uncertain outcome forbids another submission. This is still not evidence of
editing/replacing an existing native document.

Independent review found that recovery-marked folder envelopes initially mapped
to `Unavailable`, preventing Engine from publishing absence. A strengthened test
failed with that exact error before the typed `InactiveFolder` correction. The
corrected provider maps it to `NotFound`; all 228 feature-enabled library tests
passed again. Default-feature folder-node tests also passed before this final
error-classification change. These are correctness tests, not latency measures.

## Second live attempt: verified upload, publication comparison defect

Attempt 2 stopped its isolated daemon cleanly at 10:54:48 UTC. Admission queued
operation `b534c269-8b9e-47e0-97a0-a512d0127765`; its durable state is Uploaded,
with the expected semantic receipt and one completed metadata-publication row.
The exact new identity is
`FILE::com.apple.CloudDocs::8A909C8D-32D7-4E39-9202-DF1862E46C5A`.
No second upload is authorized or needed for this destination.

The public job incorrectly failed: upload receipt and observed package agreed on
identity, parent, name, folder/package kind, logical size and ETag, but the receipt
set `content_version=etag`, whereas the canonical package-folder read projection
leaves content_version absent. The shared revision comparator intentionally treats
these two revision namespaces differently. Its global semantics must not be
weakened. Correction must align new receipts and narrowly recognize already saved
legacy aliases without rewriting their journals.

The separate read-only `--public-native-verify` run then passed: exact uploaded
receipt binding, root-bound semantic comparison, ordinary mounted FUSE reads,
offline remount and fresh refetch. Evidence is
`.local-state/icloud-access-public-native-independent-verify-2026-10-01` and
`/var/tmp/cirrove-public-native-ec7f82e1/verify-d512fb47-50ec-44c3-a560-06d8e1e1b181`.
The original runner's post-success mount check was not reached because of the
false job failure. This independent remount does not retroactively prove that
check. Apple Pages UI acceptance and the corrected public success path remain
open; full native-document editing/replacement is not established by this import.

## Revision contract regression controls

Before applying the correction, both service publication tests failed: legacy
receipt versus canonical observation, and mismatched logical size. The independent
verifier's new canonical-receipt case also failed, and the real HTTPS package-create
roundtrip failed its new canonical ETag-namespace assertion. Their evidence arms
are `package-revision-negative`, `package-verifier-revision-negative`, and
`package-receipt-revision-negative` under the dated access-run directories.

The correction emits canonical package-folder receipts with no independent
content_version, checks exact logical size alongside identity/name/parent/ETag,
and accepts only the exact historical ETag alias in already-retained receipts.
It does not change the shared revision comparator, rewrite journals or resubmit
cloud work. A complete `scripts/check.sh` run is now in progress in
`icloud-access-native-import-checkpoint-fullcheck-2026-10-01`; no passing result
or commit is claimed yet.

The first complete check stopped in workspace service tests: 296 passed, one
public-socket fixture failed, 25 ignored. The fake read provider had reused the
legacy upload receipt's content_version alias, contradicting the real read
projection. The fixture now returns canonical read metadata while deliberately
retaining the legacy upload receipt; additional assertions require canonical
public completion and an unchanged historical receipt. This corrects the fixture
rather than weakening production revision checks. Full-check success remains
pending after focused socket validation.

The corrected socket fixture passed both the normal public protocol scenario and
its real-FUSE variant. The complete second `scripts/check.sh` finished successfully
at 2026-10-01T11:12:09.520850+00:00 (exit 0), including format, Clippy, workspace,
feature-gated iCloud probes, kernel mounts, script/translation/ledger checks and
documentation. Native GTK window evidence remains the separate 21-scenario run
recorded above; the check script does not run those windows. This checkpoint is
not installed and does not close native editing or release acceptance.

## Subsequent observation and Apple Pages acceptance

The exact retained upload was successfully re-observed through the new public
observer-only command after restart; its warm mounted view was confirmed and
its journal rows stayed unchanged. Apple Pages opened the exact allocated
document and displayed its synthetic source text. See
[retained import observation](icloud-native-import-watch-2026-10-01.md).
The historical false-failure job and missed original post-success mount check
remain recorded; neither was replaced by a new upload or rewritten evidence.
