# Using Cirrove

Cirrove puts a cloud drive into Files without copying the cloud onto your
disk. Files appear at once; their contents download when you open them and
stay in a bounded local cache; changes you make upload in the background.
What you want with you when there is no network, you mark once, and Cirrove
keeps it. OneDrive is the first provider.

This is the guide for using it. Building it is in the [README](../README.md);
what is finished and what is not is in the [milestones](product-milestones.md),
and what has been checked against a real account, and what has not, in
[compatibility](compatibility.md).

## Installing

**Arch:** two packages, `cirrove` (the service and the command line) and
`cirrove-desktop` (the settings window, the tray, and the Files extension).
Until there is a release they are built from the tree:

```sh
scripts/build-arch-package.sh
sudo pacman -U target/arch/cirrove-*.pkg.tar.zst    # the two it names, not the -debug ones
```

**Ubuntu 24.04 / Fedora:** `scripts/build-deb-package.sh` and
`scripts/build-rpm-package.sh` build the same two packages; see
[Distribution](distribution.md) for what is verified where.

The service is installed, not started. It starts the moment you connect a
drive from the window, or with `systemctl --user enable --now cirroved`. The
tray starts with your next login (it is an autostart entry); to have it now,
run `cirrove-tray`.

Sign-in grants are kept in your desktop keyring, so there has to be one that
is unlocked. A machine that logs you in automatically has none: nobody typed a
password to unlock it. Open *Passwords and Keys* and create the default
("Login") keyring once, or log in with your password once, and connecting
works from then on. Cirrove says this in its own words when it happens.

The tray icon needs a panel that shows StatusNotifierItems. GNOME needs the
[AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/)
for that; KDE Plasma and most other desktops have it built in. Without a
panel that can show it, everything the tray does is also in the window.

On GNOME, the extension only counts from your next login. Installing
`cirrove-desktop` brings the extension in with it, but a running GNOME Shell
looks for extensions when it starts and not afterwards -- and on Wayland it
cannot be restarted without ending the session. So right after installing,
the top bar shows nothing and `gnome-extensions` will say the extension does
not exist. Log out and back in, and the icon is there. Nothing is broken in
between, and the window works the whole time.

## Connecting a drive

Open **Cirrove** from your application menu and press **+** (or **Connect a
drive** on the empty page). You need:

- a **name** for the connection -- letters, digits, hyphens; it is what the
  command line calls the account;
- an empty **folder** where the drive should appear in Files;
- the **application (client) ID** of an Entra app registration. Cirrove does
  not ship one; the [OneDrive setup guide](onedrive-setup.md) walks through
  creating yours once. A second drive is prefilled with the first one's;
- whether to **allow changes**. Off, the drive is read-only: you can open
  everything and change nothing. On, files you save in the drive are uploaded.

**Sign in with Microsoft** opens your browser. Choose the account there, come
back, and pick which of its drives to mount. That is it: the folder fills as
you look into it.

From the command line the same is `cirrove connect --label <name>
--client-id <id> --mount-path <folder>`, which asks for the drive in the
terminal.

## Files and what happens to them

**Opening** a file downloads it. Large files start streaming at once; the
rest arrives as the application reads. The cache holds up to the limit shown
in the window (5 GiB by default) and evicts what you have not used in longest.

**Saving** a file in a drive that allows changes writes it locally first and
uploads it in the background; the application that saved it is done as soon
as the local write is. `cirrove status` shows how many changes are still on
their way. Turning the machine off before they are sent does not lose them:
they are written durably and resume at the next start.

**Names.** OneDrive refuses some names Linux allows -- the characters
`" * : < > ? \ |`, a trailing space or period, Windows device names such as
`CON` or `COM1`, a `~$` prefix -- and Cirrove refuses them at the moment you
choose them ("Invalid argument" in most applications, "File name too long"
past 255 characters) rather than accepting the file and failing to upload it
an hour later.

**Any file manager works.** The drive is a folder on your computer, so
Dolphin, Konqueror, Thunar, a terminal or any program's Open dialog use it
exactly like any other folder. What only GNOME Files has is the extra layer:
the state badges, the Cirrove column, the properties section and the
right-click **Keep offline**. If you use something else, everything those do
is in the Cirrove window and in the `cirrove` command, described below.

**Renaming, moving, creating and deleting** folders and files works as in any
folder. Deleting sends the item to the cloud's own recycle bin, not to your
desktop's wastebasket -- Cirrove refuses a `.Trash` folder in the drive's root
rather than pretend it has one. See [ADR 0008](adr/0008-deletion-and-the-recycle-bin.md).

