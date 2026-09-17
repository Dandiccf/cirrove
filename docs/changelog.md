# Changelog

What a user notices, in the user's words. The milestone rows a change closes
are in the [ledger](acceptance-ledger.json); the engineering is in git.

## Unreleased

- **Your desktop's search no longer reports your files as damaged.** GNOME
  indexes your home folder, and because a Cirrove drive looks like an ordinary
  disk to it, it walks the whole drive -- which is the point: your cloud files
  turn up in desktop search without being downloaded. But when thousands of
  files arrived at once Cirrove answered *try again* instead of making the
  caller wait, and nothing outside Cirrove reads that as *wait*: one crawl of a
  160,000-file drive produced 7,892 failures reading **PDF document is
  damaged** about files that opened perfectly the moment the machine was quiet.
  Cirrove now waits rather than turning anyone away, in every place it used to
  refuse.
- **Opening a folder reads the index seven times less.** Cirrove's index of your
  drive had grown to 564 MB and was being read through a 2 MB window, a system
  call at a time: 82 reads to open one directory, and 875,000 reads a second
  while the desktop was crawling. It is 11 reads and about 2,000 a second now.
  You notice it as a machine that stays responsive while something walks your
  drive in the background.
- **Cirrove's memory no longer grows with the size of your drive.** Something
  that walks every file -- a desktop indexer, a backup, a search -- used to
  leave Cirrove holding about **490 MB** by the end of half a million files,
  because it remembered every file the system had asked about and nothing ever
  told the system it could forget them. It now holds at most **150 MB** however
  large the drive is, letting go of what has not been touched for longest, and
  the walk is not slower for it -- 94.7 seconds against 96. On a machine with a
  gigabyte to spare that is the difference between working and not.

- **Taking away Cirrove's access now has a measured answer.** Revoke the app's
  consent in your directory and Cirrove keeps working for up to an hour -- the
  pass it holds has its own expiry and Microsoft does not re-check consent on
  every request -- then says **Sign in again**. Measured end to end on a real
  tenant: 66 minutes to notice, and signing in again restored the connection
  with its whole index intact, 184,080 items, no reindex.
- **A sign-in is no longer thrown away at the end.** Cirrove keeps your grant in
  the desktop's keyring, and it used to discover that there was no keyring --
  or that it was locked -- only after you had signed in with Microsoft and
  chosen a drive. Minutes of your attention, gone, with an error about D-Bus.
  It now asks before opening the browser, offers a locked keyring to the desktop
  to unlock, and says which package to install when there is none.
- **Signing in works on a machine without a desktop's extras.** `xdg-utils` was
  an optional dependency described as being for the command line. It is neither:
  connecting an account opens your browser through `xdg-open`, from the window
  as much as from the terminal, so on a distribution that does not install
  optional dependencies a fresh install could not connect an account at all --
  the Sign in button said "could not start the browser: No such file or
  directory". It is required now, and if it is ever missing the message names
  the package.
- **Keeping a folder offline no longer reports failure for work that
  succeeded.** It used to run inside the request the command line and the
  window make, and both give up after three seconds -- so any folder worth
  keeping said "Cirrove pin timed out" while Cirrove went on keeping every file
  of it. It now answers at once and reports its progress: the connection's
  **Kept offline** section shows what is arriving, how many files and how much
  of it, and a **Stop** button. Stopping releases the pin -- you asked for the
  folder, not for part of it -- and a download that fails keeps what it managed
  and says why. `cirrove pin` still waits and prints how far it has got;
  `cirrove jobs` and `cirrove stop` are there for the same thing.
- **Changes the cloud refused can be tried again.** Until now the only thing
  you could do with one was discard it, which is right for a change the cloud
  decided about and throws away work for one that merely failed -- a quota, a
  permission, a connection that went away. The window tells the two apart and
  offers **Try again** beside **Discard**, greyed out when every refusal is the
  kind that would act on whatever is in the cloud now. `cirrove retry-stuck`
  does the same from the command line.
- **Badges in Files stay right.** Files asks about a file once and keeps the
  answer, so keeping something offline from the Cirrove window left an open
  Files window showing "On demand" for a file that was already on the computer,
  until something made it re-list. It now refreshes itself.
- **The badges in Files are flat and monochrome.** A white mark on a dark disc
  with a light rim -- no brand colour in your file list, and still readable
  over a thumbnail of any colour.
- **A save on its way up says how far it has got.** "Uploading" on its own
  cannot tell a transfer that is moving from one that is stuck; the number was
  recorded all along and never shown.
- **Releasing a pin works everywhere.** Files kept offline from a SharePoint
  library could not be released from the window at all -- the button reported
  that the item did not exist. What is kept is a local record, and releasing it
  no longer asks the cloud for permission.
- **Recent activity says when.** Every line now carries how long ago it
  happened -- just now, 25 minutes ago, yesterday. Saves had no time recorded
  at all, so the list could tell you what you had saved and never when, which
  mattered most right after a restart: the cloud side of the list starts empty
  by design, leaving a column of saves against nothing.
- **Cirrove speaks German.** The window, its dialogs, the connection states and
  the activity list are translated; anything not translated stays in English
  rather than going blank, so a language with no catalogue loses nothing. It
  follows the language your desktop is already set to, with no setting of its
  own -- an Austrian or Swiss desktop gets the German catalogue too. A check
  now refuses a release where a string in the program has no entry to
  translate, or where a translation has dropped one of the values its sentence
  was built to carry.
- **A diagnostics bundle can no longer carry a token.** The bundle is made to
  be sent to someone else, and its redaction replaced account names, folders,
  file names and ids but went straight past anything shaped like a credential:
  a JSON Web Token passed through untouched. It now removes tokens and long
  opaque secrets, and still leaves the things a bundle is for -- versions,
  states, counts, error codes -- exactly as they were.
- **You can see what Cirrove keeps on this computer, and get some of it back.**
  `cirrove local-data` lists each connection's cache and index, anything an
  earlier removal set aside, and the total. Removing a connection has always
  moved its data aside rather than deleting it, which is right -- but nothing
  ever listed it again, so it sat there for good, and the only documented way
  to reclaim it was to delete a directory by hand. `cirrove local-data
  --discard-removed` does it properly and says how much it freed. It cannot
  touch a drive you are still using.
- **Cirrove tells you when an update has been installed but is not running
  yet.** Upgrading the package replaces the files, but Linux leaves the running
  program on the old ones, so until you log out you are still using the version
  you just replaced -- and nothing said so. The clearest way to notice used to
  be that the update appeared to change nothing at all. The window now says it
  plainly, with what to do about it.
- **Cirrove has its own mark, and it is not a cloud.** A ring broken at the
  upper right, with what is happening in the middle: a dot when everything is
  quiet, an arrow while something is moving, an exclamation when you have to
  act. In the panel it is white with a dark rim, so it belongs beside the
  shell's own icons instead of shouting in colour, and it still reads on a
  light panel. The old blue cloud was the most used shape in the category, and
  it drew as a smudge at tray size because the drawing filled only 39% of its
  canvas and sat low in it -- panels scale the canvas, so no setting could have
  made it bigger. The new one fills its frame at every size, and a check now
  measures that for every icon so it cannot quietly shrink again.
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
- **The badges in Files are Cirrove's own.** A check for kept and here, a down
  arrow while the content arrives, both on a disc with a rim so they read over a
  thumbnail of any colour. They were the desktop's generic tick and circular
  arrow, which said nothing and did not match anything else. Where the icons are not installed the generic ones are still used,
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
