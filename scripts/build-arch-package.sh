#!/usr/bin/env bash
# Build the Arch packages from the committed tree.
#
# packaging/arch/PKGBUILD is written for a tagged release and downloads a
# tarball by version. There is no tag yet, and a package built from a working
# copy with uncommitted edits would be a package nobody can rebuild. So this
# builds HEAD: it archives the commit under the file name the PKGBUILD's source
# line expects, rewrites pkgver to name the commit, and points makepkg at the
# archive so nothing is downloaded. What comes out is what `pacman -U` installs.
#
# Usage: scripts/build-arch-package.sh [output-dir] [-- makepkg args...]
#   default output: target/arch/
#
# The packages are then checked for the files each must carry, because a
# PKGBUILD that installs too little fails on the user's machine, not here.
set -euo pipefail

repo=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
out="$repo/target/arch"
if [[ $# -gt 0 && $1 != -- ]]; then out=$1; shift; fi
[[ ${1:-} == -- ]] && shift
makepkg_args=("$@")

if ! git -C "$repo" diff --quiet HEAD -- . ':!target'; then
  echo "note: the working copy has uncommitted changes; the package is built from HEAD, not from them" >&2
fi

# 0.1.0dev sorts before 0.1.0 for pacman (a letter suffix on a segment is
# older than the plain segment), and the revision count keeps successive dev
# builds upgradeable.
base=$(sed -n 's/^pkgver=//p' "$repo/packaging/arch/PKGBUILD")
ver="$base.r$(git -C "$repo" rev-list --count HEAD).g$(git -C "$repo" rev-parse --short HEAD)"

mkdir -p "$out/src"
git -C "$repo" archive --format=tar.gz --prefix="cirrove-$ver/" -o "$out/src/cirrove-$ver.tar.gz" HEAD
sed "s/^pkgver=.*/pkgver=$ver/" "$repo/packaging/arch/PKGBUILD" > "$out/PKGBUILD"

# makepkg checks makedepends against pacman. A developer whose cargo comes from
# rustup has one pacman cannot see; build with it and say so. A clean build root
# has the distribution's rust and gets the real check.
if ! pacman -T cargo >/dev/null 2>&1 && command -v cargo >/dev/null; then
  echo "note: cargo is not a pacman package here ($(command -v cargo)); building with --nodeps" >&2
  makepkg_args+=(--nodeps)
fi

(
  cd "$out"
  SRCDEST="$out/src" PKGDEST="$out" BUILDDIR="$out/build" \
    makepkg --force --cleanbuild "${makepkg_args[@]}"
)

# What each package must carry. A missing file is a broken install, not a
# warning.
expect() {
  local pkg=$1; shift
  local listing
  listing=$(tar -tf "$pkg")
  for path in "$@"; do
    grep -qx "$path" <<<"$listing" || { echo "$pkg lacks $path" >&2; return 1; }
  done
}
id=io.github.Dandiccf.Cirrove
core=$(ls "$out"/cirrove-"$ver"-*.pkg.tar.* | grep -v -- '-debug-')
desktop=$(ls "$out"/cirrove-desktop-"$ver"-*.pkg.tar.* | grep -v -- '-debug-')
expect "$core" \
  usr/bin/cirroved usr/bin/cirrove \
  usr/lib/systemd/user/cirroved.service \
  usr/share/licenses/cirrove/LICENSE
expect "$desktop" \
  usr/bin/cirrove-desktop usr/bin/cirrove-tray \
  "usr/share/applications/$id.desktop" \
  "etc/xdg/autostart/$id.Tray.desktop" \
  "usr/share/icons/hicolor/scalable/apps/$id.svg" \
  "usr/share/icons/hicolor/scalable/apps/$id-ready.svg" \
  "usr/share/icons/hicolor/scalable/apps/$id-working.svg" \
  "usr/share/icons/hicolor/scalable/apps/$id-attention.svg" \
  "usr/share/icons/hicolor/symbolic/apps/$id-symbolic.svg" \
  "usr/share/metainfo/$id.metainfo.xml" \
  usr/share/nautilus-python/extensions/cirrove.py \
  usr/share/licenses/cirrove-desktop/LICENSE
# The daemon package must not pull a desktop library in through the back door.
if tar -xOf "$core" .PKGINFO | grep -E '^depend = (gtk4|libadwaita)'; then
  echo "$core depends on a desktop library" >&2; exit 1
fi

echo
echo "built:"
echo "  $core"
echo "  $desktop"
echo "install with: sudo pacman -U $core $desktop"
