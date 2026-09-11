# Desktop preview

The native GTK4/libadwaita window is an initial account overview for the read-only
development preview. It shows saved OneDrive accounts, connection status, local
mount locations and cache limits. Mount/Unmount changes the saved preference; the
window waits for service acknowledgement before showing the operation as complete.
Closing the window leaves the daemon running.

The window preserves safe failure causes when loading settings and service status.
It distinguishes unreadable settings (including denied access), invalid settings
data with its line/column, unsupported configuration, a missing/refusing service,
denied service access, response timeout, malformed response and a protocol-version
mismatch. Settings and service failures can appear together. Retry refreshes both;
a recovered snapshot clears the warnings and restores confirmed controls. Raw
settings values, error messages and service response bodies are not displayed.

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
upgrade a daemon, and does not change an account's read/write permission.
New connections and renewed sign-in still use the [OneDrive setup flow](onedrive-setup.md).
“Sign in again” currently reports an account state; it is not yet a native sign-in action.

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
registration with it and looks the same on the bus. There is still no menu, so a
left click opens the mount when exactly one is mounted and otherwise does
nothing, and icons are generic freedesktop names until the installed icon set
exists.

Only one copy runs. The tray takes the well-known name
`io.github.Dandiccf.Cirrove.Tray`, and a second copy exits 0 saying so. That is
what makes shipping two autostart mechanisms safe rather than a double-start bug.

A right click opens a menu, served as `com.canonical.dbusmenu` at `/MenuBar`:
each account with its state, then the settings window and quitting the tray. A
left click still opens the mount directly, so the gesture that worked before
still does.

What the menu may hold is constrained by the milestone rather than by taste --
*"no control or state may be reachable only through a tray or only through one
file manager"* -- because a tray host is absent on stock GNOME and on any bare
window manager. Every entry is a shortcut to something the window also does.
Mount and unmount are therefore missing rather than forgotten: the window changes
them by writing the account settings, and a second process doing the same is a
race over one file. They belong here once the daemon has a verb for them, which
is the argument that put pinning behind the control socket.

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
  `/etc/xdg/autostart/` equivalent. This is the default and the broadest:
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

Native browser sign-in, account/library selection, reauthentication, connection
removal and cleanup are not implemented in this window. Tray actions, Nautilus
badges, pinning, transfer/conflict views and localization remain separate work.

The daemon can now push changes rather than only answer questions: `capabilities`
names what it supports and `subscribe` streams account and mount changes over the
control socket. See [ADR 0007](adr/0007-desktop-event-channel.md). This window
does not consume it yet; `cirrove-tray` does.

Mount-preference save failures and folder-opening failures still use generic
messages; account repair, service installation and reauthentication actions are
not yet provided by these diagnostics.
The [desktop milestone](product-milestones.md#5-polished-desktop-experience) remains open.

The desktop entry at `packaging/desktop/io.github.Dandiccf.Cirrove.desktop` is a
packaging source. A build does not install it or register file-type associations.
