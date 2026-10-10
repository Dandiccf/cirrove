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
**[0.2.0 Canary 2](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.2),
released on 10 October 2026, adds experimental native iCloud Drive support**
alongside the OneDrive and Google Drive previews. Packages for Linux x86_64 on
Arch, Ubuntu 24.04 and Fedora 42 are available with installation commands,
checksums and package-attestation instructions. Canary is an opt-in testing
snapshot; 0.1.0 remains the regular release. The table below describes
implementation and validation status, not a general reliability guarantee.
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
| **Google Drive My Drive** | Read/write preview in `main` and Canary 2 | Browser sign-in, ordinary-file create/edit/rename/move/trash, cache and offline pins, with bounded two-account live checks. The Cirrove OAuth app remains limited to test users pending public-app verification. |
| **Google Workspace Shared Drives** | Read/write preview in `main` and Canary 2 | Explicit drive selection, listings, ordinary-file reads and writes, restart recovery and bounded live checks on an owned Workspace drive. Restricted roles and broader long-session acceptance remain open. |
| **Apple iCloud Drive** | Experimental preview in `main` and [Canary 2](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.2) | Native Linux sign-in, on-demand reads, opt-in ordinary-file saves and scoped iWork archive-copy/recovery workflows have bounded live evidence. See [progress and limits](#icloud-integration-progress). |

Google Docs and Sheets are presented as **read-only export folders**, with selected
DOCX/PDF/ODT and XLSX/PDF/ODS exports. Editing those exports does not write back to
Google's native documents. OneNote and other provider packages also retain their
package-specific write restrictions. See [Google Drive](docs/google-drive.md)
for the precise preview scope.

Dolphin's broader live desktop acceptance remains open. Distribution and upgrade
boundaries are recorded in the [validation record](docs/validation.md).

## Try Cirrove

### Install a release

[Download Canary 2](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.2)
for the published Linux x86_64 packages for **Arch, Ubuntu 24.04 and Fedora 42**, exact
installation commands, checksums and package-attestation instructions. Install
the core and desktop packages for your distribution, then open **Cirrove** from
the application menu. Optional Dolphin integration is packaged separately where
supported.

**0.1.0 is the regular OneDrive-focused release.** Canary snapshots are opt-in
previews. Canary 2 is the first published iCloud snapshot; historical Canary 1
adds Google and does not include iCloud. Choose packages by their release notes;
older downloads do not gain the current source's features. Canary packages retain
their development version and commit suffix; they do not close the
[stable Google gates](docs/google-release-gate.json). Google's app is limited
to approved testers; your own OAuth app is an alternative below.

For existing accounts, read the
[deployment policy](docs/development.md#flat-numbers-source-journal-schema21-held-prerelease-policy)
before changing packages or restarting the service. The iCloud candidate's
retained-state installation hold remains in force. The
[user guide](docs/user-guide.md) covers setup, file-manager extras and recovery.

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
Start with [Canary 2](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.2),
checking its provider scope and installation limits, or build the current source following
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

Microsoft and Google use their provider's browser sign-in. These commands use
your own registration; they do not import another cloud client's credentials.
For writable previews and Shared Drive selection, follow the provider guide.
Experimental iCloud uses Cirrove's native local Apple sign-in. Follow the
[fresh isolated testing guide](docs/user-guide.md#icloud-development-preview)
and retained-state deployment policy; Canary 1 has no iCloud adapter, and a new
package publication does not itself authorize a regular-host upgrade.

## iCloud integration progress

**Released experimental preview, 10 October 2026:** native iCloud support is
available in [Canary 2](https://github.com/Dandiccf/cirrove/releases/tag/v0.2.0-canary.2)
and the current source. All six registered integration criteria are complete within
[their documented test boundaries](docs/benchmarks/icloud-full-integration-acceptance-2026-10-10.json).
This is bounded acceptance, not a general provider reliability guarantee.
The release contains nine packages from the [verified main build](https://github.com/Dandiccf/cirrove/actions/runs/38053590436),
plus checksums and build provenance. See [distribution details](docs/distribution.md#canary-2-published-snapshot).
The retained-state deployment policy remains on HOLD. The
[integration record](docs/icloud-write-integration.md) preserves the measured
scopes, corrections and failed trials.

### What has been tested

- **Native Linux access:** Apple sign-in, browsing, on-demand and ranged reads,
  saved-session restart and remote-change detection. Cirrove uses its own
  credentials and configuration. New connections default to read-only; changes
  require an explicit experimental opt-in.
- **Ordinary files and recovery:** owned-file create/save/replacement,
  relocation and recoverable Trash have bounded live evidence. Exact public
  recovery exports preserve retained local bytes without replaying uncertain
  mutations. Permanent deletion is unsupported.
- **iWork archive copies:** twelve registered Pages, Numbers and Keynote
  DATA/PACKAGE workflows cover ordinary and atomic saves, exact current and
  original-in-Trash checks, normal read-only remount and Apple-editor reopening.
  They do not enable native iWork editing or saving in Linux applications.
- **Installed account and desktop lifecycle:** test-machine trials cover write
  opt-in, same-account reauthentication, read-only downgrade and retained-byte
  exports while preserving other connections. Strata trials cover cached paths,
  direct/inherited pins, branded kept/fetching badges and event refresh.
  Controlled server-rejection recovery passed; natural temporal session expiry
  was not demonstrated.

### Know the boundaries

The bounded Numbers conflict trial and its separate read-only continuation
verified the original in Trash, retained replacement and competing document by
exact identity and content, together with exact local replacement-byte export.
The replacement remained Conflict; no mutation was replayed. This does not
establish successful canonical installation of the replacement or automatic
conflict resolution.

Office-copy evidence is deliberately narrow: one Writer/DOCX paragraph, one
Impress/PPTX title/subtitle, and tested Calc/XLSX values and SUM formula. Native
Numbers imports into Calc flattened SUM to its cached value. Editing an Office
copy does not update the iWork source; arbitrary layout, fonts, media and formula
fidelity are unclaimed.

The [tested samples and limits](docs/icloud-write-integration.md#tested-samples-and-implementation-limits)
separate file-specific results from implementation bounds. Native staging has
four reservations per Engine account runtime and a 64 MiB bound per anonymous
upload/verification file, not a host-wide quota. Abandoned staging retains
recovery bytes and identities, without automatic remote TTL cleanup or Stage
deletion. Historical failures remain visible in the public record.

Use disposable, personally owned files and keep an independent copy of data you
cannot replace. Follow the [isolated development guide](docs/user-guide.md#icloud-development-preview)
and the [deployment policy](docs/development.md#flat-numbers-source-journal-schema21-held-prerelease-policy)
before trying the preview. Green CI, one bounded live result and an installed
test-machine trial each support their stated scope; they do not authorize a
regular-host upgrade or guarantee general provider reliability.

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
| `cirrove-icloud` | Native Apple sessions, iCloud Drive reads and experimental ordinary-file/iWork archive-copy writes and recovery transport |
| `cirrove-auth` | Microsoft/Google browser authentication, native Apple sign-in, shared keyring credentials and refresh broker |
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
