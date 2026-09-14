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
  ubuntu) http_port=8000; pkgdir=pkgs/deb; ssh_port=2222
          # What the packages declare, so the check can ask whether declaring
          # it was enough: fuse3 is a hard Depends, the other two Recommends.
          deps='fuse3 python3-nautilus gnome-shell-extension-appindicator'
          # dpkg-query -W with no format prints name and version, which is
          # all this needs and survives the trip through two shells; a -f
          # format string would not.
          installed='dpkg-query -W' ;;
  fedora) http_port=8001; pkgdir=pkgs/rpm; ssh_port=2223
          deps='fuse3 nautilus-python gnome-shell-extension-appindicator'
          installed='rpm -q' ;;
esac

# Whether each declared dependency is on the machine right now, one line each.
depstate() { vm "for p in $deps; do printf '  %-38s %s\n' \"\$p\" \"\$($installed \$p 2>&1 | head -1)\"; done"; }
# SELinux is a named requirement of M6 line 321 on Fedora. On Ubuntu this
# reports AppArmor's state instead, which is the same question asked of the
# mandatory access control that distribution actually ships.
enforcement() { vm 'getenforce 2>/dev/null || aa-enabled 2>/dev/null || echo "no MAC tool"'; }
denials() { vm 'sudo journalctl --since -10min --no-pager 2>/dev/null | grep -iE "avc: *denied|apparmor=\"DENIED\"" | grep -i cirrove | tail -5 || true'; }

# Wait for sshd, then make sure the host's key is in: the first boot after an
# install has only the password, and the checks run without a terminal.
wait_ssh() {
  for _ in $(seq 1 120); do
    if vm true 2>/dev/null; then
      return 0
    fi
    if nc -z 127.0.0.1 "$ssh_port" 2>/dev/null; then
      run key >/dev/null 2>&1 || true
    fi
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
say "mandatory access control: $(enforcement)"
say "what the packages declare, before installing anything -- absent is the point"
depstate | tee -a "$log"
sleep 20
shot 01-fresh-session

say "install the packages"
# python3 is on every desktop install; curl is not on Ubuntu's.
fetch="python3 -c 'import sys,urllib.request; sys.stdout.write(urllib.request.urlopen(sys.argv[1]).read().decode())'"
names=$(vm "$fetch http://10.0.2.2:$http_port/$pkgdir/ | grep -oE 'href=\"[^\"]+\"' | sed 's/href=\"//;s/\"//' | grep -v debug")
echo "$names" | sed 's/^/  /'
vm "mkdir -p pkgs && cd pkgs && for f in $(echo $names | tr '\n' ' '); do python3 -c 'import sys,urllib.request; urllib.request.urlretrieve(sys.argv[1], sys.argv[2])' http://10.0.2.2:$http_port/$pkgdir/\$f \$f; done && ls -la"
case $distro in
  ubuntu) vm "sudo apt-get install -y ./pkgs/*.deb" | tail -3 ;;
  fedora) vm "sudo dnf install -y ./pkgs/*.rpm" | tail -3 ;;
esac
say "the same dependencies afterwards -- brought in by the packages, not by hand"
depstate | tee -a "$log"
say "files the packages installed"
vm 'ls -la /usr/bin/cirrove /usr/bin/cirroved /usr/bin/cirrove-tray /usr/bin/cirrove-desktop /usr/lib/systemd/user/cirroved.service /etc/xdg/autostart/io.github.Dandiccf.Cirrove.Tray.desktop /usr/share/nautilus-python/extensions/cirrove.py /usr/share/applications/io.github.Dandiccf.Cirrove.desktop /usr/share/metainfo/io.github.Dandiccf.Cirrove.metainfo.xml' | awk '{print "  "$5, $9}'
say "the desktop entries and metainfo, from where they are installed"
vm 'desktop-file-validate /usr/share/applications/io.github.Dandiccf.Cirrove.desktop && desktop-file-validate /etc/xdg/autostart/io.github.Dandiccf.Cirrove.Tray.desktop && (appstreamcli validate /usr/share/metainfo/io.github.Dandiccf.Cirrove.metainfo.xml 2>&1 | tail -1 || true)'

say "start the service as the user"
vm "$session_env systemctl --user enable --now cirroved.service && sleep 3 && $session_env systemctl --user is-active cirroved.service && cirrove status | head -c 400"
echo

# GNOME shows a StatusNotifierItem only through the AppIndicator extension.
# Ubuntu's session enables its own copy; on Fedora the package is installed
# and the user enables it, which is what the user guide says and what this
# does here in the user's place.
say "the tray extension: $(vm "$session_env gnome-extensions list --enabled 2>/dev/null | grep -i indicator || ($session_env gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com 2>&1 && echo 'enabled appindicatorsupport@rgcjonas.gmail.com')")"
sleep 3
say "start the tray the way the autostart entry will, and see the shell take it"
vm "$session_env systemd-run --user --collect /usr/bin/cirrove-tray >/dev/null 2>&1; sleep 4; pgrep -a cirrove-tray | cut -c1-60; $session_env busctl --user get-property org.kde.StatusNotifierWatcher /StatusNotifierWatcher org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems 2>&1 | tail -1; $session_env busctl --user get-property io.github.Dandiccf.Cirrove.Tray /StatusNotifierItem org.kde.StatusNotifierItem IconName 2>&1 | tail -1; echo \"status: \$($session_env busctl --user get-property io.github.Dandiccf.Cirrove.Tray /StatusNotifierItem org.kde.StatusNotifierItem Status 2>&1 | tail -1) -- Passive with no accounts, which a shell hides; the icon appears once an account is connected\"; $session_env gnome-extensions list --enabled 2>/dev/null | grep -i indicator || echo '  (no appindicator extension enabled)'"
sleep 3
say "denials while starting the service and the tray: $(denials | sed 's/^/    /' | head -5)"
say "  (empty above means none mentioning cirrove in the last ten minutes)"
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
say "denials since the reboot: $(denials | sed 's/^/    /' | head -5)"
shot 04-after-reboot

say "remove the packages and check nothing package-owned survived"
case $distro in
  ubuntu) vm 'sudo apt-get purge -y cirrove-desktop cirrove' | tail -2 ;;
  fedora) vm 'sudo dnf remove -y cirrove-desktop cirrove' | tail -2 ;;
esac
# Naming the files to look for is how the switch-to-package script came to
# leave eight icons, a desktop entry and a metainfo file behind: a list you
# write by hand only finds what you remembered to write down. Ask the
# filesystem instead, and let it name anything it still has.
vm 'find /usr /etc -iname "*cirrove*" -not -path "*/nautilus-python/__pycache__/*" 2>/dev/null | sed "s/^/  SURVIVED: /"; echo "  state directory kept: $(test -d ~/.local/state/cirrove && echo yes || echo "none existed")"'

say "done; screenshots and log in $out"
run stop >/dev/null
