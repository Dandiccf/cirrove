# Release procedure

How a tagged release comes out of this tree, written before the first one so
that the first one is a rehearsal of this and not an improvisation. Every
step is something a person types or checks; nothing here is automatic yet,
and the places where it could be are marked.

## Before tagging

1. The release blockers are closed: `scripts/acceptance-ledger.py --blockers`
   lists nothing under "What stands between this and a 1.0 release". Each of
   those rows is closed with evidence in the [ledger](acceptance-ledger.json),
   not with intent, and each carries the test that would fail if it stopped
   holding. The criterion for what blocks is in
   [Distribution](distribution.md#what-blocks-the-first-release-and-what-does-not).
2. CI is green on the commit to be tagged: every job, including the three
   package jobs and the dependency audit.
3. `Cargo.toml`'s workspace version is the release version without `-dev`
   (`0.1.0`), and the three packaging sources agree: `pkgver=0.1.0` in
   `packaging/arch/PKGBUILD`, `cirrove (0.1.0-1)` at the top of
   `packaging/debian/changelog`, `Version: 0.1.0` in
   `packaging/rpm/cirrove.spec`. The dev-build scripts append a commit
   suffix to these; a release build does not.
4. `docs/changelog.md` has the release's entry: what a user notices, in the
   user's words, with the milestone rows it closes.
5. The PKGBUILD's `sha256sums` is filled in from the tagged tarball
   (`updpkgsums` after the tag exists, so this is step 7 as well).

## Tagging

6. `git tag -a v0.1.0 -m "Cirrove 0.1.0"` on that commit, and push the tag.
   The tag is what the PKGBUILD's source line downloads; nothing before this
   point can build the release package.
7. Fill in `sha256sums` from the tag's tarball and commit that on the branch
   (the tag itself does not move).

## Building the packages

8. Arch: `scripts/build-arch-package.sh` builds HEAD, which for a release is
   the tagged commit plus the checksum commit; the package version carries no
   suffix when `pkgver` is a release version. Build in a clean chroot
   (`extra-x86_64-build` from `devtools`) rather than on a developer machine,
   so the build depends on what the PKGBUILD says and nothing else.
9. Ubuntu 24.04 and Fedora: download the `deb-packages` and `rpm-packages`
   artefacts of the tag's CI run; they were built on a clean runner and a
   clean container from the tagged commit.
10. `sha256sum` every package into `SHA256SUMS`, and sign it
    (`gpg --detach-sign`) with the release key. Which key, and where its
    public half is published, is a decision this document does not yet
    record.

## Verifying before publishing

11. Install each package on a clean machine of its family -- the Arch
    machine, the Ubuntu VM, the Fedora VM -- through sign-in and a reboot, as
    the [distribution gates](distribution.md#milestone-6-acceptance-gates)
    require. Record it in `docs/validation.md`.
12. Build the Arch package a second time from the same tag and compare the
    `.MTREE` and file checksums; record whether it is reproducible and, if
    not, what differed. A lockfile is not reproducibility.

## Publishing

13. A GitHub release on the tag with the packages, `SHA256SUMS`, its
    signature, and the changelog entry as the body.
14. The AUR recipe (`.SRCINFO` from `makepkg --printsrcinfo`) once there is a
    release to point at; APT and COPR channels are their own milestone-6 rows
    and do not gate this.

## After

15. Bump the workspace version to the next `-dev`, and the three packaging
    sources with it.
16. Anything that went differently from this document goes into this
    document.
