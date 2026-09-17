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

## The signature, and the one decision it needs

Step 13 asks for a signature over `SHA256SUMS`, and what signs it is the one
thing in this document that is not ours to decide: a release key is a credential
someone has to hold for the life of the project. The options, with what each
costs the person holding it and what a stranger does to check a download.

**A. GitHub artifact attestations** (`actions/attest-build-provenance`).
No key exists. CI signs each package as it builds it, through Sigstore and
GitHub's OIDC identity, and a stranger runs
`gh attestation verify cirrove-0.1.0-1-x86_64.pkg.tar.zst --repo Dandiccf/cirrove`.
It proves more than a detached signature does -- not only *someone with the key
made this* but *this workflow, on this commit, made this* -- and there is nothing
to keep safe, lose, or rotate. Costs: CI needs `id-token: write` and
`attestations: write`, which it does not have today; it can only cover artifacts
CI built, so the Arch package would come from CI's `arch-packages` rather than
from a local clean chroot as step 8 has it; and a verifier needs `gh` or cosign
rather than the `gpg` already on every machine.

**B. An OpenPGP release key.** What a release page has looked like for thirty
years, verifiable offline with `gpg --verify` and nothing installed. Costs: the
key has to be generated, its private half kept for the project's life, and its
public half put somewhere a stranger has reason to trust -- and losing it or
leaking it is a real event with a real recovery. There is no key on this machine
today (`gpg --list-secret-keys` is empty), so choosing this means making one.

**C. Both.** Attestations for provenance, OpenPGP over `SHA256SUMS` for the
people who expect it. Twice the ceremony, and the second one still has to be
kept safe.

**D. Neither, said plainly.** `SHA256SUMS` published unsigned, with the release
notes saying it is unsigned and what that does and does not protect against.
Honest, and weaker than the other three.

**The recommendation is A for 0.1.0.** One developer, no update channels yet --
signed APT and COPR are their own milestone-6 row and explicitly do not gate
this release -- and the failure mode of B is a lost or leaked key held by one
person, against a mechanism that has no key at all. B becomes worth its cost
when there is a channel whose metadata must be signed, which is the release
after this one. Unverified: the attestation flow has not been run here, and
proving it is one change to `ci.yml` on a branch, which is a job for whoever
picks it rather than a reason to pick it.

## Publishing

13. A GitHub release on the tag with the packages, `SHA256SUMS`, its
    signature per the decision above, and the changelog entry as the body.
14. The AUR recipe (`.SRCINFO` from `makepkg --printsrcinfo`) once there is a
    release to point at; APT and COPR channels are their own milestone-6 rows
    and do not gate this.

## After

15. Bump the workspace version to the next `-dev`, and the three packaging
    sources with it.
16. Anything that went differently from this document goes into this
    document.
