# Owned native Package CREATE: preregistration

Operation UUID: `0c5c1be4-563d-4a1c-8a47-3958c0c88553`.

Question: can a freshly created synthetic Pages document be imported as a new
native Package through Cirrove's own transport and then opened in Pages with
identical semantic contents? Prediction: Apple's PACKAGE/add_package contract
accepts the downloaded source archive and independently verified entry contents
match, but this is unproven. Failure keeps all evidence and never retries an
uncertain registration. No overwrite or existing personal document is used.

## Source acquisition

Create only a new root folder `Cirrove Package Validation 0c5c1be4-563d-4a1c-8a47-3958c0c88553` and a new
Pages document `Cirrove Package Source 0c5c1be4-563d-4a1c-8a47-3958c0c88553.pages` inside it through Apple UI.
Its sole test text is `Cirrove native package validation 0c5c1be4-563d-4a1c-8a47-3958c0c88553`. This is fresh
synthetic content. Record UI completion and exact source identity before download.
Do not select the first existing Pages file or use the personal app-container
probe helpers. No sharing/access changes, no cleanup or permanent deletion.

## Import endpoint

Destination `Cirrove Package Import 0c5c1be4-563d-4a1c-8a47-3958c0c88553.pages`, a new identity in the same
owned folder. Independently bind source account/scope, parent, source ID/ETag,
archive SHA256/size to a sealed one-shot checkpoint. Max archive64MiB, response
64KiB, checkpoint96KiB. Persist each uncertain phase before request; never replay.

Require exact allocated/registered/imported identity; confirm Package representation;
separate-session download with source-before/after revision checks; bounded safe ZIP
entry type/name/decompressed length/SHA256 equality with source (Stored+Deflate),
without extracting paths. Raw ZIP equality is separate from semantic equivalence.
Read imported contents through normal adapter/FUSE and offline remount; open in
Pages and verify the synthetic text. Installed behavior, replacement, shared
documents, and app-container writes remain unproven by this CREATE arm.

Initial status at registration: no source creation or transport arm executed. Binary/PID
and private disk TMPDIR/SQLITE_TMPDIR must be recorded before transport execution.

## Source UI preparation, 2026-10-01 07:45–07:50 UTC

Created the uniquely named validation folder in iCloud Drive through Apple's UI.
Created a new Blank Pages document, entered the registered sentinel, and renamed
it to the registered source name. Pages initially created it in its default
app folder; moved only this new synthetic document into the validation folder
using Apple's Move to Folder action. The editor screenshot visibly showed the
exact sentinel, and Drive File Info / folder view then confirmed the new folder
contains exactly one Pages document (shown as 99 KB). No personal document was
opened, copied or modified, and no sharing permissions changed.

The browser UI establishes the test source's provenance, not its protocol
representation, zone, item ID, or archive identity. Those must be acquired through
the read-only exact-name source step before any import. Browser folder tab
1216488595 was retained for the subsequent workflow; no import ran.

## Implementation and synthetic validation

The feature-only one-shot creator, independent ZIP semantic verifier and split
source/import/verify entrypoints are now integrated in the development worktree.
They have not performed a cloud import. Ordinary provider package writes remain
protected. Deflate verification adds only `flate2 1.1.10` and `zlib-rs 0.6.7` to
the lockfile through the existing ZIP crate's explicit write-probe feature.

All 13 creator tests passed, followed by a meaningful cancellation negative: with
the post-checkpoint cancellation guard removed, the actual execute test observed
one mutation request instead of zero (`package-cancel-red3`). The first two red
attempts failed to compile due to a test-only macro import and its misplaced inner
attribute; neither is negative evidence. Those test imports were corrected. The
restored creator passed all 13 tests (`package-create-green`).

Removing semantic manifest equality made the changed-content assertion fail
(`package-semantic-red`). Restoring it passed all six semantic tests, including
compression/order equivalence, changed contents, unsafe/duplicate paths, CRC,
unsupported entry types, bounded expansion, receipt mismatch and cancellation
(`package-semantic-green`). Logs/manifests use the common
`.local-state/icloud-access-<arm>-2026-10-01` paths. These are synthetic results,
not proof of Apple native format compatibility.

The two service guard tests also passed (`package-service-green`): retained
read-only verification attempts and private artifact symlink/size refusal. The
feature-enabled mounted probe passed Clippy with warnings denied
(`package-probe-clippy`). A complete check for these additional package changes
and their commit are still pending; the earlier complete check covered the
ordinary-file access checkpoint only.

## Live source acquisition

Read-only source acquisition passed 08:19:41–08:19:50 UTC using binary SHA256
`c82124ca9eca5b9b8231519dc1c01200bae7a7b901af35589c96f04a668bcb4e`.
The exact owned source is in `com.apple.CloudDocs`, with the expected parent and
fresh destination name. Its verified Package archive is 55,715 bytes, containing
10 entries / 7 files and 98,835 expanded bytes. Source revision checks and ZIP
self-verification passed. No import occurred in this step. The sealed source
plan and receipts are retained under the UUID's private run directory.

The separately invoked import will use this exact retained plan and binary; no
source changes or archive rewriting are allowed between these phases. This first
arm remains one-shot on any uncertainty; read-only verification may be repeated
in a fresh retained attempt directory.

## First live import outcome — refused allocation

The first one-shot import ran 08:20:27–08:20:33 UTC with the same binary and
failed `invalid or shared package allocation`. The response passed JSON decoding
and the single-slot count but failed Slot::validate's combined structural guard.
This does not establish which individual field caused the rejection. No raw
provider response, URL or secret was logged. The failure is before the body upload
and before add_package registration. Allocation may have occurred remotely; the
armed checkpoint and permanent owner marker remain, and this operation must not
be replayed. No native import success is claimed.

