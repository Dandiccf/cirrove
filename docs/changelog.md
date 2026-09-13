# Changelog

What a user notices, in the user's words. The milestone rows a change closes
are in the [ledger](acceptance-ledger.json); the engineering is in git.

## Unreleased

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
  cloud when ready, transfer arrows while working, an exclamation mark when
  something needs a person. The tray draws it whether or not the icon set is
  installed.
- **Packages.** Arch (`cirrove`, `cirrove-desktop`), Ubuntu 24.04 `.deb` and
  Fedora `.rpm`, each built and installed on a clean system by CI on every
  push and downloadable from the run. No release yet; see
  [distribution](distribution.md).
- **`cirrove paths`** shows, for any paths in a drive, what they are, which
  pin covers them and how much of their content is on disk.
- Pinning a folder without `--recursive` is now refused in words instead of
  dropping the connection; `unpin` no longer reports "pinned".
- A revoked grant is shown as "sign in required" rather than as being
  offline, and the journal says when a collection stops updating and when it
  resumes.
- The journal of pending uploads survives the machine losing power mid-write,
  verified by pulling the plug.
