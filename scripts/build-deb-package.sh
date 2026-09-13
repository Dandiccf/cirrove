#!/usr/bin/env bash
# Build the Debian packages from the committed tree.
#
# Like scripts/build-arch-package.sh: HEAD is archived, packaging/debian
# becomes the tree's debian/ directory with the changelog version rewritten to
# name the commit, and dpkg-buildpackage builds both binary packages. Needs
# dpkg-dev and debhelper, and a rustup cargo on PATH -- see the note in
# packaging/debian/control on why cargo is not a build dependency, which is
# also why this passes -d.
#
# Usage: scripts/build-deb-package.sh [output-dir]   default: target/deb/
set -euo pipefail

repo=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
out=${1:-$repo/target/deb}

if ! git -C "$repo" diff --quiet HEAD -- . ':!target'; then
  echo "note: the working copy has uncommitted changes; the package is built from HEAD, not from them" >&2
fi
for tool in dpkg-buildpackage dh cargo; do
  command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }
done

# 0.1.0~dev sorts before 0.1.0 for dpkg (tilde sorts before everything), and
# the revision count keeps successive dev builds upgradeable.
base=$(sed -n '1s/^cirrove (\(.*\)-[0-9]*) .*/\1/p' "$repo/packaging/debian/changelog")
ver="$base.r$(git -C "$repo" rev-list --count HEAD).g$(git -C "$repo" rev-parse --short HEAD)"

work="$out/cirrove-$ver"
rm -rf "$work"
mkdir -p "$work"
git -C "$repo" archive --format=tar HEAD | tar -x -C "$work"
cp -r "$repo/packaging/debian" "$work/debian"
sed -i "1s/.*/cirrove ($ver-1) unstable; urgency=medium/" "$work/debian/changelog"

(
  cd "$work"
  # -us -uc: unsigned; -b: binary packages only; -d: build-deps not checked,
  # because the toolchain is rustup's, not a package.
  dpkg-buildpackage -us -uc -b -d
)

# What each package must carry. A missing file is a broken install, not a
# warning.
expect() {
  local pkg=$1; shift
  local listing
  listing=$(dpkg-deb -c "$pkg" | awk '{print $6}' | sed 's|^\./||')
  for path in "$@"; do
    grep -qx "$path" <<<"$listing" || { echo "$pkg lacks $path" >&2; return 1; }
  done
}
id=io.github.Dandiccf.Cirrove
core=$(ls "$out"/cirrove_"$ver"-1_*.deb)
desktop=$(ls "$out"/cirrove-desktop_"$ver"-1_*.deb)
expect "$core" \
  usr/bin/cirroved usr/bin/cirrove \
  usr/lib/systemd/user/cirroved.service
expect "$desktop" \
  usr/bin/cirrove-desktop usr/bin/cirrove-tray \
  "usr/share/applications/$id.desktop" \
  "etc/xdg/autostart/$id.Tray.desktop" \
  "usr/share/icons/hicolor/scalable/apps/$id.svg" \
  "usr/share/icons/hicolor/scalable/apps/$id-attention.svg" \
  "usr/share/metainfo/$id.metainfo.xml" \
  usr/share/nautilus-python/extensions/cirrove.py
# The daemon package must not pull a desktop library in through the back door.
if dpkg-deb -f "$core" Depends | grep -E 'libgtk-4|libadwaita'; then
  echo "$core depends on a desktop library" >&2; exit 1
fi

echo
echo "built:"
echo "  $core"
echo "  $desktop"
echo "install with: sudo apt install $core $desktop"
