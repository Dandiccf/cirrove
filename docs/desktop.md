# Desktop preview

The native GTK4/libadwaita window is an initial account overview for the read-only
development preview. It shows saved OneDrive accounts, connection status, local
mount locations and cache limits. Mount/Unmount changes the saved preference; the
window waits for service acknowledgement before showing the operation as complete.
Closing the window leaves the daemon running.

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
./target/debug/cirrove-desktop --demo --demo-state long-names
./target/debug/cirrove-desktop --demo --demo-details --light
./target/debug/cirrove-desktop --demo --dark --snapshot /tmp/cirrove-preview.png
cargo test -p cirrove-desktop --locked
cargo test -p cirrove-desktop --test window --locked -- --ignored --test-threads=1
```

The ignored window test needs a graphical display and a local Unix socket. It uses
temporary account settings and a synthetic delayed status service, with no cloud
connection. It checks responsiveness, focus preservation and separate desired and
confirmed mount states. CI runs it under Xvfb and a private D-Bus session.
PNG snapshots capture the actual rendered demo window and also require a display.

## Remaining product work

Native browser sign-in, account/library selection, reauthentication, connection
removal and cleanup are not implemented in this window. Tray actions, Nautilus
badges, pinning, transfer/conflict views and localization remain separate work.
The [desktop milestone](product-milestones.md#5-polished-desktop-experience) remains open.

The desktop entry at `packaging/desktop/io.github.Dandiccf.Cirrove.desktop` is a
packaging source. A build does not install it or register file-type associations.
