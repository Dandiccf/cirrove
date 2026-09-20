#!/usr/bin/env bash
# Build the RPM packages from the committed tree.
#
# Like scripts/build-arch-package.sh: HEAD is archived under the name the spec's
# Source0 expects, the spec's Version is rewritten to name the commit, and
# rpmbuild builds all binary packages in its own tree under the output
# directory. Needs rpm-build and a cargo on PATH -- see the note in
# packaging/rpm/cirrove.spec on why cargo is not a BuildRequires.
#
# Usage: scripts/build-rpm-package.sh [output-dir]   default: target/rpm/
set -euo pipefail

repo=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
out=${1:-$repo/target/rpm}

if ! git -C "$repo" diff --quiet HEAD -- . ':!target'; then
  echo "note: the working copy has uncommitted changes; the package is built from HEAD, not from them" >&2
fi
for tool in rpmbuild cargo; do
  command -v "$tool" >/dev/null || { echo "$tool is not installed" >&2; exit 1; }
done

# 0.1.0~dev sorts before 0.1.0 for rpm (tilde sorts before everything), and
# the revision count keeps successive dev builds upgradeable.
base=$(sed -n 's/^Version:[[:space:]]*//p' "$repo/packaging/rpm/cirrove.spec")
# On the commit a release is tagged at, the version is the release version and
# nothing else. The procedure has said so since it was written -- "the package
# version carries no suffix when `pkgver` is a release version" -- and no script
# implemented it, so the first release build produced 0.1.0.r532.gf6ada29 and
# would have shipped a package whose version disagrees with the tag it came
# from. That is the exact failure the four-source version test exists to
# prevent, one step further along: a user reporting a version that does not
# exist.
if [[ "$(git -C "$repo" tag --points-at HEAD)" == *"v$base"* ]]; then
  ver="$base"
else
  ver="$base.r$(git -C "$repo" rev-list --count HEAD).g$(git -C "$repo" rev-parse --short HEAD)"
fi

top="$out/rpmbuild"
rm -rf "$top"
mkdir -p "$top"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}
git -C "$repo" archive --format=tar.gz --prefix="cirrove-$ver/" -o "$top/SOURCES/cirrove-$ver.tar.gz" HEAD
sed "s/^Version:.*/Version:        $ver/" "$repo/packaging/rpm/cirrove.spec" > "$top/SPECS/cirrove.spec"

rpmbuild -bb --define "_topdir $top" "$top/SPECS/cirrove.spec"

# What each package must carry. A missing file is a broken install, not a
# warning.
expect() {
  local pkg=$1; shift
  local listing
  listing=$(rpm -qlp "$pkg" 2>/dev/null)
  for path in "$@"; do
    grep -qx "$path" <<<"$listing" || { echo "$pkg lacks $path" >&2; return 1; }
  done
}
id=io.github.Dandiccf.Cirrove
core=$(ls "$top"/RPMS/*/cirrove-"$ver"-*.rpm)
desktop=$(ls "$top"/RPMS/*/cirrove-desktop-"$ver"-*.rpm)
dolphin=$(ls "$top"/RPMS/*/cirrove-dolphin-"$ver"-*.rpm)
expect "$core" \
  /usr/bin/cirroved /usr/bin/cirrove \
  /usr/lib/systemd/user/cirroved.service
expect "$desktop" \
  /usr/bin/cirrove-desktop /usr/bin/cirrove-tray \
  "/usr/share/applications/$id.desktop" \
  "/etc/xdg/autostart/$id.Tray.desktop" \
  "/usr/share/icons/hicolor/scalable/apps/$id.svg" \
  "/usr/share/icons/hicolor/scalable/apps/$id-attention.svg" \
  "/usr/share/icons/hicolor/scalable/apps/$id-kept.svg" \
  "/usr/share/icons/hicolor/scalable/apps/$id-fetching.svg" \
  "/usr/share/metainfo/$id.metainfo.xml" \
  /usr/share/nautilus-python/extensions/cirrove.py
expect "$dolphin" \
  /usr/lib64/qt6/plugins/kf6/kfileitemaction/cirrovefileitemaction.so \
  /usr/lib64/qt6/plugins/kf6/overlayicon/cirroveoverlayicon.so
# The daemon package must not pull a desktop library in through the back door.
if rpm -qpR "$core" 2>/dev/null | grep -E '^(gtk4|libadwaita|kf6-|qt6-)|libgtk-4|libadwaita-1|libKF6|libQt6'; then
  echo "$core depends on a desktop library" >&2; exit 1
fi

mkdir -p "$out"
cp "$core" "$desktop" "$dolphin" "$out/"
echo
echo "built:"
echo "  $out/$(basename "$core")"
echo "  $out/$(basename "$desktop")"
echo "  $out/$(basename "$dolphin")"
echo "install with: sudo dnf install $out/$(basename "$core") $out/$(basename "$desktop") $out/$(basename "$dolphin")"
