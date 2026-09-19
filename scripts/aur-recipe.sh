#!/usr/bin/env bash
# Build the recipe the AUR should be given, for a tag, from scratch.
#
# Not a snapshot, because a snapshot is wrong the moment the next release
# happens and right-looking in between. The AUR-ready files do not and should
# not live on `main`: step 15 puts `pkgver` back to a development version and
# `sha256sums` back to `SKIP`, which is correct there and useless here. So this
# regenerates them from whichever tag is asked for.
#
# It recomputes the digest rather than trusting the one in the tagged tree.
# That tree's `sha256sums` necessarily belongs to an earlier tarball -- a
# recipe cannot contain the digest of its own commit's tarball -- and on 0.1.0
# it was stale for a second reason: the tag was moved after the digest was
# filled in.
#
# Usage: scripts/aur-recipe.sh v0.1.0 [output-dir]
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tag=${1:?a tag, e.g. v0.1.0}
out=${2:-$repo/target/aur/$tag}
version=${tag#v}

git -C "$repo" rev-parse -q --verify "refs/tags/$tag" >/dev/null || {
  echo "no such tag: $tag" >&2; exit 1; }

mkdir -p "$out"
git -C "$repo" show "$tag:packaging/arch/PKGBUILD" > "$out/PKGBUILD"

url="https://github.com/Dandiccf/cirrove/archive/refs/tags/$tag.tar.gz"
tarball=$(mktemp) && trap 'rm -f "$tarball"' EXIT
echo "fetching $url"
curl -fsSL -o "$tarball" "$url"
digest=$(sha256sum "$tarball" | cut -d' ' -f1)

sed -i -e "s/^pkgver=.*/pkgver=$version/" \
       -e "s/^sha256sums=.*/sha256sums=('$digest')/" "$out/PKGBUILD"
( cd "$out" && makepkg --printsrcinfo > .SRCINFO )

# The same three-way agreement scripts/verify-release-recipe.sh asks for, on
# the files just written rather than on the ones in the tree.
indexed=$(sed -n 's/^\tsha256sums = //p' "$out/.SRCINFO" | head -1)
indexed_ver=$(sed -n 's/^\tpkgver = //p' "$out/.SRCINFO" | head -1)
[[ "$indexed" == "$digest" ]] || { echo ".SRCINFO digest disagrees" >&2; exit 1; }
[[ "$indexed_ver" == "$version" ]] || { echo ".SRCINFO version disagrees" >&2; exit 1; }

echo
echo "$out"
echo "  pkgver     $version"
echo "  sha256sums $digest"
echo "  .SRCINFO   agrees with both"
echo
echo "to submit, once the AUR account exists and its key is registered:"
echo "  git clone ssh://aur@aur.archlinux.org/cirrove.git /tmp/aur-cirrove"
echo "  cp $out/PKGBUILD $out/.SRCINFO /tmp/aur-cirrove/"
echo "  cd /tmp/aur-cirrove && git add -A && git commit -m 'Cirrove $version' && git push"
