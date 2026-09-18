#!/usr/bin/env bash
# Does the Arch recipe's checksum match the tarball the tag actually serves?
#
# This is the one thing in a release that no other check can see, because it
# compares a file in this repository against a file on a server. It went wrong
# on the first release and was caught by eye: the tag was moved after the digest
# was filled in, so `sha256sums` named a tarball that no longer existed and an
# AUR user building from source would have hit a mismatch. `.SRCINFO` carried
# the same stale value, which is what the AUR would have been handed.
#
# Not part of scripts/check.sh: it needs the network and a published tag, and a
# check that cannot run offline is a check people learn to skip. The release
# procedure calls it, once, at the point where it can be true.
#
# Usage: scripts/verify-release-recipe.sh [tag]     (default: the pkgver's tag)
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
recipe="$repo/packaging/arch/PKGBUILD"
srcinfo="$repo/packaging/arch/.SRCINFO"

pkgver=$(sed -n 's/^pkgver=//p' "$recipe")
tag=${1:-v$pkgver}
declared=$(sed -n "s/^sha256sums=('\\(.*\\)')$/\\1/p" "$recipe")

if [[ "$declared" == SKIP ]]; then
  echo "sha256sums is SKIP, so there is nothing to verify."
  echo "That is correct between releases and wrong in one: pkgver=$pkgver"
  [[ "$pkgver" == *dev* ]] && exit 0
  echo "  -- pkgver names a release, so the recipe must carry a digest" >&2
  exit 1
fi

url="https://github.com/Dandiccf/cirrove/archive/refs/tags/$tag.tar.gz"
tmp=$(mktemp) && trap 'rm -f "$tmp"' EXIT
echo "fetching $url"
curl -fsSL -o "$tmp" "$url"
actual=$(sha256sum "$tmp" | cut -d' ' -f1)

echo "  recipe   $declared"
echo "  tarball  $actual"
if [[ "$declared" != "$actual" ]]; then
  echo "MISMATCH: an AUR build from source would refuse this tarball." >&2
  echo "  updpkgsums in packaging/arch, then makepkg --printsrcinfo > .SRCINFO" >&2
  exit 1
fi

# The index the AUR is actually handed has to agree with the recipe it indexes.
# It is generated, so it drifts silently the moment the recipe changes without
# it -- which is exactly how the stale digest reached it.
indexed=$(sed -n 's/^\tsha256sums = //p' "$srcinfo" | head -1)
echo "  .SRCINFO $indexed"
if [[ "$indexed" != "$actual" ]]; then
  echo "MISMATCH: .SRCINFO disagrees with the recipe it indexes." >&2
  echo "  cd packaging/arch && makepkg --printsrcinfo > .SRCINFO" >&2
  exit 1
fi
indexed_ver=$(sed -n 's/^\tpkgver = //p' "$srcinfo" | head -1)
if [[ "$indexed_ver" != "$pkgver" ]]; then
  echo "MISMATCH: .SRCINFO says pkgver $indexed_ver, the recipe says $pkgver." >&2
  exit 1
fi

echo "the recipe, its index and the tag's tarball all agree."
