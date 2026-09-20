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
| Multiple accounts mounted at once | fixture -- the manager, window and tray carry any number; one real account has ever been connected | manager and window tests |
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
reopen and OAuth guards have synthetic coverage. Google documents appear as browser
links; native exports and Shared Drives are outside the preview.

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
