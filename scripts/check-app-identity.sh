#!/usr/bin/env bash
# Does the window a shell shows match the application entry it should launch?
#
# Naming consistency across the desktop entry, the icon, the metainfo and the
# D-Bus name is asserted in CI from the files. What CI cannot see is the
# association a shell actually makes at runtime: a window whose app id does not
# match the entry gets a generic icon in the dock, no launcher grouping, and no
# "pin to dock" that points anywhere. That is only visible on a running shell,
# so this asks the shell.
#
# On Wayland the match is by app id. On X11 it is by WM_CLASS, and the entry
# needs StartupWMClass when the two differ.
#
# Usage: scripts/check-app-identity.sh      (with the window open, or it opens one)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
# The application's own entry, not the tray's autostart entry beside it.
entry=packaging/desktop/io.github.Dandiccf.Cirrove.desktop
id=$(basename "$entry" .desktop)
echo "the entry declares: $id"

opened=0
if ! pgrep -x cirrove-desktop >/dev/null; then
  command -v cirrove-desktop >/dev/null || { echo "cirrove-desktop is not installed" >&2; exit 1; }
  setsid nohup cirrove-desktop >/dev/null 2>&1 </dev/null &
  opened=1
  sleep 5
fi
trap '[[ $opened == 1 ]] && pkill -x cirrove-desktop || true' EXIT

found=""
if command -v hyprctl >/dev/null && hyprctl -j clients >/dev/null 2>&1; then
  found=$(hyprctl -j clients | python3 -c "
import json,sys
for c in json.load(sys.stdin):
    if c.get('title','').startswith('Cirrove'):
        print(c.get('class',''), 'xwayland' if c.get('xwayland') else 'wayland')
        break
")
elif command -v swaymsg >/dev/null; then
  found=$(swaymsg -t get_tree | python3 -c "
import json,sys
def walk(n):
    if n.get('name','').startswith('Cirrove') and (n.get('app_id') or n.get('window_properties')):
        print(n.get('app_id') or n['window_properties'].get('class',''),
              'wayland' if n.get('app_id') else 'x11')
        return True
    return any(walk(c) for c in n.get('nodes',[]) + n.get('floating_nodes',[]))
walk(json.load(sys.stdin))
")
elif command -v xprop >/dev/null && [[ -n ${DISPLAY:-} ]]; then
  found="$(xprop -name Cirrove WM_CLASS 2>/dev/null | sed 's/.*= //') x11"
else
  echo "no supported shell query here (hyprctl, swaymsg or xprop on X11)" >&2
  exit 2
fi

[[ -n $found ]] || { echo "no Cirrove window found; is it open?" >&2; exit 1; }
reported=${found%% *}
session=${found##* }
echo "the shell reports : $reported ($session)"
if [[ $reported == "$id" ]]; then
  echo "match: the shell will use this application's entry, icon and grouping"
else
  echo "MISMATCH: the shell will not associate this window with $id." >&2
  [[ $session == x11 ]] && echo "On X11 the entry needs StartupWMClass=$reported." >&2
  exit 1
fi
