# Compatibility

What has been checked against a real account, what only against fixtures,
and what not at all. A row says "real" only where a live run is recorded in
[validation](validation.md), [write validation](write-validation.md) or
[the benchmarks](benchmarks/); "fixture" means the code path is exercised by
tests against a synthetic provider and has never met the real service;
"no" means nothing claims it. Updated when a row changes hands.

## Accounts and sign-in

| | Status | Where it is recorded |
| --- | --- | --- |
| Microsoft work/school account (Entra tenant), single-tenant app registration | real -- the development account, in daily use | validation.md, the benchmarks |
| Personal Microsoft account (`common` authority) | no | -- |
| Multiple accounts mounted at once | real -- OneDrive, personal Google My Drive and Workspace My Drive remained mounted together during the isolated Shared Drive runs | benchmarks/google-shared-drive-live.json; manager and window tests |
| Browser sign-in with PKCE, token refresh at the token's lifetime | real | validation.md |
| Consent disabled and re-enabled by an administrator | real -- shown as sign-in required, not as offline; recovery on re-enable | benchmarks/revoked-grant-and-reauthentication.json |
| Sign-in again (reauthentication) from the CLI and the window | fixture -- the flow runs; nobody has completed a live re-sign-in through the window | window scenario; accounts tests |
| Keyring locked at daemon start | real -- a fresh Fedora 44 that logs in automatically had no keyring at all (the directory empty, only a transient session collection), so a sign-in would have failed at the moment it stored the token; logging in once with a password created and unlocked it, which is what the user guide prescribes | benchmarks/fresh-fedora-installation.json |

## Drives and libraries

| | Status | Where |
| --- | --- | --- |
| The account's own OneDrive (drive type `business`) | real | everything |
| Choosing among several drives at connection time | fixture -- the listing is real Graph, the choice was always the first | connect flow |
| SharePoint document libraries as separate drives | no | -- |
| A linked folder into another drive | real -- the development account's `Dokumente` is a shortcut to a second collection's root, so every recorded run has gone through one; traversed, read, kept offline, written to and deleted through the link on 2026-09-16 | validation.md § a linked folder into a second drive |
| Duplicate links, moved or deleted links, link cycles | fixture -- ancestry, cycles, duplicate targets and removed links are tested against a synthetic provider only | validation.md § shortcuts |
| Folder-only access (a share of one folder, not the drive) | no | -- |
| Per-item restricted permissions, revoked access to one item | no | -- |

## Files and changes

| | Status | Where |
| --- | --- | --- |
| Reading: listing, opening, streaming large files, cache eviction | real | validation.md, benchmarks |
| Opening the mount folder from the window | real -- Fedora 44 GNOME Wayland with the account signed in, 2026-09-14: the Open in Files button opens Files at the mount, and with xdg-desktop-portal, -gnome and -gtk all stopped and masked it still does, through GTK's fallback. Cancellation is covered on the flow that has a chooser -- Kept offline, + , Folder... raises the portal folder chooser rooted inside the mount, and Escape leaves the pins unchanged and the window running | benchmarks/session-matrix.json |
| Opening a file in an application | real -- in the Ubuntu VM GNOME session: a text editor and a spreadsheet open a mount file from Files, and a headless LibreOffice conversion opens a .docx; confirmed by the account owner double-clicking in the window | vm-recovery-and-real-desktop.json |
| Remote creates, edits, moves and deletions reaching the mount | real -- and now also driven server-side through Graph rather than by Cirrove: folder create visible in 4 s, file create 6 s, a 31 to 111 byte edit 16.4 s with stat's size and the bytes read agreeing at every sample, rename and delete under 2 s, move 16 s | validation.md § change notification, benchmarks/remote-change-propagation.json |
| Saving, renaming, moving, creating and deleting from the mount | real -- 192 changes applied on the live account, zero stuck at last count | write-validation.md |
| Deletion into the provider's recycle bin | real | ADR 0008 |
| Permanent deletion as a second gesture | no -- ADR 0008 names it; nothing implements it yet | -- |
| Conflict: a change the cloud refused, discarded from the window or CLI | real -- fourteen folder removals stranded by a chaining fault, cleared with `discard-stuck` | ADR 0005 correction, mutations tests |
| Names the provider refuses (`:`, trailing `.`, `CON`, `~$`, 256 characters) | fixture -- the rules are Microsoft's published ones, enforced at the mount and tested there; no live attempt recorded | onedrive naming tests, writable_session |
| OneNote notebooks and other packages | decided and enforced -- a package is shown as a folder so its contents can be listed and copied, and every change inside it is refused with `EOPNOTSUPP` at the mount, at any depth: a section file is written by OneNote alone, and letting a text editor save over one offers to corrupt a notebook. Refused before anything is journalled, so it cannot become a stuck change. Fixture-tested end to end through a real mount | writable_session real_a_package_is_readable_and_refuses_every_change_inside_it |
| Offline pinning, per file and per folder, reads with the network gone | real | benchmarks/live-offline-pinning.json, benchmarks/offline-pinning-reachability.json |
| Power loss mid-write | real -- the plug pulled, the journal recovered | benchmarks/journal-under-power-cut.json |
| Deep suspend past the token lifetime | real -- 90 minutes of S3 on the Fedora VM, 2026-09-14: the same process comes back with its mount intact, refreshes its Microsoft grant unprompted, renews both lapsed subscriptions, and receives a change made afterwards in 6 s | benchmarks/deep-suspend-beyond-token-lifetime.json |
| 24 hours of operation | real -- 24.0 hours in the Ubuntu VM, 2026-09-13 15:56 to 2026-09-14 15:55: the mount present in all 1440 samples, memory flat at 43 MiB peak, the index steady, the slowest root listing of the day 7 ms, and unattended recovery from both a deliberate restart and a five-minute link outage | benchmarks/sustained-operation.json |

