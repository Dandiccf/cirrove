# Cirrove

[![CI](https://github.com/Dandiccf/cirrove/actions/workflows/ci.yml/badge.svg)](https://github.com/Dandiccf/cirrove/actions/workflows/ci.yml)

**Your clouds. One filesystem.**

Cirrove makes your cloud drives available as ordinary folders on Linux. Browse
from a local metadata index, download file contents when you open them, keep
selected files or folders offline, and use your existing editors and file
managers. A native settings window, tray and file-manager integration make the
connection and file state visible. Cirrove is an independent Apache-2.0 project
with a shared filesystem, cache and recovery journal for its cloud adapters.

**Status: early preview, actively developed and under validation.**
[Version 0.1.0](https://github.com/Dandiccf/cirrove/releases/tag/v0.1.0), released
on 18 September 2026, is the first OneDrive-focused release.
[0.2.0 Canary 1](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.1)
packages the newer OneDrive and Google Drive previews for testing. iCloud is
being developed separately and is not included in `main` or the released
packages. The table below describes implementation and validation status; it is not a general reliability guarantee.
See the [user guide](docs/user-guide.md) to get started and the
[compatibility matrix](docs/compatibility.md) for the tested boundaries. Keep a
separate copy of data you cannot replace while the preview is under validation.

## What you can do

- **Open cloud files in existing Linux applications.** Browse the cached
  directory index and fetch only the contents you open, rather than download
  the whole drive first.
- **Keep files or folders offline.** Pin jobs download the selected content,
  show progress, and retain it within the configured cache budget.
- **Save ordinary files through a writable connection.** With **Allow changes**
  enabled, saves enter a durable local journal and upload in the background.
  Local-save completion and cloud acknowledgement remain distinct.
- **Create, rename, move and trash ordinary files and folders.** Operations use
  provider identities and conditional revisions; unsupported package changes
  are refused. Deletion uses the provider's recycle bin where supported.
- **Keep work when a transfer is interrupted or refused.** Pending edits survive
  service restart; conflicts retain recovery data instead of treating an
  unconfirmed upload as success. The window and CLI expose progress and recovery
  actions.
- **Manage multiple connections from one desktop.** Connect, mount, unmount,
  sign in again and inspect account state through the GTK window and tray.
  English and German interfaces follow the desktop locale.
- **See file state in your file manager.** Files (Nautilus) and Dolphin expose
  availability badges and offline actions, backed by the daemon's state.
- **Control local storage.** Disk caches have budgets, pins reserve space, and
  local-data reporting shows the cache, index and retained changes.

These describe the current preview implementation. The
[compatibility matrix](docs/compatibility.md) and
[validation record](docs/validation.md) distinguish synthetic coverage from
real-account and application evidence. Native cloud documents and unsupported
filesystem operations have their own limits.

## Cloud services and current support

| Cloud service | Current status | What works and what remains open |
| --- | --- | --- |
| **Microsoft OneDrive** | Read/write preview in `main`; the focus of 0.1.0 | Browser sign-in, on-demand files, background saves, conflict recovery and offline pinning. Daily use and real-account recovery evidence are primarily from OneDrive for Business; Personal-account acceptance remains open. |
| **SharePoint document libraries** | Linked-library discovery and projection in `main` | Libraries and shortcuts use separate provider identities. Broader tenant permissions, folder-only sharing and revoked-access behavior still need real-account validation; general SharePoint support is not claimed by 0.1.0. |
| **Google Drive My Drive** | Read/write preview in `main` and Canary 1 | Browser sign-in, ordinary-file create/edit/rename/move/trash, cache and offline pins, with bounded two-account live checks. The Cirrove OAuth app remains limited to test users pending public-app verification. |
| **Google Workspace Shared Drives** | Read/write preview in `main` and Canary 1 | Explicit drive selection, listings, ordinary-file reads and writes, restart recovery and bounded live checks on an owned Workspace drive. Restricted roles and broader long-session acceptance remain open. |
| **Apple iCloud Drive** | Experimental development in [draft PR 86](https://github.com/Dandiccf/cirrove/pull/86) | Native Linux sign-in, read-only mounts and owned ordinary-file saves have live evidence. Twelve bounded Pages, Numbers and Keynote workflows cover DATA/PACKAGE documents, ordinary saves and atomic replacement, preserved originals in Trash, remount and reopening in Apple's editors. Export fidelity has documented limits. Installed connection/write opt-in/downgrade, Strata deployment and remaining reliability acceptance are still open. This integration is not yet part of `main` or a release; see [the progress and limits](#icloud-integration-progress). |

A further bounded Numbers trial recovered a lost final replacement confirmation
through inspection of the same operation, with no repeated cloud write, and
independently verified the new document and the original in Trash. See the
[iCloud validation record](docs/icloud-write-integration.md). A fresh Pages Desktop
import now also passed public completion, original-mount access, independent
content verification and Apple Pages open. A separate fresh CLI import passed
the same complete sequence, closing the bounded Pages import criterion. Broader
native editing, session recovery and installed upgrades remain open.
A [fresh LibreOffice Calc trial](docs/benchmarks/icloud-calc-after-metadata-publication-2026-10-06.json)
now completed two actual XLSX saves through an isolated iCloud mount.
Independent iCloud reads matched both versions byte for byte, and a fresh
read-only remount returned the second version exactly. This follows corrections
to atomic-save ordering and publication of the confirmed replacement identities.
A separate [read-only observer](docs/benchmarks/icloud-calc-trash-original-2026-10-06.json)
also verified the exact original bytes in iCloud Trash.
Bounded native archive replacement and recoverable removal are now validated;
three full-integration acceptance areas remain open, and installed delivery stays on HOLD.
A [fresh standalone removal trial](docs/benchmarks/icloud-native-trash-fresh-receipt-loss-2026-10-06.json)
recovered a controlled process exit without repeating the mutation, verified the
complete original content in Trash and passed normal read-only absence checks.
A separate [mismatched-selection trial](docs/benchmarks/icloud-native-replacement-mismatched-selection-fresh-2026-10-06.json)
refused replacement with a deliberately wrong revision and independently confirmed
the original unchanged. A separate [competing-name trial](docs/benchmarks/icloud-native-pre-trash-recovery-name-occupant-fresh-2026-10-07.json)
refused a real conflict and independently verified all three documents unchanged.
Naturally stale revisions, races after the final preflight, broader native editing
and release acceptance remain open. Earlier uncertain and partial
outcomes remain in the [validation record](docs/icloud-write-integration.md).

Google Docs and Sheets are presented as **read-only export folders**, with selected
DOCX/PDF/ODT and XLSX/PDF/ODS exports. Editing those exports does not write back to
Google's native documents. OneNote and other provider packages also retain their
package-specific write restrictions. See [Google Drive](docs/google-drive.md)
for the precise preview scope.

Strata's [isolated companion GUI checks](docs/strata.md) passed for availability
badges, offline actions, inherited pins and native-document restrictions.
A [fresh private-host trial](docs/benchmarks/icloud-strata-private-host-dialog-2026-10-06.json)
also passed the real normal read-only iCloud catalog, native Show availability
menu and exact filename/On demand dialog, with one actual indexed archive child.
It preserved installed artifacts and settings. A [fresh real Numbers DATA trial](docs/benchmarks/icloud-strata-numbers-data-pin-2026-10-08-bad238ab-7129-47c4-b7a5-b331219f41d7.json)
also passed one Keep/Stop cycle from an initially uncached file, the kept badge,
availability dialog and menu/badge refresh. Its independent audit passed 33/33;
the fetching description was observed through accessibility. This used a private
host test environment. A separate [inherited folder-pin trial](docs/benchmarks/icloud-strata-numbers-data-inherited-pin-2026-10-08-10793800-3517-45a5-9b8b-105d2ce0a4ef.json)
now passed parent Keep/Stop, inherited availability, menus and exact readback,
with an independent 35/35 audit. Installed Strata acceptance remains open. Dolphin's broader live
desktop acceptance also remains open. Distribution and upgrade
boundaries are recorded in the [validation record](docs/validation.md).

## Try Cirrove

### Install a release

The [0.1.0 release](https://github.com/Dandiccf/cirrove/releases/tag/v0.1.0)
contains Linux x86_64 packages for **Arch, Ubuntu 24.04 and Fedora**, installation
commands, checksums and package-attestation instructions. Install the core and
desktop packages for your distribution, then open **Cirrove** from the application
menu. This release is OneDrive-focused. To test Google Drive without compiling,
use [0.2.0 Canary 1](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.1),
a prerelease snapshot with the same three package families. `du` can overreport
virtual files' disk allocation in this snapshot; use `cirrove local-data` for
cache/index accounting. Its Google app is limited to approved testers; your own
OAuth app is an alternative. Canary packages retain their development version and commit suffix, and do not imply that the
[stable Google release gates](docs/google-release-gate.json) have closed.
The [user guide](docs/user-guide.md) explains connections, offline pins,
file-manager extras and recovery.

### Build and try without a cloud account

Requirements: Linux, the pinned Rust toolchain (currently **1.98.1**), a C/C++
build toolchain, CMake and pkg-config. The desktop also needs **GTK 4.14+** and
**libadwaita 1.5+** development packages. SQLite is bundled and HTTPS uses Rustls.

```sh
git clone https://github.com/Dandiccf/cirrove.git
cd cirrove
cargo build --workspace --locked

# Synthetic metadata demo: no sign-in or cloud requests.
./target/debug/cirrove demo --state-dir "$(mktemp -d)"
```

For the service and CLI without GTK, use `cargo build --locked`. Mounting requires
`/dev/fuse`, `fusermount3` and a kernel supporting `FUSE_DIRECT_IO_ALLOW_MMAP`;
the FUSE control filesystem must allow the mount owner to open its `abort`
control. Browser sign-in requires `xdg-open` and a desktop Secret Service keyring.
The optional Dolphin plugins additionally need Qt 6, KIO 6 and KI18n 6.
See [Development](docs/development.md) and [Desktop](docs/desktop.md) for setup.

### Install a developer build and connect

For a machine following `main`, use the supported developer installer:

```sh
scripts/install-developer.sh
cirrove keyring-check
cirrove-desktop
```

The installer builds and installs into your home, then starts or restarts the
user service and tray. Use either release packages or a developer install;
[do not mix the two](docs/development.md#the-two-ways-to-have-cirrove-installed).
Follow [Connecting your own accounts](#connecting-your-own-accounts) below for
the required registration and browser sign-in. `cirrove status` and
`cirrove accounts` inspect the local service and configured connections.

Account metadata defaults to `~/.local/state/cirrove`; sign-in credentials stay
in your desktop keyring. Separate state and socket options support isolated
experiments. Existing cloud clients and their configuration are not imported.

### Test and help validate

```sh
scripts/check.sh
```

This runs formatting, Clippy, workspace and script tests, the acceptance ledger,
and synthetic FUSE scenarios when `/dev/fuse` and `fusermount3` are available.
The tests use fake providers and local HTTP fixtures; they do not sign in to your
cloud account. Desktop-window scenarios need a display and are run separately,
as described in [Development](docs/development.md#local-checks).

Real-provider testing is a separate step. Start with a read-only connection and
a folder of disposable, personally owned test files. Write validation should
have an explicit scope and verify contents independently; do not run mutation
fixtures against irreplaceable data. For a useful report, follow
[Contributing](CONTRIBUTING.md): include the version, distribution, desktop,
expected behavior and reproducible steps, with credentials and private file or
account details removed.

## Connecting your own accounts

You can try Cirrove with your own accounts before wider onboarding is available.
Use the [0.2.0 Canary 1 packages](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.1)
for the newer OneDrive and Google Drive previews, or build `main` following
[the development installation guide](docs/development.md). Canary is an opt-in
testing snapshot, with no automatic nightly/update channel. The 0.1.0 release
remains OneDrive-focused. See [the user guide](docs/user-guide.md#connecting-a-drive)
for the connection window.

- **OneDrive:** create your own Microsoft Entra desktop app registration and
  enter its application ID when connecting. Cirrove does not yet bundle a shared
  Microsoft registration. The [OneDrive setup guide](docs/onedrive-setup.md)
  covers registration and browser sign-in; the
  [personal Microsoft account guide](docs/microsoft-developer-setup.md) covers
  creating a development directory when you do not already have one.
- **Google Drive:** builds with Cirrove's Google app can sign in accounts on its
  approved tester list. For independent private testing, create your own Google
  Cloud project, enable the Drive API, add your account as a test user, and
  download a **Desktop app** OAuth client JSON. Choose **Use another Google OAuth
  app** in the window, or pass `--client-json` to the CLI. Keep that JSON private
  with mode `0600`. The [Google sign-in guide](docs/google-drive.md#google-sign-in-and-optional-custom-app)
  gives the complete steps and explains Testing-mode grant expiry.

For a first read-only connection, choose a separate empty mount folder and use
the window with **Allow changes** off, or the CLI:

```sh
cirrove connect --label my-onedrive \
  --client-id YOUR_MICROSOFT_APPLICATION_ID \
  --mount-path "$HOME/Cloud/Cirrove-MyOneDrive"

cirrove connect-google --label my-google \
  --client-json /absolute/path/to/your-desktop-client.json \
  --mount-path "$HOME/Cloud/Cirrove-MyGoogle"
```

Each account signs in through its provider's browser flow. These commands use
your own registration; they do not import credentials from another cloud client.
For writable preview setup and Shared Drive selection, follow the provider guide.
iCloud remains a development integration rather than a general onboarding route.

## iCloud integration progress

**Development update, 8 October 2026:** iCloud has moved beyond a feasibility
study. Cirrove has its own native adapter using Apple's undocumented web
transport, with no rclone or Stratosync runtime, configuration or credential
import. Live checks have covered sign-in, directory browsing, on-demand and
ranged reads, saved-session restart, and detection of a remote content change.
New iCloud connections default to read-only.

The current application boundary is:

| Format | Bounded workflow verified | Remaining limit |
|---|---|---|
| Pages / Numbers / Keynote **PACKAGE** | Archive replacement, original in Trash, normal read-only remount and Apple reopen | Native iWork editing and saving in Linux applications are unsupported by the tested filters. |
| Pages **DATA** | Ordinary and atomic saves, exact bytes and Trash, normal read-only remount, exact Apple text after reload | One genuine export pair; broader fidelity and native Linux Pages editing remain untested. |
| Keynote **DATA** | Ordinary and atomic saves, exact bytes and Trash, normal read-only remount, expected slide title and subtitle after Apple reload | One genuine export pair; broader presentation fidelity and native Linux Keynote editing remain untested. |
| Numbers **DATA** | Ordinary and atomic saves, exact new bytes and original in Trash, normal read-only remount, Apple open/reload with values and SUM preserved | One genuine export pair; the earlier Apple refusal remains recorded and unexplained. |
| DOCX / Writer | Real application create/edit/save/reopen through an isolated mount, byte readback and original in Trash | One paragraph; layout, fonts and media are untested. |
| XLSX / Calc | Real application saves/reopens with SUM preserved, byte readback and original in Trash; Apple's Excel export preserved values and SUM on a read-only Calc open | Two owned direct Numbers imports flatten SUM to a cached value. Use Apple's Excel export for the tested formula-preserving route; general spreadsheet fidelity is untested. |
| PPTX / Impress | Real application create/edit/save/reopen, byte readback and original in Trash | One slide's title and subtitle; layout, media and animation are untested. |

All twelve registered iWork archive-copy workflows now have complete bounded
endpoints: Pages, Numbers and Keynote, DATA and PACKAGE representations, and
ordinary and atomic saves. These workflows remain distinct from full iCloud
release acceptance.

The [documented iWork application criterion](docs/benchmarks/icloud-iwork-application-acceptance-2026-10-08.json)
is now complete, including the supported DOCX, PPTX and Apple Excel export-copy
checks with their explicit content limits. Installed account/recovery behavior,
complete desktop delivery and reliability acceptance remain open.

The final [ordinary Keynote PACKAGE save](docs/benchmarks/icloud-keynote-package-ordinary-a0a06783-4280-4a2c-b34c-d94b737df23e-2026-10-08.json)
preserved the original in Trash and passed a separate
[normal read-only remount](docs/benchmarks/icloud-keynote-package-ordinary-normal-readonly-2026-10-08-4c0b9e76-a9c0-4267-977f-af372b8e2cb8.json)
and [Apple Keynote reopening](docs/benchmarks/icloud-keynote-package-ordinary-exact-apple-reopen-2026-10-08-d976ad1d-71ac-4374-8f6e-c63013c409db.json).
Independent audits passed 39, 35 and 33 checks. The exact title, subtitle and
single slide remained visible after one reload, without content edits.

A [fresh offline Calc check](docs/benchmarks/icloud-numbers-data-calc-formula-export-2026-10-08-5ef91b93-ac46-40f4-80cd-bf5a07d49d7e.json)
reproduced the native Numbers import limit with the current genuine fixture.
Values 7, 3 and 10 survived, but Calc had already replaced SUM with the constant
10 before exporting XLSX. The native source stayed unchanged. This path does not
preserve editable formulas. A separate
[Apple Excel export](docs/benchmarks/icloud-numbers-apple-excel-export-2026-10-08-4cfbc759-df14-4a24-a84d-9e1614eb3146.json)
and [read-only Calc open](docs/benchmarks/icloud-numbers-apple-xlsx-readonly-calc-2026-10-08-d1a23b21-9996-4c3d-b6de-840ec776b7cc.json)
preserved the same values, SUM formula and source marker. Apple adds a table-title
row, so native C2 becomes Excel C3. This validates the tested export-copy route;
it does not establish native Numbers editing in Calc or general spreadsheet fidelity.

A fresh [Numbers DATA atomic writer](docs/benchmarks/icloud-numbers-data-atomic-2026-10-08-de0f154b-d2de-4daa-bce3-c734dd311ca2.json)
has passed exact new-version and original-in-Trash byte checks, a separate
[normal read-only remount](docs/benchmarks/icloud-numbers-data-atomic-normal-readonly-remount-2026-10-08-b27a5609-cc26-4247-bdae-cc36bd1893c7.json)
and [Apple Numbers reopening](docs/benchmarks/icloud-numbers-data-atomic-exact-apple-reopen-2026-10-08-32f9269d-8713-463f-bf1d-b2834ca17cc9.json).
Values 7, 3 and 10, the SUM formula and the source marker remained visible after
one reload. Independent writer, remount and Apple audits passed 36, 33 and 33
checks. The first remount's strict timestamp-comparison failure is retained;
the corrected, separately registered remount preserves journal bytes and logical
state while recording SQLite's changes to the cloned SHM timestamps.

A fresh [Keynote DATA atomic writer](docs/benchmarks/icloud-keynote-data-atomic-2026-10-08-01da7cef-1407-4830-9f76-08c498a11a76.json)
also passed exact replacement and original-in-Trash checks, a separate
[normal read-only remount](docs/benchmarks/icloud-keynote-data-atomic-normal-readonly-remount-2026-10-08-a4f0ef41-022e-4a13-ab81-14dc54aa4dcf.json)
and [Apple Keynote reopening](docs/benchmarks/icloud-keynote-data-atomic-exact-apple-reopen-2026-10-08-a55c81d2-bc01-4597-a708-7ca8f05f05ed.json).
The expected title, subtitle and one slide remained visible after one reload,
without content edits. Independent audits passed 36, 33 and 33 checks. This
completes one bounded archive-copy workflow; broader native editing and full
iCloud acceptance remain open.

A fresh [Pages DATA atomic writer](docs/benchmarks/icloud-pages-data-atomic-2026-10-08-ffe0da39-0a83-42cc-b165-2910defae614.json)
passed exact replacement and original-in-Trash checks, a separate
[normal read-only remount](docs/benchmarks/icloud-pages-data-atomic-normal-readonly-remount-2026-10-08-f3b7b6d2-927b-4982-9223-4aeb5d878110.json)
and [Apple Pages reopening](docs/benchmarks/icloud-pages-data-atomic-exact-apple-reopen-2026-10-08-18ba4dbb-fe8f-4c98-9ce6-5ded7b17076f.json).
The complete known paragraph matched before and after one reload, without
content edits. Independent audits passed 36, 33 and 33 checks. This completes
one bounded archive-copy workflow; full iCloud acceptance remains open.

A fresh [Pages PACKAGE ordinary writer](docs/benchmarks/icloud-pages-package-ordinary-8bbbb835-e4ea-460c-b6f5-bbbfba5d79b9-2026-10-08.json)
also passed a canonical archive overwrite, original preservation in Trash, a
[normal read-only remount](docs/benchmarks/icloud-pages-package-ordinary-normal-readonly-2026-10-08-b6d2dbdb-7691-4113-aa9d-de69668d38b4.json)
and [Apple Pages opening and reload](docs/benchmarks/icloud-pages-package-ordinary-exact-apple-reopen-2026-10-08-192eaf1f-655e-4cc3-ae8d-e553d6892f81.json).
Independent audits passed 39/39, 35/35 and 33/33. The full known paragraph matched
before and after reload. This establishes one archive-copy workflow; broader
fidelity, native Linux Pages editing and installed acceptance remain open.

An earlier [Pages DATA atomic trial](docs/benchmarks/icloud-pages-data-atomic-2026-10-08-f3157aef-67b6-4542-a933-2c165e27ab77.json)
stopped after local replacement because of a test-controller error. The narrow
controller correction passed its controls. A separate fresh read confirmed the
original A bytes and temporary B bytes; the full cloud outcome, including Trash,
remains unresolved. The failed operation has not been replayed and does not
count as a completed workflow.

New [Pages](docs/benchmarks/icloud-pages-package-atomic-d34f610e-e05d-4e1c-8341-098c6dd935f2-terminal-audit-2026-10-08.json)
and [Keynote](docs/benchmarks/icloud-keynote-package-atomic-ff5abf15-0264-46a1-9efd-94a70ae724a7-terminal-audit-2026-10-08.json)
PACKAGE atomic-save trials have each passed one temporary-file write, fsync and
rename through the mounted canonical archive. Independent iCloud reads verified
the new content and the original in Trash; each audit passed 34/34.
Pages also passed a [fresh normal read-only remount](docs/benchmarks/icloud-pages-package-atomic-normal-readonly-2026-10-08-9f7027c9-afa1-44bf-aefc-c3bb3bd62cac.json)
(audit 32/32) and [Apple Pages reopening](docs/benchmarks/icloud-pages-package-atomic-exact-apple-reopen-2026-10-08-238dd615-d388-423c-aa16-6ae706335bdf.json),
with the complete known paragraph preserved after one reload (audit 25/25).
Keynote also passed a [fresh normal read-only remount](docs/benchmarks/icloud-keynote-package-atomic-normal-readonly-2026-10-08-184da3f0-452f-44df-917f-f442913a048c.json)
(audit 32/32) and [Apple Keynote reopening](docs/benchmarks/icloud-keynote-package-atomic-exact-apple-reopen-2026-10-08-d26b6cbe-d96b-45ab-9c31-ab6d7a5ef611.json),
with its genuine one-slide title and subtitle visually preserved after one
reload (audit 26/26). These are archive replacements, with no native Linux
Pages or Keynote editing claim. Full-iCloud acceptance and installed delivery
remain open and on HOLD.

The dated results below distinguish archive-copy saves, actual Linux application
saves and conversions. These development checks do not constitute a full iCloud
release or installed acceptance.

The latest development work extends Cirrove's shared journal and filesystem to
explicit ordinary-file writes, conditional changes, recoverable Trash and
interrupted-save recovery. Scoped application checks and large-file create and
replacement checks have passed on development-owned fixtures. Native
Pages/Numbers/Keynote support is also in progress: package reading, import,
replacement and local-save recovery paths are implemented in the development
tree. Selected Pages checks include independent content verification and Apple
reopen. An owned Numbers package test imported one document and saved a new
canonical archive through the normal FUSE mount. Independent readers verified
the new content and original in Trash; a held reader retained the original, and
a read-only remount read the new version without replay. Apple Numbers displayed
the expected replacement values and formula. The
[registered result](docs/benchmarks/icloud-numbers-fuse-save-confirmation-2026-10-04.json)
records the precise scope and retained evidence.

A [new Numbers PACKAGE atomic-save trial](docs/benchmarks/icloud-numbers-package-atomic-save-2026-10-07-33711731-75c5-4d9e-bc26-aa385cab9748.json)
completed one temporary-file write, flush and rename through FUSE, preserving a
held reader of the original. Its controller stopped at a local verification
error after the upload completed. A [separate read-only verification](docs/benchmarks/icloud-numbers-atomic-postflight-read-2026-10-07-60fc4036-f746-4f74-a796-052cd70d276a.json)
confirmed the complete replacement content and exact original in Trash without
repeating the write. A [fresh normal read-only remount](docs/benchmarks/icloud-numbers-atomic-normal-readonly-remount-2026-10-07-b6ce3c1e-f8e7-4dfd-8a2f-b2d46b66150a.json)
now verified the complete replacement again. [Apple Numbers reopening](docs/benchmarks/icloud-numbers-atomic-exact-apple-reopen-2026-10-07.json)
matched the exact current document ID and displayed 7, 3, 10 and `SUM(A2:B2)`.
The original controller failure remains recorded. This is one bounded
archive-copy atomic save; native Linux editor fidelity and the broader
application matrix remain open.

The [subsequent Numbers DATA trial](docs/benchmarks/icloud-numbers-data-revision-confirmation-2026-10-04.json)
created a genuine Apple export as an ordinary file through FUSE. An independent
reader confirmed its actual DATA representation and exact original bytes before
one in-place save. Further independent reads verified the exact replacement
bytes and the original in Trash. This was a file copy and overwrite; it did not
exercise a real editor or Apple Numbers reopen. A [separate normal read-only
remount](docs/benchmarks/icloud-numbers-data-readonly-remount-2026-10-07-8d791c0c-5902-44e6-b184-d39a2ad5935e.json)
matched the exact current B revision and all 138,943 bytes in 41.56 seconds;
its independent audit passed 49/49. A [separate exact-ID Apple Numbers open](docs/benchmarks/icloud-numbers-data-apple-reopen-2026-10-07-d6466c5a-d307-4552-90a1-887a91ff9300.json)
then failed with “This spreadsheet can’t be opened right now.” Its cause remains
unproved; no values, formula or reload were observed, and there was no retry.
Independent audit 22/22 verified the retained refusal and owned-tab closure.

A [fresh Numbers DATA trial on the corrected candidate](docs/benchmarks/icloud-numbers-data-corrected-candidate-2026-10-08-8bbfa670-42ac-4f04-91d1-a6cbd6afc018.json)
now passed one new create and one overwrite, independently matching all 138,943
replacement bytes and all 138,881 original bytes in Trash. Its independent audit
passed 36/36. A [new normal read-only remount](docs/benchmarks/icloud-numbers-data-normal-readonly-remount-2026-10-08-68e37925-ca6f-483b-b62c-1db7450bf98a.json)
also returned the exact replacement bytes and revision. A [new exact-item Apple Numbers observation](docs/benchmarks/icloud-numbers-data-exact-apple-reopen-2026-10-08-8bb-d9fd412d-02f8-4c91-ac5c-ab9a952af52a.json)
now passed opening and one reload with values 7/3/10, `SUM(A2:B2)` and the genuine
source marker intact; its independent audit passed 26/26. The earlier refusal
remains recorded. This bounded result does not establish its cause, native Linux
Numbers editing or broader spreadsheet fidelity.

A fresh Numbers browser trial also confirmed that an edit and its formula
survived closing and reopening in Apple's editor, and produced a native export.
The trial stopped at an export-format verification gap. A
[subsequent offline fix](docs/benchmarks/icloud-flat-native-source-proof-2026-10-05.json)
now verifies the unchanged native export; independent readback of the edited
version and its remount still need acceptance. The
[browser trial record](docs/benchmarks/icloud-numbers-browser-editor-2026-10-05.json)
separates those completed observations from the remaining checks. This browser
workflow does not establish saves from a Linux editor through the mount.

A later fresh browser trial again confirmed the saved Numbers values and formula
after reloading the document. Its native export could not be acquired, so the
trial stopped with partial evidence. A [separate read-only observation](docs/benchmarks/icloud-retained-numbers-read-2026-10-05.json)
has now verified changed provider content through a direct read, a separate
reader and a fresh mount, with matching content-tree identities. This does not
by itself decode the displayed cell values or prove browser export fidelity.
A [subsequent independent local importer](docs/benchmarks/icloud-numbers-independent-decoder-2026-10-05.json)
read the expected values 11/3/14 from that actual Numbers archive, including its
unmodified wrapped layout. It did not expose the formula, so formula and local
editor save fidelity remain unproved by that importer.
The [browser trial record](docs/benchmarks/icloud-numbers-browser-editor-confirmation-completion-2026-10-05.json)
keeps that export gap explicit.

A [fresh mounted Calc trial](docs/benchmarks/icloud-calc-after-metadata-publication-2026-10-06.json)
now confirms two actual XLSX editor saves and local reopening with the SUM
formula preserved: 7/3/SUM=10, then 11/3/SUM=14. Independent iCloud DATA reads
matched each saved version, and a normal read-only remount returned the exact
second version. A separate [read-only Trash observation](docs/benchmarks/icloud-calc-trash-original-2026-10-06.json)
verified the exact original bytes. All uploads and temporary-file cleanup
completed. The
[earlier partial mounted trials](docs/benchmarks/icloud-calc-mounted-editor-diagnostic-2026-10-05.json)
remain retained; their uncertain saves were not replayed to obtain this result.

The [separate local import trial](docs/benchmarks/icloud-calc-editor-2026-10-05.json)
also exposed a native Numbers limit: LibreOffice Calc opened the captured Numbers
file with correct values, but imported its SUM result as a number rather than a
formula. Native Numbers editing in Calc is therefore not a validated
formula-preserving workflow; see the
[format limits](docs/compatibility.md#icloud-development-and-linux-editor-formats).
A [fresh offline Numbers-to-XLSX conversion](docs/benchmarks/icloud-numbers-calc-genuine-export-2026-10-06.json)
then imported an Apple-edited Numbers export, created one XLSX copy and reopened
it with values 17, 3 and 20 preserved. Calc imported the SUM result as the number
20, so the conversion completed but did not preserve the formula. This automated
trial preserved the original source; interactive editing and saving back to
native Numbers remain unvalidated.

The public Desktop Pages import interface now passes a separate
[local chooser test](docs/benchmarks/icloud-pages-chooser-visible-tree-2026-10-06.json):
selecting a real archive, checking its confirmation and cancelling without
submitting an import. A [subsequent local test](docs/benchmarks/icloud-pages-local-chooser-focus-fields-2026-10-06.json)
also passed exact entry and readback of all three import confirmation fields.
A [fresh Desktop import](docs/benchmarks/icloud-pages-live-desktop-import-owned-focus-2026-10-06.json)
has now submitted one owned Pages document, completed its public job and made
the document readable on the original mount. An independent iCloud reader
verified its content tree, and [Apple Pages opened that exact new document](docs/benchmarks/icloud-pages-desktop-apple-open-2026-10-06.json)
with the expected test text. The controller subsequently refused to read the
successful observer's evidence because of a file-mode mismatch; the retained
result and separate offline audit are recorded without repeating the import.
A [separate fresh CLI arm](docs/benchmarks/icloud-pages-fresh-cli-import-2026-10-06.json)
then passed public completion, original-mount capture, independent content
verification and Apple Pages open of its own new document. These two complete
arms close the public Pages import criterion. Native editing and full release
acceptance remain open.

The development branch now provides an explicit source format for genuine
Apple Numbers exports without an outer document folder. Import and replacement
use `--source-layout flat-numbers`, with no `--source-root`; wrapped archives
keep their existing contract. The sealed source stays unchanged. Flat inputs
now use a separately derived transport archive with the destination-name wrapper;
its complete content must match the source's semantic V2 identity. [The validation record](docs/benchmarks/icloud-flat-numbers-normal-source-2026-10-06.json)
separates local and synthetic checks from live acceptance. A [fresh real-account
import](docs/benchmarks/icloud-flat-numbers-fresh-public-cli-import-2026-10-06.json)
stopped with a conflict: independent readback found changed internal paths and
missing preview files. A [subsequent transport correction](docs/benchmarks/icloud-flat-numbers-wire-envelope-2026-10-06.json)
passes the preservation regression and an offline check with the actual Apple
export. A [separately registered fresh import](docs/benchmarks/icloud-flat-numbers-wire-fresh-public-cli-import-2026-10-06.json)
then passed through the normal CLI: original-mount and independent iCloud
readback preserved all 42 files, including the three previews. Apple Numbers
opened that exact new document with values 11/3/14 and its `SUM(A2:B2)` formula.
The [earlier replacement trial](docs/benchmarks/icloud-flat-numbers-genuine-editor-replacement-2026-10-06.json)
stopped at read-only preflight, before editing or changing any cloud document.

A [fresh browser donor](docs/benchmarks/icloud-numbers-edited-b-donor-2026-10-06.json)
has now supplied a genuine edited Numbers export: 17/3/SUM=20 after reload,
with independently decoded cached values. A [separate fresh replacement and
recovery trial](docs/benchmarks/icloud-flat-numbers-native-final-2026-10-06.json)
imported its own protected original, replaced it with that export and deliberately
exited before returning the final local confirmation. The same operation recovered
with one provider inspection and no mutation replay. Independent reads verified
the complete new content tree and the original in Trash. Apple Numbers then opened
the exact new document and retained 17/3/20 and `SUM(A2:B2)` after reload, without
a repair dialog. A [separate read-only remount](docs/benchmarks/icloud-flat-numbers-native-final-ro-remount-2026-10-06.json)
started but stopped before archive capture completed. Its stopped local catalog
still selects the old item at the original name despite the confirmed move to
Trash. A [local correction](docs/benchmarks/icloud-native-package-backup-publication-2026-10-06.json)
now passes the normal-completion, reopened-journal and read-only-startup regressions;
a fresh read-only startup also repaired the completed live history and selected
only new B with original A in Trash. Its [capture trial](docs/benchmarks/icloud-native-package-readonly-backfill-2026-10-06.json)
then refused a newer iCloud revision, preserving the exact reference check.
A [fresh separately registered replacement](docs/benchmarks/icloud-native-final-remount-before-apple-corrected-2026-10-06.json)
subsequently passed the same recovery and independent current/Trash checks. Its
[normal read-only remount](docs/benchmarks/icloud-native-package-readonly-remount-before-apple-2026-10-06.json)
then preserved the complete new content tree, including all 42 files and the
three previews, before any Apple application opening. The earlier failed arms
remain recorded. [Apple Numbers subsequently opened this new document](docs/benchmarks/icloud-native-final-apple-after-remount-2026-10-06.json)
and retained 17/3/20 and `SUM(A2:B2)` after reload, with no cell edits or repair
dialog. This closes that bounded semantic remount check; it does not
establish native Linux editor saves, all iWork
formats or sustained reliability.

Local validation also exposed two standalone native-removal recovery gaps:
read-only startup did not finish pending metadata absence, and a historical
observer required a writer. The [development correction](docs/benchmarks/icloud-native-trash-readonly-publication-2026-10-06.json)
passes the actual startup regressions and 47 targeted controls, preserving
retained saves and later restores. The separate
[fresh real-iCloud removal trial](docs/benchmarks/icloud-native-trash-fresh-receipt-loss-2026-10-06.json)
also passed recovery after process loss, independent Trash-content verification
and normal read-only absence checks. This single bounded arm does not establish
broader conflict handling, sustained reliability or installed acceptance.

A [genuinely edited Keynote presentation](docs/benchmarks/icloud-keynote-genuine-replacement-2026-10-07-3d901c2b.json)
also passed normal public CLI replacement. Independent full semantic V2 reads
verified current B and exact original A in Trash. A separate
[normal read-only remount](docs/benchmarks/icloud-keynote-readonly-remount-2026-10-07-f9923c49.json)
returned the complete B content tree before Apple opening.
[Apple Keynote then opened that exact receipt-bound item](docs/benchmarks/icloud-keynote-apple-reopen-2026-10-07-a0c382a4.json)
and retained its one-slide B title and subtitle after one reload, with no edits.
This completes that bounded PACKAGE workflow; wider format and editor fidelity
remain unproved.

An earlier [Keynote DATA trial](docs/benchmarks/icloud-keynote-data-ordinary-save-2026-10-07-672e98c3-39b6-45a1-85bd-16cc77a6404b.json)
exposed stale metadata after a save, and Apple refused its exact replacement.
The [metadata correction](docs/benchmarks/icloud-ordinary-completed-current-convergence-2026-10-07.json)
and [binary staging correction](docs/benchmarks/icloud-ordinary-replacement-binary-mime-2026-10-07.json)
passed local regressions and the complete project check. A
[new owned Keynote DATA trial](docs/benchmarks/icloud-keynote-data-mime-followup-2026-10-07-2b4c7986-df74-4332-b24f-9d6bfd435fcd.json)
then created A and overwrote it once with a genuine edited Apple export B.
Independent reads verified every byte of B and the original A in Trash; a
[normal read-only remount](docs/benchmarks/icloud-keynote-data-ro-remount-2026-10-07-edba5896-a07e-4cf8-98a7-d52735c5f3d1.json)
returned the exact B bytes. [Apple Keynote opened this new replacement](docs/benchmarks/icloud-keynote-data-exact-apple-reopen-2026-10-07-2b4-8653574f-c244-47b2-9284-99828cb9ecbf.json)
with its one-slide B title and subtitle intact after one reload. Independent
writer, remount and Apple-observation audits passed 36, 29 and 26 checks.
This completes one bounded DATA archive-copy workflow; native Linux Keynote
saving and broader format fidelity remain unproved. Earlier failures are retained;
this successful new case does not establish their Apple refusal cause.

A [genuine Pages replacement](docs/benchmarks/icloud-pages-direct-flat-replacement-2026-10-07-ab8555d8.json)
also accepted the unchanged Apple export without an outer document folder.
Independent full content checks verified the new document and its original in
Trash. A [fresh read-only remount](docs/benchmarks/icloud-pages-direct-flat-readonly-remount-2026-10-07-805f4b1e.json)
returned the complete new content; [Apple Pages reopened the exact replacement](docs/benchmarks/icloud-pages-direct-flat-apple-reopen-2026-10-07-3a054fad.json)
with its genuine 93-byte paragraph unchanged after one reload. This validates
explicit `flat-pages` archive input, not native Pages editing in a Linux editor.
Earlier Writer trials could not load that export; a
[service probe](docs/benchmarks/icloud-pages-uno-filter-probe-2026-10-07-697c1f07-ba06-40c4-a420-e3b7340c3177.json)
stopped at filter construction on a host missing `libwpg`. A
[verified private library capsule](docs/benchmarks/icloud-pages-uno-private-library-2026-10-07-8e73eb0d-fe0e-497b-9a81-ad266a96d4d5.json)
restored service activation and type detection. A
[fresh offline Writer conversion](docs/benchmarks/icloud-pages-writer-private-library-2026-10-07-2f2fec37-6b7c-40aa-b8db-49fa98d5192c.json)
then imported the genuine Pages export read-only, produced one DOCX and reopened
it read-only with the exact 93-byte paragraph preserved. Independent OOXML checks
agreed. This is text-only conversion evidence; fonts, layout, media and native
Pages saving remain unproved. The host dependency remains absent: no system
package repair or installation was performed.
A [fresh mounted Writer trial](docs/benchmarks/icloud-writer-mounted-docx-2026-10-07-f015bd4d-44a1-4676-88ca-a0a0445ed9ab.json)
then created and edited an ordinary DOCX through an isolated iCloud mount.
Independent iCloud reads matched both saved versions, a normal read-only remount
returned the second version exactly, and the original was verified in Trash.
All 29 independent checks passed. This validates the bounded DOCX workflow;
native Pages saving, document layout and installed acceptance remain open.
A [fresh offline Impress conversion](docs/benchmarks/icloud-keynote-impress-pptx-2026-10-07-20d21b4e-a918-41db-b7f5-e1a0fb3c593c.json)
likewise preserved the exact one-slide title and subtitle through one PPTX export
and read-only reopen, with 28 independent checks passed. This does not establish
layout, fonts, media, animations or native Keynote saving.
A [fresh mounted Impress trial](docs/benchmarks/icloud-impress-mounted-pptx-2026-10-07-85ee1211-763d-46b7-b7a7-2886e6867e0d.json)
then created and edited an ordinary PPTX through an isolated iCloud mount.
Independent DATA reads matched both versions, a normal read-only remount returned
the second version exactly, and the original was verified in Trash. All 24
independent checks passed. This validates one slide's title and subtitle;
native Keynote saving and broader presentation fidelity remain open.

A [fresh native Pages DATA trial](docs/benchmarks/icloud-pages-data-create-replace-2026-10-07-e5ed45e4-02cd-45e3-8ea1-20568c3e6a3a.json)
created one genuine Apple-exported Pages file through the ordinary iCloud mount,
independently verified it as DATA, then overwrote it once with the edited export.
Independent readbacks matched the complete new version and the exact original
in Trash. All 53 retained-evidence checks passed, without a retry. A
[separate normal read-only remount](docs/benchmarks/icloud-pages-data-readonly-remount-2026-10-07-e751e06a-51e0-48be-b964-f510b3e78f1b.json)
returned the exact B identity and all 101,769 bytes; its independent audit passed
37/37. A [separate exact-ID Apple open](docs/benchmarks/icloud-pages-data-apple-reopen-2026-10-07-df50f6ee-f23c-4229-860d-0c209fe78821.json)
then failed with “This document can’t be opened right now.” Its independent audit
passed 28/28 for the retained failure and closure; the cause remains unresolved.
There was no reload or retry. The successful PACKAGE reopen above is a separate
case. Raw DATA byte parity does not establish Apple editor admission or native
Linux Pages saving.

A [new Pages DATA trial on the corrected candidate](docs/benchmarks/icloud-pages-data-corrected-candidate-2026-10-07-9fa551d8-c617-4ae7-ac5c-d4d5360bafb9.json)
then passed one ordinary create and overwrite, independently verifying the exact
101,769-byte replacement and 100,824-byte original in Trash. A
[separate normal read-only remount](docs/benchmarks/icloud-pages-data-normal-readonly-remount-2026-10-08-6231f854-e2dd-4a83-a018-38526ed315ca.json)
returned the exact new bytes in 8.62 seconds. [Apple Pages opened this new item](docs/benchmarks/icloud-pages-data-exact-apple-reopen-2026-10-08-9fa-4e7c87d1-0bba-4983-8362-0a61f8269683.json)
with the exact genuine 93-byte paragraph unchanged after one reload; no content
was edited. Independent writer, remount and Apple-observation audits passed
36, 29 and 28 checks. This completes one bounded Pages DATA archive-copy workflow, while
the old failure and its unresolved cause remain recorded. Native Linux Pages
saving, layout, fonts and media fidelity remain unsupported or untested.

An [isolated saved-session connection](docs/benchmarks/icloud-saved-session-real-lifecycle-2026-10-07-ebbd9f71.json)
completed but stopped before read-only startup became ready. A
[separately registered follow-up](docs/benchmarks/icloud-saved-session-existing-account-2026-10-07-b8483086.json)
reused that connected account without connecting again. It preserved sealed and
dirty edits through disabled write opt-in, same-account reauthentication and
read-only downgrade, then passed normal read-only mounts and exact local exports.
No writable daemon was started. Fresh password/2FA, expired-session recovery and
installed upgrade/downgrade acceptance remain open.

The [CI run for development commit `7230383`](https://github.com/Dandiccf/cirrove/actions/runs/37599833161)
passed all seven jobs; the installed release remains unchanged and on HOLD.
The [dated acceptance assessment](docs/benchmarks/icloud-native-replacement-removal-criterion-closure-2026-10-07.json)
closes the bounded native replacement/removal criterion, including conflict
refusal and recovery after process loss without repeating the cloud mutation.

These are bounded development results. **Full iCloud support is still open:**
the successful Numbers trials confirm bounded CLI replacement and canonical
archive-copy FUSE saves, plus one ordinary DATA create/save. Ordinary and atomic
editor workflows beyond the bounded Calc, Writer and Impress trials, broader Pages/Numbers/Keynote
editing/reopen fidelity, DATA coverage beyond these owned fixtures, long-session
renewal, installed read/write transitions, and sustained account-scale use
still require acceptance. Test the iCloud branch with isolated state and mounts;
[its deployment policy](docs/development.md#flat-numbers-source-journal-schema21-held-prerelease-policy)
requires keeping it separate from the installed release. Read the
[iCloud development record](https://github.com/Dandiccf/cirrove/blob/research/icloud-feasibility/docs/icloud-write-integration.md)
and follow [PR 86](https://github.com/Dandiccf/cirrove/pull/86) for published
implementation and evidence. A passing fixture does not make this a released
or generally reliable iCloud client.

## Upcoming cloud services and community contributions

After completing the current integrations, providers we want to tackle next
include **Dropbox, Nextcloud and Box**. These are future targets; there is no
adapter or release date promised for them. We also welcome proposals for other
cloud services and feedback on which providers matter most to Linux users.

**Help us bring more clouds to Linux.** Contributions to new provider adapters,
authentication flows, synthetic protocol fixtures and scoped real-account
validation are welcome. An adapter can reuse Cirrove's filesystem, metadata
store, cache, offline pins and durable recovery machinery. Start with
[the architecture](docs/architecture.md), [contributor instructions](CONTRIBUTING.md)
and [the roadmap](docs/roadmap.md), then
[open an issue](https://github.com/Dandiccf/cirrove/issues) to discuss the provider
and its identity, change-detection and write-safety requirements. Help with
existing providers, documentation, translations and desktop testing is welcome
too.

## Architecture and contributing

| Crate | Responsibility |
| --- | --- |
| `cirrove-core` | Provider-neutral identity, metadata/read/upload contracts, cancellation and request budgets |
| `cirrove-store` | Transactional metadata, observations, persistent inodes and cache index |
| `cirrove-onedrive` | Microsoft Graph metadata, version-checked reads and conditional/resumable uploads |
| `cirrove-googledrive` | Google Drive v3 reads plus v2 conditional writes for writable My Drive and Shared Drive preview mounts |
| `cirrove-auth` | Microsoft/Google browser authentication, shared keyring and refresh broker |
| `cirrove-icloud` | Experimental native Apple transport, document representations and scoped recovery adapters; development only |
| `cirrove-service` | Daemon, CLI, account workers, FUSE projection and content cache |
| `cirrove-desktop` | Native account overview and asynchronous service controls |

Read [Architecture](docs/architecture.md), [Roadmap](docs/roadmap.md),
[OneDrive 1.0 milestones](docs/product-milestones.md),
[Development](docs/development.md) and [Contributing](CONTRIBUTING.md).
Packaging and update-channel work is tracked in the
[distribution plan](docs/distribution.md). The [compatibility matrix](docs/compatibility.md)
records which desktop and installation combinations have actually been tested.

Provider scope and ongoing iCloud work are summarized in
[Cloud services and current support](#cloud-services-and-current-support)
and [iCloud integration progress](#icloud-integration-progress) above. Detailed
acceptance evidence remains in the linked provider and validation documents.

## License

[Apache License 2.0](LICENSE). Copyright 2026 Cirrove contributors.
