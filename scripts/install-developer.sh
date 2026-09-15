#!/usr/bin/env bash
# Install Cirrove into this user's home, from a build of this tree.
#
# The counterpart to scripts/switch-to-package.sh. A packaged install is what a
# release delivers and CI and the VMs prove it; a developer install is what a
# machine that follows this tree wants, because it needs no root and can be
# updated as often as the tree changes. The two must never be installed at
# once: Files loads extensions from both /usr/share and ~/.local/share, icons
# in the home shadow the packaged ones, and everything appears twice. So this
# refuses to run while the packages are installed and says how to remove them.
#
# Nothing here touches ~/.local/state/cirrove -- accounts, credentials, the
# index, the cache and unsent bytes stay exactly as they are.
#
# Usage: scripts/install-developer.sh [--no-build]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
id=io.github.Dandiccf.Cirrove
bin="$HOME/.local/bin"
icons="$HOME/.local/share/icons/hicolor"
ext="$HOME/.local/share/nautilus-python/extensions"
unit="$HOME/.config/systemd/user/cirroved.service"

if pacman -Qq cirrove >/dev/null 2>&1 || pacman -Qq cirrove-desktop >/dev/null 2>&1; then
  echo "The Cirrove packages are installed; a home install on top of them would load" >&2
  echo "everything twice. Remove them first:" >&2
  echo "    sudo pacman -Rns cirrove cirrove-desktop" >&2
  exit 1
fi

if [[ ${1:-} != --no-build ]]; then
  echo "building"
  (cd "$repo" && cargo build --release --locked --workspace)
fi

echo "installing into $HOME"
install -Dm755 "$repo/target/release/cirroved" "$bin/cirroved"
install -Dm755 "$repo/target/release/cirrove" "$bin/cirrove"
install -Dm755 "$repo/target/release/cirrove-desktop" "$bin/cirrove-desktop"
install -Dm755 "$repo/target/release/cirrove-tray" "$bin/cirrove-tray"

# The unit template runs %h/.local/bin/cirroved, which is exactly right here.
install -Dm644 "$repo/packaging/systemd/cirroved.service" "$unit"

install -Dm644 "$repo/packaging/desktop/$id.desktop" -t "$HOME/.local/share/applications/"
install -Dm644 "$repo/packaging/metainfo/$id.metainfo.xml" -t "$HOME/.local/share/metainfo/"

# Translations. The window looks for its catalogue beside its own binary, so
# ~/.local/bin/cirrove-desktop finds ~/.local/share/locale with nothing set.
if command -v msgfmt >/dev/null; then
  for po in "$repo"/po/*.po; do
    lang=$(basename "$po" .po)
    install -d "$HOME/.local/share/locale/$lang/LC_MESSAGES"
    msgfmt --check -o "$HOME/.local/share/locale/$lang/LC_MESSAGES/cirrove.mo" "$po"
  done
  echo "installed translations: $(ls "$repo"/po/*.po | wc -l) language(s)"
else
  echo "note: msgfmt is not installed, so the window stays English" >&2
fi
install -Dm644 "$repo"/packaging/icons/scalable/apps/*.svg -t "$icons/scalable/apps/"
install -Dm644 "$repo"/packaging/icons/symbolic/apps/*.svg -t "$icons/symbolic/apps/"
gtk-update-icon-cache -f "$icons" >/dev/null 2>&1 || true
install -Dm644 "$repo/packaging/nautilus/cirrove.py" -t "$ext/"

# The tray's autostart entry, with Exec= resolved the way the systemd XDG
# autostart generator needs for ~/.local/bin. See the script's own comments.
python3 "$repo/scripts/install-tray-autostart.py"

systemctl --user daemon-reload
systemctl --user enable cirroved.service
# restart, not `enable --now`: --now does nothing to an already-running daemon,
# so a second developer install would leave the previous binary serving the
# mount and report success. Every install must put the binary it just built in
# front of the user. The unit unmounts on stop and remounts on start; the state
# directory is untouched, so the account and index survive.
systemctl --user restart cirroved.service

# Replace a running tray with the one just installed, and reload Files so it
# picks up the extension.
pkill -x cirrove-tray 2>/dev/null || true
sleep 1
(setsid nohup "$bin/cirrove-tray" >/dev/null 2>&1 &)
nautilus -q 2>/dev/null || true

# Wait for the mount to be served again before reporting, so the line below is
# the truth and not a hope.
for _ in $(seq 40); do
  "$bin/cirrove" status >/dev/null 2>&1 && break
  sleep 1
done

echo
echo "installed from $(cd "$repo" && git rev-parse --short HEAD):"
systemctl --user show cirroved.service -p FragmentPath -p ExecStart -p ActiveState | sed 's/^/  /'
echo "  tray: $(pgrep -x cirrove-tray >/dev/null && echo running || echo 'not running')"
echo "  files extension: $ext/cirrove.py"
echo "Your accounts, credentials and cache under ~/.local/state/cirrove were not touched."
