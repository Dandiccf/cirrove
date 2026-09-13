# Changelog

What a user notices, in the user's words. The milestone rows a change closes
are in the [ledger](acceptance-ledger.json); the engineering is in git.

## Unreleased

- **The window says when a desktop has no tray**, instead of only writing it to
  the system log: a line naming what to install, on GNOME the AppIndicator
  extension. It appears only when the session really has no tray, never because
  the check itself failed.
- **A drive's read or write access is changed from the window.** The Access row
  offers the other direction: **Allow changes** where the drive is read-only,
  **Make read-only** where it can write. Microsoft's own consent screen still
  does the granting. This was the last thing an ordinary user had to open a
  terminal for. Making a drive read-only stops Cirrove changing anything in it;
  withdrawing the permission itself is done in your Microsoft account, and the
  window says so rather than implying the button does it.
- **What you keep offline is in the window.** Each connection lists what it
  keeps, named by where it is in the drive rather than by a provider id, with
  how much of it is really on this computer and how much of the cache pinning
  has claimed. **Stop keeping** releases one. **Keep a file or folder offline**
  opens a chooser in the drive. Until now this existed only on the command line
  and in the Files context menu, so there was nowhere to see what the cache was
  spent on.
- **A pin that reserved space but has not downloaded yet says so** rather than
  reporting the space as filled. It is the difference between a file that will
  open on a train and one that will not.
- **The badges in Files are Cirrove's own.** A blue cloud with a check for kept
  and here, a cloud with a down arrow while the content arrives, both with a rim
  so they read over a thumbnail of any colour. They were the desktop's generic
  tick and circular arrow, which said nothing about a cloud and did not match
  the tray. Where the icons are not installed the generic ones are still used,
  because a name no theme provides draws nothing at all.
- **A file that is on this computer but not kept offline says so.** The
  properties dialog used to read "Availability: On demand" beside "On this
  computer: all 66.2 KB", which says nothing about whether the copy will still
  be there tomorrow. It now reads "all 66.2 KB, until the space is needed". A
  pinned file still says "all 66.2 KB", because there the promise is real.
- **`cirrove --help` names the right command.** `diagnose` described itself as
  the way to abandon changes the cloud refused, which is `discard-stuck` --
  and `discard-stuck` had no description at all, so the one command that fixes
  a stuck change was the one the help would not explain. `enable` and
  `disable` now say what they do too.
- **The diagnostics file no longer carries the drive's id.** It was replaced
  everywhere the daemon reports it as a field, but a log line naming it in
  passing got through.
- **Cirrove works on KDE Plasma as well as GNOME.** The tray appears there with
  nothing installed alongside it, where GNOME needs an extension, and the window
  follows the system's dark style.
- **File properties in Files** (Alt+Enter) gain a Cirrove section: availability,
  how much is on this computer, the pin, and the cloud location.
- **Files shows what is kept offline.** A check on pinned files and folders
  whose content is on disk, the synchronising emblem while it arrives, a
  "Cirrove" column with the words, and **Keep offline** / **Stop keeping
  offline** in the context menu. Needs `nautilus-python`.
- **Everything about an account is in the window.** Connect a drive (browser
  sign-in, then pick the drive), sign in again when a grant stopped working,
  discard changes the cloud refused, remove a connection -- each offered only
  where it applies, and removal asks differently when changes never reached
  the cloud.
- **Cirrove has an icon.** A cloud over a drive in launchers; in the tray, a
  colored cloud -- blue with a check when everything is up to date, blue with an
  up arrow while a save is on its way, amber with an exclamation when something
  needs a person -- so it reads on any panel, including shells that do not
  recolor symbolic icons, and the three tell each other apart at the size a
  panel actually draws them. The tray draws it whether or not the icon set is
  installed.
- **Packages.** Arch (`cirrove`, `cirrove-desktop`), Ubuntu 24.04 `.deb` and
  Fedora `.rpm`, each built and installed on a clean system by CI on every
  push and downloadable from the run. No release yet; see
  [distribution](distribution.md).
- **Recent activity.** The window lists what changed lately -- in the cloud,
  and saved here with where it is on its way -- the tray shows the last five
  under each connection, and `cirrove recent` prints them.
- **`cirrove diagnose`** writes a diagnostics file with account names,
  folders, file names, addresses, ids and the machine's name replaced, for
  sharing.
- Names OneDrive refuses (a colon, a trailing period, `CON`, ...) are refused
  when you choose them rather than failing to upload later.
- **`cirrove paths`** shows, for any paths in a drive, what they are, which
  pin covers them and how much of their content is on disk.
- Pinning a folder without `--recursive` is now refused in words instead of
  dropping the connection; `unpin` no longer reports "pinned".
- A revoked grant is shown as "sign in required" rather than as being
  offline, and the journal says when a collection stops updating and when it
  resumes.
- The journal of pending uploads survives the machine losing power mid-write,
  verified by pulling the plug.
