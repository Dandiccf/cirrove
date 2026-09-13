#!/usr/bin/env bash
# Move this user from a developer install to the installed packages.
#
# A developer install puts cirroved in ~/.local/bin, its unit in
# ~/.config/systemd/user and the tray's autostart entry in ~/.config/autostart.
# Each of those shadows the packaged file at the same name, so after
# `pacman -U` the packages are installed and nothing runs from them. This
# removes the three shadows and starts the packaged unit. Run it after the
# packages are installed, not before: that way there is no moment with nothing
# installed at all.
#
# Accounts, credentials and the journal under ~/.local/state/cirrove are not
# touched. The mount goes away for the seconds between stopping one daemon and
# starting the other; close files in it first.
set -euo pipefail

for binary in cirroved cirrove cirrove-tray cirrove-desktop; do
  command -v "/usr/bin/$binary" >/dev/null \
    || { echo "/usr/bin/$binary is not installed; install the packages first (docs/distribution.md#arch)" >&2; exit 1; }
done
test -f /usr/lib/systemd/user/cirroved.service \
  || { echo "the packaged unit is not installed" >&2; exit 1; }

user_unit="$HOME/.config/systemd/user/cirroved.service"
if [[ -f $user_unit ]]; then
  echo "stopping the developer unit and removing $user_unit"
  systemctl --user disable --now cirroved.service || true
  rm -f "$user_unit"
fi
for binary in cirroved cirrove cirrove-tray cirrove-desktop; do
  if [[ -e $HOME/.local/bin/$binary ]]; then
    echo "removing ~/.local/bin/$binary (the package provides /usr/bin/$binary)"
    rm -f "$HOME/.local/bin/$binary"
  fi
done
autostart="$HOME/.config/autostart/io.github.Dandiccf.Cirrove.Tray.desktop"
if [[ -f $autostart ]]; then
  echo "removing $autostart (the package provides the /etc/xdg/autostart entry)"
  rm -f "$autostart"
fi
extension="$HOME/.local/share/nautilus-python/extensions/cirrove.py"
if [[ -f $extension ]]; then
  # Files loads both copies otherwise, and every menu item and badge appears twice.
  echo "removing $extension (the package provides the one under /usr/share)"
  rm -f "$extension"
  nautilus -q 2>/dev/null || true
fi
pkill -x cirrove-tray || true

systemctl --user daemon-reload
systemctl --user enable --now cirroved.service
echo "the packaged service is running:"
systemctl --user show cirroved.service -p FragmentPath -p ExecStart -p ActiveState | sed 's/^/  /'
# Start the tray the way the next login will, from the packaged entry.
(setsid nohup /usr/bin/cirrove-tray >/dev/null 2>&1 &)
echo "the tray is started from /usr/bin; at the next login the autostart entry does it"