## Unsupported operations

What the mount answers when asked for something it does not do. Each is a
deliberate answer, not an accident, and the errno is the one a local
filesystem gives in the same situation.

| Operation | Answer |
| --- | --- |
| Hard links, symbolic links, device and special files | not implemented: `ENOSYS` from the FUSE layer |
| Reading extended attributes | none: `ENODATA`, and an empty list |
| Setting extended attributes | not implemented: `ENOSYS` |
| Changing mode, owner or group | `EOPNOTSUPP`: the provider has no such thing, and pretending would lie to the next `stat` |
| Creating anything but a regular file | `EOPNOTSUPP` |
| A `.Trash` folder in the drive's root, or moving into one | `EOPNOTSUPP` (the provider's recycle bin is the wastebasket) |
| A name the provider would refuse | `EINVAL`; `ENAMETOOLONG` past the limit |
| Renaming with flags other than `RENAME_NOREPLACE` | `EOPNOTSUPP` |
| Any change inside a provider package (a OneNote notebook and what is under it) | `EOPNOTSUPP`: the contents are readable and copyable, and only the application that writes that format can change them safely |
| Shared, writable memory mapping (`mmap` `MAP_SHARED`) | `ENODEV`: files open with `FOPEN_DIRECT_IO`, and the kernel permits only private (`MAP_PRIVATE`) mappings on a direct-I/O file even with `FUSE_DIRECT_IO_ALLOW_MMAP`. Read and private mmap work, so ordinary applications -- text editors, LibreOffice -- open files; a program that requires a shared mapping does not. |

## Desktops and distributions

