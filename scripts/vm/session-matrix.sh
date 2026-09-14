#!/usr/bin/env bash
# The session matrix M5 line 265 asks for: native Wayland and X11, GNOME and
# Plasma, with the windows, the system dark-style preference and portal-backed
# folder opening looked at in each.
#
# It runs against the Ubuntu VM, and the reason is the account. Portal folder
# opening is reached from an account's "Open folder" action, so a machine with
# no account cannot show it, and the owner's OneDrive is signed into that VM
# and nowhere else. Ubuntu also ships an X11 session for GNOME, which Fedora 44
# does not, so the X11 half of the matrix has to come from here too.
#
# Usage:
#   scripts/vm/session-matrix.sh ubuntu sessions        # what this machine offers
#   scripts/vm/session-matrix.sh ubuntu plasma-install  # add Plasma, keeping gdm3
#   scripts/vm/session-matrix.sh ubuntu use <id>        # switch session, reboot
#   scripts/vm/session-matrix.sh ubuntu probe [label]   # record the session in front of us
#
# "use" then "probe" is one row of the matrix. Nothing here signs in: the
# account is already in the VM's keyring, and unlock.sh is what makes it
# readable after a boot.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
distro=${1:?ubuntu}
action=${2:?sessions, plasma-install, use or probe}
shift 2 || true
out=${CIRROVE_MATRIX_OUT:-$HOME/Work/cirrove-vms/$distro/matrix}
mkdir -p "$out"
run() { "$here/run.sh" "$distro" "$@"; }
vm() { run ssh "$@"; }
say() { echo "== $*"; }

# Everything that must run inside the logged-in session rather than beside it.
# WAYLAND_DISPLAY and DISPLAY are deliberately left out here: which of the two
# is set is the thing under test, so each probe sets them itself.
bus='XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus'

wait_ssh() {
  for _ in $(seq 1 120); do vm true 2>/dev/null && return 0; sleep 5; done
  echo "the VM did not answer on ssh" >&2; return 1
}

case $action in
  sessions)
    # Session ids are the desktop-entry basenames, and which directory they
    # live in is what decides Wayland or X11 -- not their name. Ubuntu ships
    # an "ubuntu.desktop" in both, which is exactly why this asks rather
    # than assuming.
    vm 'for d in /usr/share/wayland-sessions /usr/share/xsessions; do
          for f in $d/*.desktop; do
            [ -e "$f" ] || continue
            case $d in *wayland*) t=wayland ;; *) t=x11 ;; esac
            printf "  %-8s %-22s %s\n" "$t" "$(basename "$f" .desktop)" \
              "$(sed -n "s/^Name=//p" "$f" | head -1)"
          done
        done'
    ;;

  plasma-install)
    # sddm comes with Plasma and asks, through debconf, to become the display
    # manager. Letting it would cost the autologin that every check here
    # depends on, and it is configured in gdm3. Answer before it asks.
    say "adding Plasma, with gdm3 kept as the display manager"
    vm 'echo "sddm shared/default-x-display-manager select gdm3" | sudo debconf-set-selections
        echo "gdm3 shared/default-x-display-manager select gdm3" | sudo debconf-set-selections
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -y kde-plasma-desktop 2>&1 | tail -5
        echo "display manager now: $(cat /etc/X11/default-display-manager 2>/dev/null)"'
    ;;

  use)
    id=${1:?a session id from the sessions action}
    say "switching the autologin session to $id"
    # AccountsService is where GDM reads the session for an autologin user.
    # Older and newer versions disagree on the key name, so write both; the
    # one that is not understood is ignored rather than rejected.
    vm "sudo install -d /var/lib/AccountsService/users
        sudo tee /var/lib/AccountsService/users/tester >/dev/null <<EOF
[User]
Session=$id
XSession=$id
SystemAccount=false
EOF
        sudo cat /var/lib/AccountsService/users/tester"
    vm 'sudo systemctl reboot' || true
    sleep 20
    wait_ssh
    sleep 25
    "$here/unlock.sh" "$distro" || true
    say "up again; probe it next"
    ;;

  probe)
    label=${1:-$(date +%H%M%S)}
    dir="$out/$label"; mkdir -p "$dir"
    log="$dir/probe.log"
    exec > >(tee -a "$log") 2>&1

    say "what session actually came up"
    # Asking the session rather than trusting what we asked for: a compositor
    # that fails to start falls back, and a fallback that goes unnoticed turns
    # a Wayland row into an X11 row without saying so.
    vm "loginctl show-session \$(loginctl list-sessions --no-legend | awk '{print \$1}' | head -1) \
          -p Type -p Class -p Desktop -p Active 2>/dev/null
        $bus systemctl --user show-environment 2>/dev/null | grep -E 'XDG_CURRENT_DESKTOP|XDG_SESSION_TYPE|WAYLAND_DISPLAY|^DISPLAY' || true"

    say "the account, and the mount"
    vm "cirrove status 2>&1 | head -20"

    say "the tray, on whatever host this session provides"
    vm "$bus busctl --user get-property org.kde.StatusNotifierWatcher /StatusNotifierWatcher \
          org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems 2>&1 | tail -1
        $bus busctl --user get-property io.github.Dandiccf.Cirrove.Tray /StatusNotifierItem \
          org.kde.StatusNotifierItem Status 2>&1 | tail -1"

    say "the system dark-style preference, as the portal reports it"
    vm "$bus busctl --user call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop \
          org.freedesktop.portal.Settings ReadOne ss org.freedesktop.appearance color-scheme 2>&1 | tail -1"

    run shot "$dir/session" >/dev/null
    say "screenshot in $dir/session.png; log in $log"
    ;;

  *) echo "unknown action $action" >&2; exit 1 ;;
esac
