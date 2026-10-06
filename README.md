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
| **Apple iCloud Drive** | Experimental development in [draft PR 86](https://github.com/Dandiccf/cirrove/pull/86) | Native Linux sign-in and read-only mounts have live evidence. Bounded Numbers CLI replacement and canonical archive saves through FUSE independently verified the new version and the original in Trash, including a read-only remount without replay. A separate Numbers DATA trial verified ordinary FUSE creation and one in-place save, with exact new bytes and the original in Trash. One owned Keynote PACKAGE import also passed public completion, mounted access, independent content readback and Apple Keynote open; broader iWork editing and recovery remain under validation. This integration is not yet part of `main` or a release. |

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
Native iWork editing and five full-integration acceptance areas remain open.

Google Docs and Sheets are presented as **read-only export folders**, with selected
DOCX/PDF/ODT and XLSX/PDF/ODS exports. Editing those exports does not write back to
Google's native documents. OneNote and other provider packages also retain their
package-specific write restrictions. See [Google Drive](docs/google-drive.md)
for the precise preview scope.

Dolphin's broader live desktop acceptance remains open. Distribution and upgrade
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

**Development update, 6 October 2026:** iCloud has moved beyond a feasibility
study. Cirrove has its own native adapter using Apple's undocumented web
transport, with no rclone or Stratosync runtime, configuration or credential
import. Live checks have covered sign-in, directory browsing, on-demand and
ranged reads, saved-session restart, and detection of a remote content change.
New iCloud connections default to read-only.

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

The [subsequent Numbers DATA trial](docs/benchmarks/icloud-numbers-data-revision-confirmation-2026-10-04.json)
created a genuine Apple export as an ordinary file through FUSE. An independent
reader confirmed its actual DATA representation and exact original bytes before
one in-place save. Further independent reads verified the exact replacement
bytes and the original in Trash. This was a file copy and overwrite; it did not
exercise a real editor or Apple Numbers reopen.

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
Edited replacement, Trash verification and restart recovery for this flat source
format remain open.

These are bounded development results. **Full iCloud support is still open:**
the successful Numbers trials confirm bounded CLI replacement and canonical
archive-copy FUSE saves, plus one ordinary DATA create/save. Ordinary and atomic
editor workflows beyond the bounded Calc trial, broader Pages/Numbers/Keynote
editing/reopen fidelity, DATA coverage beyond these owned fixtures, session retention
and renewal, installed read/write transitions, and sustained account-scale use
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
