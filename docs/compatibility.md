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
| Keyring locked at daemon start | fixture | auth vault tests |

## Drives and libraries

| | Status | Where |
| --- | --- | --- |
| The account's own OneDrive (drive type `business`) | real | everything |
| Choosing among several drives at connection time | fixture -- the listing is real Graph, the choice was always the first | connect flow |
| SharePoint document libraries as separate drives | no | -- |
| Linked folders (shortcuts into another drive), duplicate links, moved or deleted links | fixture -- ancestry, cycles, duplicate targets and removed links are all tested against a synthetic provider | validation.md § shortcuts |
| Folder-only access (a share of one folder, not the drive) | no | -- |
| Per-item restricted permissions, revoked access to one item | no | -- |

## Files and changes

| | Status | Where |
| --- | --- | --- |
| Reading: listing, opening, streaming large files, cache eviction | real | validation.md, benchmarks |
| Opening a file in an application (read, and LibreOffice which reads the whole document) | real -- gnome-text-editor and a headless LibreOffice conversion both open a mount file in the Ubuntu VM | vm-recovery-and-real-desktop.json |
| Remote creates, edits, moves and deletions reaching the mount | real -- the delta feed on the live account, with notifications | validation.md § change notification |
| Saving, renaming, moving, creating and deleting from the mount | real -- 192 changes applied on the live account, zero stuck at last count | write-validation.md |
| Deletion into the provider's recycle bin | real | ADR 0008 |
| Permanent deletion as a second gesture | no -- ADR 0008 names it; nothing implements it yet | -- |
| Conflict: a change the cloud refused, discarded from the window or CLI | real -- fourteen folder removals stranded by a chaining fault, cleared with `discard-stuck` | ADR 0005 correction, mutations tests |
| Names the provider refuses (`:`, trailing `.`, `CON`, `~$`, 256 characters) | fixture -- the rules are Microsoft's published ones, enforced at the mount and tested there; no live attempt recorded | onedrive naming tests, writable_session |
| OneNote notebooks and other packages | fixture -- mapped as folders from a fixture body; never opened on the real account | onedrive lib tests |
| Offline pinning, per file and per folder, reads with the network gone | real | benchmarks/live-offline-pinning.json, benchmarks/offline-pinning-reachability.json |
| Power loss mid-write | real -- the plug pulled, the journal recovered | benchmarks/journal-under-power-cut.json |
| Deep suspend past the token lifetime | no -- registered, not run | benchmarks/deep-suspend-beyond-token-lifetime.json |
| 24 hours of operation | no -- registered, not run | benchmarks/sustained-operation.json |

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
| Shared, writable memory mapping (`mmap` `MAP_SHARED`) | `ENODEV`: files open with `FOPEN_DIRECT_IO`, and the kernel permits only private (`MAP_PRIVATE`) mappings on a direct-I/O file even with `FUSE_DIRECT_IO_ALLOW_MMAP`. Read and private mmap work, so ordinary applications -- text editors, LibreOffice -- open files; a program that requires a shared mapping does not. |

## Desktops and distributions

| | Status |
| --- | --- |
| Arch, Hyprland (Omarchy), Quickshell tray, Nautilus 50 | real -- the development machine |
| GNOME session (Wayland), AppIndicator tray, Files | real -- Fedora 44 and Ubuntu 24.04 in VMs: the tray registers with the shell and survives a reboot, Files loads the extension; no icon drawn, for want of an account (see distribution.md) |
| KDE Plasma | no |
| Ubuntu 24.04 packages installed on a clean system | real -- CI runner and an Ubuntu 24.04.4 Desktop VM through a reboot and purge |
| Fedora packages installed on a clean system | real -- CI container (42) and a Fedora 44 Workstation VM through a reboot and removal |
| Debian stable | not claimed: older than the desktop floor |
