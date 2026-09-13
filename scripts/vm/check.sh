#!/usr/bin/env bash
# The real-desktop checks, run against an installed VM from scripts/vm/run.sh:
# install the packages CI built, start the service, look at the session --
# the tray registered with the shell, the Files extension loaded -- reboot,
# look again, remove the packages, and check nothing package-owned survived.
# Screenshots land next to the log so a person can look at what the checks
# looked at.
#
# What this cannot do is sign in: a Microsoft sign-in is a person's browser
# and credentials. The VM's console (vnc) is where that happens when it does,
# and the check reports the service as having no accounts, which is the
# truthful state of a fresh install.
#
# Usage: scripts/vm/check.sh ubuntu|fedora [output-dir]
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
distro=${1:?ubuntu or fedora}
out=${2:-$HOME/Work/cirrove-vms/$distro/checks}
mkdir -p "$out"
log="$out/check.log"
run() { "$here/run.sh" "$distro" "$@"; }
vm() { run ssh "$@"; }
shot() { run shot "$out/$1" >/dev/null; echo "  screenshot $1.png"; }
say() { echo "== $*" | tee -a "$log"; }

case $distro in
  ubuntu) http_port=8000; pkgdir=pkgs/deb ;;
  fedora) http_port=8001; pkgdir=pkgs/rpm ;;
esac

wait_ssh() {
  for _ in $(seq 1 120); do
    vm true 2>/dev/null && return 0
    sleep 5
  done
  echo "the VM did not answer on ssh" >&2
  return 1
}
# The session's bus and display, for commands that must run inside it.
session_env='XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus WAYLAND_DISPLAY=wayland-0 DISPLAY=:0'

say "boot"
run boot >/dev/null
wait_ssh
say "installed: $(vm 'cat /etc/os-release | grep PRETTY_NAME')"
sleep 20
shot 01-fresh-session

say "install the packages"
names=$(vm "curl -s http://10.0.2.2:$http_port/$pkgdir/ | grep -oE 'href=\"[^\"]+\"' | sed 's/href=\"//;s/\"//' | grep -v debug")
echo "$names" | sed 's/^/  /'
vm "mkdir -p pkgs && cd pkgs && for f in $(echo $names | tr '\n' ' '); do curl -sO http://10.0.2.2:$http_port/$pkgdir/\$f; done && ls -la"
case $distro in
  ubuntu) vm "sudo apt-get install -y ./pkgs/*.deb" | tail -3 ;;
  fedora) vm "sudo dnf install -y ./pkgs/*.rpm" | tail -3 ;;
esac
say "files the packages installed"
vm 'ls -la /usr/bin/cirrove /usr/bin/cirroved /usr/bin/cirrove-tray /usr/bin/cirrove-desktop /usr/lib/systemd/user/cirroved.service /etc/xdg/autostart/io.github.Dandiccf.Cirrove.Tray.desktop /usr/share/nautilus-python/extensions/cirrove.py /usr/share/applications/io.github.Dandiccf.Cirrove.desktop /usr/share/metainfo/io.github.Dandiccf.Cirrove.metainfo.xml' | awk '{print "  "$5, $9}'
say "the desktop entries and metainfo, from where they are installed"
vm 'desktop-file-validate /usr/share/applications/io.github.Dandiccf.Cirrove.desktop && desktop-file-validate /etc/xdg/autostart/io.github.Dandiccf.Cirrove.Tray.desktop && (appstreamcli validate /usr/share/metainfo/io.github.Dandiccf.Cirrove.metainfo.xml 2>&1 | tail -1 || true)'

say "start the service as the user"
vm "$session_env systemctl --user enable --now cirroved.service && sleep 3 && $session_env systemctl --user is-active cirroved.service && cirrove status | head -c 400"
echo

say "start the tray the way the autostart entry will, and see the shell take it"
vm "$session_env systemd-run --user --collect /usr/bin/cirrove-tray >/dev/null 2>&1; sleep 4; pgrep -a cirrove-tray | cut -c1-60; $session_env busctl --user get-property org.kde.StatusNotifierWatcher /StatusNotifierWatcher org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems 2>&1 | tail -1; $session_env busctl --user get-property io.github.Dandiccf.Cirrove.Tray /StatusNotifierItem org.kde.StatusNotifierItem IconName 2>&1 | tail -1; $session_env gnome-extensions list --enabled 2>/dev/null | grep -i indicator || echo '  (no appindicator extension enabled)'"
sleep 3
shot 02-tray-in-session

say "Files with the extension"
vm "$session_env systemd-run --user --collect --setenv=CIRROVE_NAUTILUS_DEBUG=1 nautilus /home/tester >/dev/null 2>&1; sleep 6; journalctl --user --since -1min --no-pager 2>/dev/null | grep -i 'cirrove:' | tail -3 || true"
shot 03-files-open

say "reboot, and look again"
vm 'sudo systemctl reboot' || true
sleep 15
wait_ssh
sleep 25
vm "pgrep -a cirrove-tray | cut -c1-60 || echo '  tray not running after login'; $session_env systemctl --user is-active cirroved.service || true; $session_env busctl --user get-property io.github.Dandiccf.Cirrove.Tray /StatusNotifierItem org.kde.StatusNotifierItem IconName 2>&1 | tail -1"
shot 04-after-reboot

say "remove the packages and check nothing package-owned survived"
case $distro in
  ubuntu) vm 'sudo apt-get purge -y cirrove-desktop cirrove' | tail -2 ;;
  fedora) vm 'sudo dnf remove -y cirrove-desktop cirrove' | tail -2 ;;
esac
vm 'for f in /usr/bin/cirroved /usr/bin/cirrove /usr/bin/cirrove-tray /usr/bin/cirrove-desktop /usr/lib/systemd/user/cirroved.service /etc/xdg/autostart/io.github.Dandiccf.Cirrove.Tray.desktop /usr/share/nautilus-python/extensions/cirrove.py; do test -e "$f" && echo "  SURVIVED: $f"; done; echo "  state directory kept: $(test -d ~/.local/state/cirrove && echo yes || echo "none existed")"'

say "done; screenshots and log in $out"
run stop >/dev/null