**Keeping files offline.** In Files, right-click a file or folder in the
drive and choose **Keep offline**. Cirrove downloads it -- for a folder,
everything beneath it -- and holds it in the cache no matter what else you
open; it reads even with no network. A check mark on the icon means the
content is here; the synchronising emblem means it is still arriving. The
**Cirrove** column (View → Visible Columns) says the same in words. **Stop
keeping offline** releases it. Kept content counts against the cache limit,
and the window shows how much of the limit pinning has claimed.

**Alt+Enter** on a file opens its properties; a **Cirrove** section there shows
whether it is kept offline, how much of it is on this computer, and where it is
in the cloud.

The same from the command line: `cirrove pin <name> --path Documents/Reports
--recursive`, `cirrove unpin`, `cirrove pins`, and `cirrove paths <name>
<path...>` for the state of any path.

## The window and the tray

The window lists each connection with its state, account, whether changes are
allowed, its folder and its cache limit. **Mount** and **Unmount** are the
only two states a connection can be in; an unmounted drive is saved and comes
back with one click. **Sign in again** appears when the sign-in stopped
working -- a password change, a revoked consent, an administrator's policy --
and opens the browser. **Remove** takes a connection out of Cirrove; it is
offered only for an unmounted drive, and asks differently when there are
changes that never reached the cloud, because removing the connection is the
one way to lose them. What it removes is set aside under the state directory,
not deleted, and nothing in the cloud is touched.

**Recent activity** at the bottom of the window lists what changed lately:
files that arrived, changed or went away in the cloud since the service
started, and files saved here with where they are -- waiting, uploading, in
the cloud, or refused. The tray shows the last five under each connection;
`cirrove recent` prints them.

The tray shows one icon for all connections: a cloud when everything is
ready, transfer arrows while something is being fetched or sent, an
exclamation mark when a person must act. Its menu opens each drive's folder,
mounts and unmounts, and opens the window.

## When a save does not reach the cloud

A file you saved can fail to upload -- the cloud was out of space, the grant
lost permission, or the power went before the bytes were durable. The file
stays on your computer; the cloud has an older version or none. The window
shows **Saves that did not reach the cloud** with a count, and the tray marks
it. To try again, open the file and save it once more. `cirrove recent` lists
these as "saved here · upload failed".

## When the cloud refuses a change

Sometimes a change made locally cannot be applied: the file was changed or
removed on the cloud side in the meantime, or the provider rejected it.
Cirrove stops retrying, because a stale retry would act on whatever is there
now. The window shows **Changes the cloud refused** with the count, and the
tray icon shows the exclamation mark. **Discard** abandons those changes:
the local copies go, the cloud keeps its version, and the drive shows what the
cloud actually has. Nothing is re-sent. From the command line:
`cirrove discard-stuck <name>`.

## When the service is not there

The window says so in a banner and keeps showing your saved connections; the
tray shows the exclamation mark and "service unavailable". `systemctl --user
status cirroved` says why. The window also tells you when the running service
is older than itself ("Service update required").

## Diagnostics

`cirrove diagnose` writes one file with the versions, the service's status
and the last two hours of its journal, with your account name, tenant,
folders, file names, addresses and provider ids replaced by tokens -- the
same value gives the same token, so the bundle still reads, but the values
are not in it. The labels you chose for your connections are kept. The
replacement works on the shapes of things, not their meaning, so read the
file before sharing it; a file named after an ordinary word is not caught.

The unredacted versions, for your own eyes: `cirrove status` prints
everything the service knows about its state, as JSON; `journalctl --user -u
cirroved --since -1h` is the service's own account of what it did. Neither
contains your sign-in tokens, and the journal never prints file contents.

## Removing Cirrove

Removing the packages removes what they installed and nothing else: your
connections, their sign-in grants in the desktop keyring, the local index and
cache, and any changes still on their way stay under `~/.local/state/cirrove`.
On Debian and Ubuntu, `apt remove` keeps the tray's autostart entry under
`/etc/xdg/autostart` the way it keeps every configuration file; `apt purge
cirrove-desktop cirrove` removes that too.
To remove a connection and its data, use **Remove** in the window (or
`cirrove forget <name>`) first, which sets its data aside under
`~/.local/state/cirrove/removed/`; delete that directory when you are sure.
Grants in the keyring are labelled Cirrove; remove them from the keyring, or
revoke the application's consent in your Microsoft account settings.

Never delete data through a mounted drive's folder to "clean up": that
deletes it in the cloud.

## Supported versions

There is no release yet; what is supported is the current build of the main
branch on Arch, on the machine it is developed on. When releases start,
the policy is: the latest release, and the one before it for the length of
one release cycle; the distribution floor is Ubuntu 24.04 (GTK 4.14,
libadwaita 1.5), current Fedora, and current Arch; x86_64. Anything older or
elsewhere may work and is not claimed.
