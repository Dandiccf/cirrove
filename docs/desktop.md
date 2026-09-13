# Desktop

The native GTK4/libadwaita window is where accounts are managed without a
terminal. It shows each saved OneDrive connection with its state, its Microsoft
account, whether the grant allows changes, where it appears in Files and its
cache limit, and it offers every action an account can need, each only where it
applies:

- **Connect a drive** (the `+` in the header, or the button on the empty page):
  a name, a folder, the app registration's client id -- prefilled from an
  existing account, since it is the one thing nobody remembers -- and whether
  changes are allowed. Sign-in happens in the browser; the window then lists the
  drives the account can see and saves the connection when one is chosen. It is
  the CLI's `connect` in two steps, `accounts::begin_connect` and
  `PendingConnection::finish`, against the same service code; closing the
  dialog before choosing a drive forgets the grant.
- **Mount / Unmount** changes the saved preference; the window waits for the
  service to acknowledge before showing the operation as complete, and says so
  when the service takes longer than it should.
- **Sign in again** appears on an account whose grant stopped working and runs
  `accounts::reauthenticate`: the account is released while the browser asks,
  and taken back after.
- **Changes the cloud refused** appears when the daemon has given up on
  changes the mount made locally that the provider never took, with a count and
  **Discard**, which asks the daemon (`discard-stuck`) to abandon them so the
  mount shows what the cloud actually has. Nothing is re-sent: the conflict
  means the remote moved, and a stale retry would act on whatever is there now.
- **Remove** takes a connection out of Cirrove. Only an unmounted one: removal
  under a running mount would race it, and the button says "Unmount before
  removing" rather than letting the daemon refuse later. The confirmation
  differs when the account still holds changes that never reached the cloud --
  those are the user's, and removing the connection is the one way to lose them,
  so it says how many and what the alternative is. What is removed is set aside
  under the state directory, not deleted; nothing in the cloud is touched.

