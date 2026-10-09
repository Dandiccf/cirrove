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
| **Apple iCloud Drive** | Experimental development in [draft PR 86](https://github.com/Dandiccf/cirrove/pull/86) | Native Linux sign-in, read-only mounts and owned ordinary-file saves have live evidence. Twelve bounded Pages, Numbers and Keynote workflows cover DATA/PACKAGE documents, ordinary saves and atomic replacement, preserved originals in Trash, remount and reopening in Apple's editors. Export fidelity has documented limits. Installed read-only sign-in, explicit write opt-in and Strata preservation have now been observed. Complete installed recovery/downgrade and broader reliability remain open. This integration is not yet part of `main` or a release; see [the progress and limits](#icloud-integration-progress). |

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

**Experimental development, 9 October 2026:** iCloud is being validated in
[PR 86](https://github.com/Dandiccf/cirrove/pull/86). It is not part of `main`
or a released Cirrove version. **Full iCloud acceptance remains open and
candidate delivery is on HOLD.** New connections default to read-only;
writes require an explicit experimental opt-in. Four of the six full-integration
criteria are closed: public Pages import, native replacement/removal recovery,
application workflows and installed Strata preservation. Installed account
lifecycle and reliability remain open.

Cirrove has its own native Linux iCloud adapter using Apple's undocumented
web transport. It does not import another cloud client's credentials or
configuration. Bounded live trials have covered native sign-in, browsing,
on-demand and ranged reads, saved-session restart, remote-change detection,
and owned-file create/save/replacement, relocation and recoverable Trash.
Permanent deletion is unsupported.

### What has been tested

- **iWork archive-copy workflows:** all twelve registered Pages, Numbers and
  Keynote DATA/PACKAGE workflows completed ordinary or atomic saves, exact
  replacement/original-in-Trash checks, normal read-only remounts and reopening
  in Apple's editors. These use genuine document copies; they do not establish
  native iWork editing or saving in Linux applications.
- **Office export copies:** bounded Writer/DOCX and Impress/PPTX workflows
  preserved one paragraph and one slide's title/subtitle. Calc/XLSX saves and
  Apple's Excel export preserved the tested values, SUM formula and source
  marker. Direct native Numbers imports into Calc flattened SUM to a cached
  value in two trials. Export-copy success does not repair that import limit
  or write changes back to the native iWork source.
- **Slow and interrupted reads:** one normal mounted slow-link trial returned
  the complete 138,943-byte owned file with its expected checksum. A controlled
  interruption returned an error without delivering incomplete content; a
  later fair-proxy diagnostic read the same retained copy exactly. Earlier
  failures remain recorded, and that diagnostic does not uniquely establish
  their cause or demonstrate repeated provider reliability.
- **Retained local recovery:** bounded copied-state trials preserved and
  exported sealed/dirty working bytes, including an abandoned native Stage.
  Offline migration and running recovery-only checks remained separate from
  installing a writer against the user's original state.

The [application acceptance assessment](docs/benchmarks/icloud-iwork-application-acceptance-2026-10-08.json)
records the completed document criterion and exact content limits. Layout,
fonts, images, charts, animation, macros, external links and arbitrary formulas
or documents remain untested; native Linux Pages/Numbers/Keynote saving is
unsupported by the tested filters.

### Installed test-machine checkpoint

A new Arch test machine installed the attested **aa02** candidate packages and
optional Strata companion. Initial public native iCloud sign-in produced a
ready, mounted read-only account; attempted creation returned EROFS, and a
read returned the exact 138,943-byte Numbers fixture. Strata's four actual GUI
actions—direct Keep/Stop and parent Keep/Stop—showed direct, inherited and
unpinned states with event refresh and exact readback. Installed provider
files, default Nautilus association and absent Strata preferences were preserved.

The first fixture was already resident and tested aa02. After upgrading to
attested **7dc061c** packages, Cirrove restored the same saved session and ready
read-only mount without changing account identity or settings. A second Strata
trial used a fresh, independently verified **8 MiB file with zero resident
bytes**. All four actions passed again, including a visibly observed **Fetching**
badge, kept/inherited badges, availability dialogs, subscribed event refresh and
exact readback. Installed provider files, preferences and default Nautilus
survived deployment and testing. This closes the installed Strata preservation
criterion; it does not establish every desktop or general iCloud reliability.

Explicit native reauthentication then enabled writes on the same installed
account. The following normal save exposed a remaining bug: its newly created
folder reached iCloud, but the file stayed locally retained as Conflict. The
trial was stopped without retrying the upload; this sequence is under
investigation and does not count as successful installed write acceptance.
The [targeted parent-lookup correction](docs/benchmarks/icloud-fresh-mkdir-parent-regression-2026-10-09.json)
passes nine local controls after the identical primary fixture failed on the old code.
A fresh installed cloud retest is still required.
The [registered installed trial](docs/benchmarks/icloud-installed-bundled-2026-10-09-432da505-cb17-4658-8982-15a75dc67588.json)
also verified offline exports of a **44-byte sealed save** and a **52-byte
unlinked working file**, both with matching checksums. Read-only downgrade
and active recovery of those same generations remain untested. The trial
records the separate transport failure and each outcome’s scope.

A subsequent [offline package-upgrade test](docs/benchmarks/icloud-offline-installed-upgrade-2026-10-09-ae484a4d-4a97-4eca-b470-6c270eec8220.json)
installed the attested `2705756` packages into a fresh copy-on-write guest disk.
Both retained files exported with identical checksums before and after the
upgrade. The account stayed disabled; settings, persistent state and the
original disk were preserved. The installed daemon, authentication and provider
were not started by this test. This prepares the next genuine account-lifecycle
trial; it does not establish read-only downgrade or active recovery.

### What still blocks full acceptance

1. **Complete installed account lifecycle:** a successful owned-file save
   after the observed same-account write opt-in, followed by read-only downgrade
   and retained sealed/dirty-byte recovery, preserving other accounts and
   permanent-deletion refusal.
2. **Reliability:** remaining in-flight replacement and namespace uncertainty,
   expired-session recovery, storage/staging limits and abandoned-work handling,
   with registered real-account evidence and explicit size/format bounds.

Newly received saved cookies now retain absolute expiry. The identical
[local regression fixture](docs/benchmarks/icloud-cookie-expiration-local-2026-10-09.json)
failed on the old code and passed eleven controls with the fix; the complete
project check also passed. Whole-second precision may expire cookies up to one
second early. Unchanged legacy ciphertext lacks its original receipt time;
its labelled restoration anchor persists only after a later snapshot write.
This local fix does not prove natural Apple session expiry or its recovery.

Saved-session account health now revalidates during each existing scheduled
poll. Previously, caching the first successful validation could leave an
account marked Ready after a later authentication rejection. The identical
[regression fixture](docs/benchmarks/icloud-saved-session-health-regression-2026-10-09-38c5c4b2-2c9a-4538-8b17-c37937a6654d.json)
failed on the old code; three corrected controls and the complete project check
passed. Each poll performs one nonrecursive root listing. This establishes
synthetic rejection handling; natural Apple expiry and installed recovery
remain unproved.

Normal daemon polls also [persist changed session cookies](docs/benchmarks/icloud-owned-session-cookie-writeback-2026-10-09.json)
so restarts retain renewed cookies and deletions in local tests. Repeated updates
from the same source stay bounded without extending their expiry. Recovery-only
startup leaves credentials unchanged; legacy keyring sessions require foreground
migration.
Real Apple session-expiry recovery and wider reliability remain open.

Read the [full integration record](docs/icloud-write-integration.md) for historical
successes, retained failures and measured limits, and the
[deployment policy](docs/development.md#flat-numbers-source-journal-schema21-held-prerelease-policy)
before trying the branch with isolated state and mounts. Synthetic controls,
green CI, one bounded live pass and an installed test-machine checkpoint each
support their stated scope; none establishes a generally reliable iCloud release.

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