| | Status |
| --- | --- |
| Arch, Hyprland (Omarchy), Quickshell tray, Nautilus 50 | real -- the development machine |
| GNOME session (Wayland), AppIndicator tray, Files | real -- Fedora 44 and Ubuntu 24.04 in VMs: the tray registers with the shell and survives a reboot, Files loads the extension, and with an account signed in the icon is drawn in the top bar (Ubuntu 2026-09-13, Fedora 2026-09-14 -- the latter also showing the mount in the Files sidebar and the kept-offline badge on a pinned file) |
| KDE Plasma 6 session (Wayland), tray, window | real -- Fedora 44 VM, 2026-09-13: the packaged autostart starts the tray, it registers with KDE's own StatusNotifierWatcher and appears in the tray with nothing installed alongside it, the window renders under KWin as a native Wayland client with its own icon in the task bar, and it follows the system dark-style preference. Portal folder opening is untested here for want of an account. See [the record](benchmarks/plasma-session-coverage.json) |
| X11 session, either desktop | real -- Ubuntu 24.04 VM, 2026-09-14, with the account signed in: GNOME on Xorg and Plasma on X11, the window rendering, the tray registered and the drive mounted in both. This is where the X11 application identity defect was found: the window's WM_CLASS was the binary name, so no shell could match it to its desktop entry | benchmarks/session-matrix.json |
| KDE Plasma 6 session on Ubuntu (Wayland and X11) | real -- Plasma 5.27 added alongside GNOME on the Ubuntu VM, 2026-09-14, both session types with a real account: native Wayland without falling back, the tray on KDE's own watcher, and the portal reporting a dark preference where GNOME reports none | benchmarks/session-matrix.json |
| Ubuntu 24.04 packages installed on a clean system | real -- CI runner and an Ubuntu 24.04.4 Desktop VM through a reboot and purge |
| Fedora packages installed on a clean system | real -- CI container (42), and a Fedora 44 Workstation VM reinstalled 2026-09-14 with nothing Cirrove needs pre-installed: the two rpms alone pulled in nautilus-python and the GNOME appindicator extension, SELinux stayed enforcing with no denial, the tray and Files came back after a reboot from the packaged autostart, and removal left no file named cirrove under /usr or /etc. A sign-in the same morning closed the rest: the drive mounts under SELinux enforcing with zero AVC denials of any kind, a 67 KB file reads end to end through FUSE, the tray goes Active and the shell draws it, Files shows the mount and the branded badge, and all of it survives a reboot with nothing started by hand. See [the record](benchmarks/fresh-fedora-installation.json) |
| Debian stable | not claimed: older than the desktop floor |
| Uninstall with a live account | real -- Fedora 44 VM, 2026-09-14, packages removed with the drive mounted and 2.6 GB of state: nothing packaged survived under /usr or /etc, the state directory and the keyring grant were left alone, and stopping the service unmounted cleanly with no stale handle. The removal does leave the daemon running from a deleted binary still serving the mount until the session ends. `cirrove local-data` now reports what is kept and `--discard-removed` reclaims what an earlier removal set aside | benchmarks/clean-uninstall.json |
| Arch packages installed on a clean system | real -- an Arch 2026.09.01 VM installed unattended on 2026-09-14, packages from CI: every file where it belongs including the German catalogue, service and tray back after a reboot from the packaged autostart alone, the window German from the package, and pacman -Rns leaving nothing named cirrove behind. Note that Arch does not install the optional Files and tray integration: unlike Fedora's and Debian's Recommends, optdepends are the user's to add. Sign-in untested here | benchmarks/arch-clean-install.json |
| Package upgrade under a live account | real -- Fedora 44 VM, 2026-09-14, r409 to r418 and back: the account, the credential in its keyring and a pin survive both directions and exactly one Files extension remains. Neither direction restarts the daemon or the tray, which go on running from deleted binaries, so the person stays on the version they replaced; the window now says so. See [the record](benchmarks/upgrade-and-rollback.json) |

## Google Drive preview

Google My Drive runs through the shared engine, store, cache, journal, pinning and
FUSE mount. Read-only and read-write OAuth grants produce the matching mount mode.
My Drive listing, polled changes, bounded reads, duplicate names, offline cache
reopen and OAuth guards have synthetic coverage. Native Google Docs and Sheets
appear as read-only package folders with bounded DOCX/XLSX exports; one item of
each type was checked live on the original account. PDF and OpenDocument formats
are also exposed. One exact Shared Drive Doc with heading, bold/italic text and
bullets and one Sheet with a frozen bold header, currency formatting, two
formulas and a column chart passed API readback, all six direct exports and all
six repeatable reads through a fresh private mount. Only those two registered
fixtures were then trashed. This is representative structured-content evidence,
not arbitrary-document or native-writeback coverage. Shared Drive discovery and
a separate collection have synthetic and one bounded Workspace live
run: root, folder, binary content, feed recovery after a drive-level change,
restart and small native exports passed in an isolated mount. See the
[live record](benchmarks/google-shared-drive-live.json). Explicitly granted
Shared Drive writes have bounded live create, crash/restart, rename, move,
replacement and trash evidence from one isolated Workspace administrator;
the reviewed branch also passed a separate no-bypass writable FUSE create and
independent exact-ID/hash readback. A separate one-hour private mount completed
61 matching reads with a fresh Shared Drive feed and clean private shutdown.
Another fresh private mount retained an empty content cache for one hour through
one accepted indexing interval, then read an exact uncached 16,777,263-byte
fixture in 5.247 seconds and shut down without changing the installed service.
Restricted roles and longer sessions remain unverified. See
the [mounted record](benchmarks/google-shared-writable-mount-live.json),
[no-bypass record](benchmarks/google-shared-production-gate-live.json) and
[hour record](benchmarks/google-shared-hour-session-live.json), plus the
[large late-read record](benchmarks/google-shared-late-large-uncached-live.json).