Next: compare the guard assumptions with the existing FILE allocator and public
Apple schema; split diagnostics into non-sensitive structural categories before
any separately registered fresh attempt. Do not relax shared-account or source
identity restrictions based on this combined error.

## Fresh second arm preregistration

Run `ac9e5456-bd10-4b7d-9215-21bbb85dde69`. Create a new root folder `Cirrove Package Validation ac9e5456-bd10-4b7d-9215-21bbb85dde69`.
Through Apple UI duplicate only the synthetic Pages source from arm
`0c5c1be4-563d-4a1c-8a47-3958c0c88553`, rename the copy
`Cirrove Package Source ac9e5456-bd10-4b7d-9215-21bbb85dde69.pages`, and move only that copy to the new folder.
The known document text deliberately remains the first arm's synthetic sentinel;
no personal document is used. Source acquisition must independently bind the new
identity, folder, revision and downloaded archive. Destination is
`Cirrove Package Import ac9e5456-bd10-4b7d-9215-21bbb85dde69.pages`.

Prediction: allowing the Apple-schema-defined optional owner and empty owner_id
without using them as registration authority will remove the unsupported local
restriction, while exact owned-root identity and URL restrictions still hold.
This is not a replay of the first armed operation. Bounds and acceptance endpoint
remain identical. Record new binary and source receipts before import; no retry
on uncertain mutation.

Second-arm UI preparation completed: the synthetic source was duplicated,
renamed and moved using Apple Drive. Get Info confirms its location ends in
the second run folder UUID. The original synthetic source remains unchanged.
The upcoming source probe independently checks exact parent and sole source.

Second source acquisition passed 08:33:17–08:33:56 UTC. Binary SHA-256
`922147fb11583f416d3d331a21393c5d9974024a697d90c74b64746f3fe91419`.
Exact owned parent, sole source and revision checks passed. Archive: 55715 bytes; 10 entries / 7 files; 98835 expanded bytes. Semantic self-check passed.
The separately invoked one-shot import uses this retained plan and binary.

## Second import and initial verification

Import passed 08:34:25–08:34:39 UTC: allocation, content transfer, registration
and exact identity readback succeeded. Independent read-only verification ran
08:34:48–08:34:57 UTC and failed the strict ZIP path comparison. Retained
archives each contain 10 entries / 7 files and 98,835 expanded bytes. Local
inspection found identical lengths and SHA-256 values for all seven files,
but Apple renamed the enclosing root from the source filename to the requested
import filename; entry order also differs. This is not yet a verifier pass.
Next correction must require the two explicit expected root names and compare
all relative paths, kinds, sizes and content hashes unchanged. Arbitrary prefix
removal is not acceptable. No upload replay is needed or permitted; verification
can use a fresh read-only attempt after corrected tests.

Native Pages UI open succeeded after the initial independent download. The
editor title is the exact second-arm Import filename and its document text
shows the original synthetic sentinel `Cirrove native package validation
0c5c1be4-563d-4a1c-8a47-3958c0c88553`. No editor changes were made. This proves
Apple Pages can open this imported package; corrected automated semantic
verification and mounted/offline acceptance remain outstanding.

## Registered read-only mounted follow-up

Use a fresh retained verification attempt for the existing second operation,
via `--owned-package-mounted ac9e5456-bd10-4b7d-9215-21bbb85dde69`. Before mounting,
inspect the sealed allocated identity, exact two-item owned inventory, unchanged
source and root-bound semantic comparison against an independently downloaded
import. Canonicalize only the private verifier copy, then compare all mounted
bytes through the normal provider, a cache-only offline remount and a fresh
provider refetch. Prediction: all three digests equal the independent canonical
receipt. This makes no further cloud mutation and never chooses a personal
Pages sample. Deadline900s, private btrfs staging, no overlapping compilation.

Mounted-binding synthetic control: removing exact package identity/revision
checks failed the guard test (08:45:36–08:45:58 UTC). Restoring them passed
the same test (08:46:12–08:46:20 UTC). These are guard tests, not live mounted
acceptance; the separately registered owned imported artifact arm follows.

A broader optional `clippy --all-targets --features icloud-write-probe` run
failed existing probe-binary test-module lints (`unwrap_used` and items after
test modules), outside the changed package code. The repository check uses
feature-probe binary targets individually; that required target check is run
separately and the broader failure is not counted as a pass.

## Owned imported package mounted result

The registered read-only follow-up passed 08:48:10–08:49:03 UTC (52.95s),
exit0, binary SHA-256
`f31e937439cf574f7b7d9e0b365a075825450c2f88b7bc828423fd1f0d70bc17`.
The exact allocated import and unchanged original source passed the corrected
root-bound semantic comparison. All bytes then matched the independent
canonical receipt through normal provider/FUSE, offline remount, and a fresh
provider refetch. This follows the separately observed native Apple Pages
open with expected synthetic text. No further upload or registration occurred.
This closes the bounded owned CREATE/readback experiment, not normal journal
package writes, native replacement, general format compatibility or installed
release acceptance. Complete repository check still follows before commit.

Repository validation for this checkpoint: complete `scripts/check.sh` passed
09:16:09–09:23:56 UTC, including feature-specific package tests and kernel mounts.
Artifact `.local-state/icloud-access-journal-shared-owner-fullcheck-2026-10-01/`.
This does not enable native package writes through normal mounts or establish
installed-daemon acceptance.