- **Recent activity**, at the bottom, when there is any: what arrived,
  changed or went away in the cloud since the service started, and what was
  saved here with where it is on its way -- waiting, uploading, in the cloud,
  or refused, the refused ones marked. Changes to content, not files merely
  opened. The daemon answers `recent` per account (the delta feed's
  deliveries from the second delta on, so opening an account does not
  "recently change" every file it has, and the journal's latest saves); the
  window asks with every refresh, the tray asks after every account event and
  shows five lines under the account's entry, and `cirrove recent` prints the
  same on the command line.

Closing the window leaves the daemon running. One operation runs at a time,
named in the row it belongs to.

The window preserves safe failure causes when loading settings and service status.
It distinguishes unreadable settings (including denied access), invalid settings
data with its line/column, unsupported configuration, a missing/refusing service,
denied service access, response timeout, malformed response and a protocol-version
mismatch. Settings and service failures can appear together. Retry refreshes both;
a recovered snapshot clears the warnings and restores confirmed controls. Raw
settings values, error messages and service response bodies are not displayed.

`crates/cirrove-desktop/tests/window.rs` drives the window against a daemon that
is only a socket: the focus-and-acknowledgement scenario, and one that checks
each action is offered exactly where it applies and that Discard reaches the
daemon. GTK binds to the first thread that initialises it and libtest gives
every test its own, so that file is its own small harness, running the
scenarios in order on one thread; CI runs it under Xvfb.

## Run

Build with the [desktop dependencies](../README.md#build-and-try) installed:

```sh
cargo build --workspace --locked
./target/debug/cirroved
```

In another terminal, run `./target/debug/cirrove-desktop`. For isolated development,
pass both `--state-dir /absolute/private/state` and `--socket /absolute/control.sock`
to the desktop and use those same paths for the daemon. The desktop needs a daemon
built with status protocol 1; an older running daemon shows “Service update required”.
Building the source alone does not update an installed service.

The default paths are the same as the CLI. The window does not start, install or
upgrade a daemon, and does not change an existing account's read/write
permission -- that is `cirrove reauth --write-access`, which discards nothing
but is a consent change worth typing. The app registration itself is still
created by hand, see the [OneDrive setup flow](onedrive-setup.md); the window
asks for its client id.

## Synthetic interface preview and tests

`./target/debug/cirrove-desktop --demo` uses built-in fictional accounts. Mount
buttons simulate changes in memory, and opening folders or the setup guide is
disabled. No real settings, keyring, service or cloud provider is used.

```sh
./target/debug/cirrove-desktop --demo --demo-state service-offline
./target/debug/cirrove-desktop --demo --demo-state service-timeout
./target/debug/cirrove-desktop --demo --demo-state incompatible-service
./target/debug/cirrove-desktop --demo --demo-state invalid-settings
./target/debug/cirrove-desktop --demo --demo-state long-names
./target/debug/cirrove-desktop --demo --demo-details --light
./target/debug/cirrove-desktop --demo --dark --snapshot /tmp/cirrove-preview.png
cargo test -p cirrove-desktop --locked
cargo test -p cirrove-desktop --test window --locked -- --ignored --test-threads=1
```

The ignored window test needs a graphical display and a local Unix socket. It uses
temporary account settings and a synthetic delayed status service, with no cloud
connection. It checks responsiveness, focus preservation and separate desired and
confirmed mount states, visible failure causes and recovery through Retry. CI runs
it under Xvfb and a private D-Bus session. Local socket fixtures distinguish invalid
responses from timeouts; neither requires a cloud account.
PNG snapshots capture the actual rendered demo window and also require a display.

## Tray

`./target/debug/cirrove-tray` publishes a status icon as a StatusNotifierItem on
the session bus and follows the daemon over `subscribe`. It holds no state it was
not told: the icon, the tooltip and the `Status` property are the daemon's answer
and nothing inferred from it. Losing the service is rendered rather than hidden,
because a tray that keeps its last icon after the daemon is gone is reporting
something it does not know.

```sh
./target/debug/cirrove-tray                      # the usual socket
./target/debug/cirrove-tray --socket /abs/control.sock
```

It needs a tray host implementing `org.kde.StatusNotifierWatcher`. Most Wayland
shells and panels provide one; GNOME needs an extension. A missing host is not a
failure to start: the tray publishes its item, waits, and registers whenever a
host appears -- which also covers a panel being restarted, since that takes every
registration with it and looks the same on the bus. A missing host is said out
loud once -- naming the interface and the GNOME extension -- because tolerating
it silently is how a user on stock GNOME ends up with no icon and nothing
anywhere saying why. Once, not on every change: a flaky panel must not fill a
journal.

The icons are Cirrove's own: a cloud for ready, a cloud with transfer arrows for
working, a cloud with an exclamation mark for anything a person must act on.
Three icons rather than overlays, because not every host draws overlays. They
ship in `packaging/icons/` for a package to install into the hicolor theme, and
the tray does not depend on that having happened: it embeds the same files,
writes them under `$XDG_RUNTIME_DIR/cirrove-tray/icons` at start, and advertises
that directory as `IconThemePath`, so a tray run from a bare build draws
correctly too. The first tray in this project registered, declared every
property, and drew nothing because the icon it named did not exist; this is the
version of that lesson that survives a rename, and
`packaging::every_icon_the_tray_names_ships_as_a_file` reads the names out of
the code and checks a file exists for each.

Only one copy runs. The tray takes the well-known name
`io.github.Dandiccf.Cirrove.Tray`, and a second copy exits 0 saying so. That is
what makes shipping two autostart mechanisms safe rather than a double-start bug.

A right click opens a menu, served as `com.canonical.dbusmenu` at `/MenuBar`:
each account with its state as a submenu holding *Open folder* and
*Mount*/*Unmount*, then the settings window and quitting the tray. A left click
opens the mount when exactly one is mounted and otherwise does nothing, which is
the gesture that worked before the menu existed.

Mounting from the tray changes the saved preference through
`accounts::set_enabled_by_id` -- the same call the window and the CLI make, which
holds the configuration lock and the per-account operation lock. This was
described for a while as needing a daemon control verb first, on the grounds that
a second writer would race the window over one file. That was wrong: the locks
were already there and a third caller is serialised by them like any other.
Acknowledgement is free, because the tray is subscribed and the daemon's mount
event redraws the row.

A failure reaches the icon rather than only a log. Everything else the tray shows
is the daemon's answer and nothing inferred from it; this is the one exception
and it is not an inference -- the tray asked, the call failed, and the tray is
the only thing that saw it. The icon goes to `NeedsAttention`, the tooltip leads
with what to do about it, and the next change the daemon reports clears it,
because a notice that outlived its cause keeps the icon shouting after the user
already fixed it somewhere else.

What the menu may hold is constrained by the milestone rather than by taste --
*"no control or state may be reachable only through a tray or only through one
file manager"* -- because a tray host is absent on stock GNOME and on any bare
window manager. Every entry is a shortcut to something the window also does, and
mounting qualifies for exactly that reason.

## Files

`packaging/nautilus/cirrove.py` is a nautilus-python extension. With it, Files
shows what Cirrove keeps offline and offers to change it:

- a badge on every pinned file and folder -- a check when the content is on
  disk, the synchronising emblem while a pin is still being fetched -- and none
  on the ordinary on-demand file, because badging every file in a cloud drive
  would say nothing;
- a "Cirrove" column (View → Visible Columns) with the words: On demand, Kept
  offline, Kept offline (folder) for a pin inherited from a folder above, or
  the fetch percentage;
- **Keep offline** / **Stop keeping offline** in the context menu of a
  selection or a folder background, recursive for folders. Un-keeping is
  offered only when every selected item is pinned in its own right: an
  inherited pin belongs to the folder above, which the user did not select.

It speaks to the daemon the way the CLI does: `status` for the mounts (trusted
for five seconds), `paths` for each entry's kind, pin cover and bytes on disk
-- the daemon answers up to 200 paths at once, and the menu asks that way; the
badges go one at a time because Files asks about a listing's entries one at a
time and waits for each -- and `pin`/`unpin` for the menu. Every call has a
three-second timeout; a daemon that is not there, or an older one without
`paths`, draws nothing rather than stalling Files. `cirrove paths <account>
<paths...>` shows the same answers on the command line.

Two things about Files that the first version of this got wrong, kept here
because the second version of any extension will meet them again. The badge
work must go through GLib's asynchronous socket I/O, not Python threads: the
interpreter inside Files only runs while the main thread is in a Python
callback, so a thread started there runs for a moment and then stands still,
reply in hand. And the completion must name the provider object Files handed
to `update_file_info_full`, not the Python `self` -- they wrap the same
object, and a completion under the wrong one leaves the listing waiting on its
first file forever, which looks exactly like an extension that was never
called.

**Supported file managers.** Files (Nautilus 43 and later, through
nautilus-python 4) is the supported one. Dolphin is decided, not done: it gets
the same two verbs through a KIO/Dolphin plugin when a KDE session is in the
declared session matrix, and not before -- a plugin nobody runs a session for
is a plugin nobody has seen. Nemo and Caja take nautilus-python-style
extensions with older APIs and are not targeted; the daemon contract is the
same for all of them, so none of this is a daemon decision.

The package installs it under `/usr/share/nautilus-python/extensions/`; for
development, copy it to `~/.local/share/nautilus-python/extensions/` and
restart Files with `nautilus -q`. `scripts/test-nautilus-extension.py` drives
its functions against a daemon that is only a socket; the classes Files calls
are the thin part on top.

## Ejecting the mount

A user cannot eject it from a file manager while the daemon is running. Both
`fusermount3 -u` and `gio mount -u` -- which is what a file manager's Eject
calls -- refuse with "target is busy", and no user process holds it: `fuser -mv`
reports only the kernel mount.

That is the daemon doing its job rather than a defect. It is the FUSE server; a
mount yanked out from under it would leave it serving something detached. The
supported way is to change the desired state -- `cirrove disable`, the window's
Mount toggle, or Unmount in the tray menu -- and the daemon then unmounts
cleanly, measured at about two seconds from the command to `fuser::mnt:
Unmounting` in its log.

What is a defect is what the user sees. "Target is busy" names nothing they can
act on and no process they could close, so the reasonable conclusion is that the
eject silently failed or that the drive re-mounted itself immediately. Both were
reported by the first person to try it. Nothing in the product says where the
Unmount that works actually lives.

## Starting it at login

Two mechanisms ship, because neither covers every desktop and a package cannot
find out which applies. It is installed once, system-wide, before any session
exists, and the same machine can have several users on different desktops; a
choice made at install time is wrong at the next login. `docs/distribution.md`
already forbids package scripts from assuming a desktop, a shell, a home
directory or a session bus, so the files stay standard and the binary absorbs the
variation.

- `packaging/desktop/io.github.Dandiccf.Cirrove.Tray.desktop` -- the XDG
  autostart entry, installed to `~/.config/autostart/` or an
  `/etc/xdg/autostart/` equivalent. **A user-local install must rewrite `Exec=`
  to an absolute path.** The file ships `Exec=cirrove-tray`, which is right for a
  package that puts the binary in `/usr/bin`, and silently wrong for one in
  `~/.local/bin`: `systemd-xdg-autostart-generator` resolves `Exec=` against its
  own PATH at session start, does not find it, logs "executable specified in
  Exec= does not exist", and writes no unit. Nothing reaches the user and the
  tray simply never appears. Measured on a real login, not inferred.

  `scripts/install-tray-autostart.py` does this correctly: it resolves the binary
  against the generator's own PATH rather than the shell's, writes a bare name
  when that resolves and an absolute path when it does not, and then runs the
  generator to confirm a unit comes out rather than assuming one will. `--check`
  reports without changing anything and `--remove` undoes it. This is the default and the broadest:
  GNOME, KDE, XFCE, LXQt, Cinnamon and MATE run it, `systemd-xdg-autostart-generator`
  turns it into a unit where systemd is present, and it works on distributions
  that have no systemd at all. It names no desktop in `OnlyShowIn`/`NotShowIn`,
  deliberately.
- `packaging/systemd/cirrove-tray.service` -- optional, for users who want
  `systemctl --user` control. It is `WantedBy=graphical-session.target` and
  `PartOf=` it, which is the correct lifetime and the reason this is a second
  unit rather than a flag on `cirroved`: the daemon holds a mount and must
  outlive any session, the tray has nothing to draw on without one. Not enabled
  by default, because `graphical-session.target` is not reached on every desktop.

Enabling both is harmless; the second copy stands down.

What neither covers is a bare window manager with no session manager -- plain
Hyprland without uwsm, i3, dwm -- which runs no XDG autostart at all. There the
user adds the command to their own configuration. That is a documented limit, not
something the packaging can solve, and milestone 5's session-matrix box exists to
record which combinations have actually been checked rather than assumed.

Against a daemon too old for `subscribe` the tray shows "Cirrove service is not
reachable" and retries. That is the intended degradation, not a defect: the old
daemon answers the verb with its ordinary refusal and is otherwise unaffected.

## Remaining product work

Transfer progress with cancellation, Dolphin, and localization remain
separate work.

The daemon can now push changes rather than only answer questions: `capabilities`
names what it supports and `subscribe` streams account and mount changes over the
control socket. An event a client does not recognise is skipped rather than
ending the stream, so a newer daemon never breaks an older tray and adding an
event costs no protocol break.

`status` also reports `stuck_changes`: namespace changes the daemon has given up
on, each one a change the mount already made locally that the provider never
took. `cirrove discard-stuck` abandons them, so the mount shows what the cloud
actually has and the user can decide again; it never re-sends anything, because
the conflict means the remote moved and a stale retry would act on whatever is
there now. The window shows the count with a Discard button; the tray shows it
in the icon, the tooltip and the menu. See [ADR 0007](adr/0007-desktop-event-channel.md).
The window polls `status`; `cirrove-tray` consumes the event stream.

Mount-preference save failures and folder-opening failures still use generic
messages, and service installation is not an action in the window.
The [desktop milestone](product-milestones.md#5-polished-desktop-experience) remains open.

The desktop entry at `packaging/desktop/io.github.Dandiccf.Cirrove.desktop` is a
packaging source. A build does not install it or register file-type associations.