A bounded live writable run on one separate account completed binary file create,
in-place edit, editor-style atomic replacement, file and folder rename/move,
regular-file deletion, observed-empty folder removal, conflict preservation and
daemon restart. An existing Google binary file was also pinned until resident and
then unpinned. A broader installed-mount run added a 20 MiB-plus write with direct
Drive hash comparison, LibreOffice ODT and DOCX saves, held-descriptor atomic
replacement, an externally renamed file followed and edited through the mount,
and pin/unpin through the natural path presented by Files and Dolphin. The last
case exposed and corrected a mismatch between FUSE's writable-name overlay and
control-socket path resolution; its negative regression control and installed
live retest are recorded. This establishes the bounded workflows on that account,
not broad provider reliability. Folder removal remains a non-atomic
list-then-PATCH sequence, and Drive offers no atomic sibling-name reservation. See
[Google Drive](google-drive.md), the
[writable mount record](benchmarks/google-drive-writable-mount.json) and the
[broader live record](benchmarks/google-drive-live-acceptance-2.json).

## iCloud development and Linux editor formats

iCloud remains an experimental branch, outside the released packages. Its
[validation record](icloud-write-integration.md) distinguishes provider content
verification from application import and save fidelity.

| Format and workflow | Observed scope | Remaining limit |
| --- | --- | --- |
| Native Numbers file opened read-only in LibreOffice Calc | One actual iCloud Numbers archive opened in a visible Calc session with A2=11, B2=3 and C2=14; the original archive stayed unchanged. | C2 imported as a numeric VALUE, with `getFormula()` returning `14`, rather than the SUM formula shown in Apple Numbers. Formula-preserving native Numbers editing is not established. |
| XLSX created and edited in LibreOffice Calc | One local file was created, reopened, saved once and reopened again with `SUM(A2:B2)` preserved and values changing from 7/3/10 to 11/3/14. An independent OOXML inspection confirmed both snapshots. | This local application trial does not establish iCloud upload, atomic replacement, remount or installed acceptance. |
| Historical isolated iCloud mount trial, 5 October | One actual Calc save and reopen retained 7/3/SUM=10; its source and sealed content stayed preserved after the trial stopped. A separate fresh read-only mount verified the owned folder, but did not list the final XLSX. | This individual trial's cloud save remains unconfirmed, and its second save was not attempted. The initiating refusal remains unexplained; later successful trials do not retroactively confirm this one. |
| Historical Calc temporary-file cleanup controls | Diagnostics reproduced a local journal refusal. The correction passed a regression that failed before the change, retained-byte/restart checks, and three matrices of 40 hostile identity/dependency controls. | These synthetic controls alone establish no actual cloud save. The separate 6 October trial below supplies bounded provider evidence. |
| Two Calc XLSX saves through a fresh isolated iCloud mount, 6 October | Calc saved and reopened 7/3/SUM=10, then 11/3/SUM=14. Independent DATA reads matched both source byte streams; a separate normal read-only remount matched all 5,612 B bytes. A subsequent independent Trash reader matched all 5,281 original A bytes. | One bounded ordinary XLSX workflow, with UNO observations and no human GUI witness. The Trash CLI succeeded, while its controller retained a reporting exit 1. Restoration, native iWork fidelity, repeatability and installed acceptance are not established. |
| Genuine edited Numbers B imported into Calc and exported once as XLSX, 6 October | A fresh headless, network-isolated Calc session read the genuine Apple-edited export, preserved cached 17/3/20, exported one XLSX copy and reopened it with the same measured cells. The Numbers source stayed unchanged. | Import and reopen both exposed C2 as VALUE with `getFormula()` returning `20`, not SUM. The conversion workflow completed but formula fidelity failed; no native Numbers save or GUI acceptance was tested. |
| [Genuine Pages B imported into Writer and exported once as DOCX, 7 October](benchmarks/icloud-pages-writer-private-library-2026-10-07-2f2fec37-6b7c-40aa-b8db-49fa98d5192c.json) | A headless, network-isolated session with a verified private `libwpg` capsule imported Pages read-only and reopened its one DOCX export read-only. Both retained the exact 93-byte text as one paragraph; independent ZIP CRC/OOXML checks agreed and the source stayed unchanged. | Text-only conversion. Fonts, layout, media, native Pages save and general format fidelity remain unproved. The host dependency remained absent; no system package repair, GUI or installed acceptance was tested. |
| [Two Writer DOCX saves through a fresh isolated iCloud mount, 7 October](benchmarks/icloud-writer-mounted-docx-2026-10-07-f015bd4d-44a1-4676-88ca-a0a0445ed9ab.json) | Writer created and reopened A, then edited, saved and reopened B, preserving the exact registered paragraphs. Independent DATA reads matched all 5,829 A and 5,906 B bytes; a normal read-only remount matched B and a typed Trash observer verified original A. Independent audit passed 29/29; all owned processes and mounts closed. | One headless ordinary DOCX workflow using the genuine Pages-to-DOCX conversion as its seed. Native Pages saving, layout/fonts/media, GUI, repeatability and installed acceptance remain unproved. Application network access was disabled; the mounted daemon performed authorized cloud writes. |
| [Genuine Keynote B imported into Impress and exported once as PPTX, 7 October](benchmarks/icloud-keynote-impress-pptx-2026-10-07-20d21b4e-a918-41db-b7f5-e1a0fb3c593c.json) | A headless, network-isolated session loaded Keynote read-only and reopened one PPTX export read-only with the exact title and subtitle on one slide. Independent ZIP CRC/slide XML and terminal checks passed 28/28; source and tool pins stayed unchanged. | Text-only conversion. Layout, fonts, media, transitions, animations, native Keynote save, GUI and installed acceptance remain unproved. |
| [Two Impress PPTX saves through a fresh isolated iCloud mount, 7 October](benchmarks/icloud-impress-mounted-pptx-2026-10-07-85ee1211-763d-46b7-b7a7-2886e6867e0d.json) | Impress created and reopened A, then edited, saved and reopened B with the exact registered title and subtitle. Independent DATA reads matched all 8,304 A and 8,416 B bytes; a normal read-only remount matched B and a typed Trash observer verified original A. Independent audit passed 24/24; all owned processes and mounts closed. | One headless ordinary PPTX workflow using the genuine Keynote-to-PPTX conversion as its seed. Native Keynote saving, layout/fonts/media/animations, GUI, repeatability and installed acceptance remain unproved. Application network access was disabled; the mounted daemon performed authorized cloud writes. |

These results are specific to the tested files and installed LibreOffice build.
Do not infer general iWork format fidelity from readable cell values. The installed
Numbers, Pages and Keynote LibreOffice filters are import-only; no native iWork
save or export is promised. The same genuine Numbers B has separate
[Apple edit/export evidence](benchmarks/icloud-numbers-edited-b-donor-2026-10-06.json)
and [replacement/recovery evidence](benchmarks/icloud-flat-numbers-native-final-2026-10-06.json);
content preservation there does not establish Linux application formula fidelity.
The broader native editor matrix remains open. Preserve native originals when
testing conversions. See the
[two-save DATA and remount record](benchmarks/icloud-calc-after-metadata-publication-2026-10-06.json),
[original Trash read](benchmarks/icloud-calc-trash-original-2026-10-06.json),
[genuine-B Calc conversion and its retained initial setup refusal](benchmarks/icloud-numbers-calc-genuine-export-2026-10-06.json),
the [registered Calc trial](benchmarks/icloud-calc-editor-2026-10-05.json),
the [partial mounted trial](benchmarks/icloud-calc-mounted-editor-2026-10-05.json),
the [fresh read-only observation](benchmarks/icloud-calc-retained-read-2026-10-05.json),
and [independent Numbers decoding](benchmarks/icloud-numbers-independent-decoder-2026-10-05.json).
