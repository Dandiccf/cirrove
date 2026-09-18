# Release procedure

How a tagged release comes out of this tree, written before the first one so
that the first one is a rehearsal of this and not an improvisation. Every
step is something a person types or checks; nothing here is automatic yet,
and the places where it could be are marked.

## Before tagging

1. The release blockers are closed, **except the two that are the release
   itself**: `scripts/acceptance-ledger.py --blockers` lists nothing under
   "What stands between this and a 1.0 release" other than the packages-from-a-
   release row and the tagged-release row. Each closed row is closed with
   evidence in the [ledger](acceptance-ledger.json), not with intent, and each
   carries the test that would fail if it stopped holding. The criterion for
   what blocks is in
   [Distribution](distribution.md#what-blocks-the-first-release-and-what-does-not).

   The exception is not a loophole and it took until 2026-09-18 to notice,
   because the list had never been short enough for the circle to show. Read
   literally, this step forbids a first release for ever: those two rows cannot
   close before a tag exists, a tag cannot be made before this step passes, and
   no amount of work on anything else moves either. The criterion the ledger
   actually applies is *"would a person reasonably say you shipped 1.0 with
   this broken"*, and it cannot be asked of "you have not shipped yet". So the
   two rows are what this step is a precondition **for**, not part of it.
   Everything else on the blocking list still has to be closed with evidence
   first, which is the whole of the list's value.
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

   **A recipe cannot carry the digest of its own commit's tarball**, and it
   took a release to notice: writing the digest in changes the commit, which
   changes the tarball, which changes the digest. So the tagged tree's
   `sha256sums` necessarily belongs to some earlier tarball or says `SKIP`,
   and the one an AUR user builds against is the one on `main` after step 7.
   `scripts/test-package-versions.py` holds the pairing that *can* be
   asserted -- a `dev` version must say `SKIP`, a release version must carry a
   digest -- which catches shipping a release recipe that verifies nothing,
   and leaving a release digest behind in a development cycle.

## Tagging

6. `git tag -a v0.1.0 -m "Cirrove 0.1.0"` on that commit, and push the tag.
   The tag is what the PKGBUILD's source line downloads; nothing before this
   point can build the release package.
7. Fill in `sha256sums` from the tag's tarball and commit that on the branch
   (the tag itself does not move).

## Building the packages

8. Arch: the published package is the tagged commit's `arch-packages`
   artefact, because an attestation covers what CI built and nothing else.
   `scripts/build-arch-package.sh` still builds HEAD locally -- the tagged
   commit plus the checksum commit, no version suffix when `pkgver` is a
   release version -- in a clean chroot (`extra-x86_64-build` from `devtools`)
   rather than on a developer machine, so the build depends on what the
   PKGBUILD says and nothing else. That local build is step 12's
   reproducibility comparison; it is not what ships.
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
12b. `scripts/verify-release-recipe.sh` — the recipe's `sha256sums`, its
    `.SRCINFO`, and the tarball the tag actually serves must be the same
    digest. Nothing else can see this, because it compares a file here against
    a file on a server, and it went wrong on the first release: the tag was
    moved after step 7 filled the digest in, so the recipe named a tarball that
    no longer existed and `.SRCINFO` carried the stale value onward. **Any step
    that moves a tag sends you back to step 7 and then to here.**

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

**A is chosen, by the account owner on 2026-09-17, and it is built.** One
developer, no update channels yet -- signed APT and COPR are their own
milestone-6 row and explicitly do not gate this release -- and the failure mode
of B is a lost or leaked key held by one person, against a mechanism that has no
key at all. B becomes worth its cost when there is a channel whose metadata must
be signed, which is the release after this one.

All three package jobs in `ci.yml` now attest what they build, with
`actions/attest-build-provenance` pinned to v4.2.2, each job carrying
`id-token: write` and `attestations: write` of its own -- naming any permission
at a job replaces the workflow's whole set, so `contents: read` is repeated
beside them. The attestation is made **before** the upload, so what a release
publishes is the file this workflow signed rather than one that passed through
anything afterwards. It runs on every push and pull request, not only on a
release: a mechanism exercised once a year is one that is broken when you need
it.

**What this changes elsewhere in this document.** Step 8 built the Arch package
locally in a clean chroot; the published one must now come from the tagged
commit's `arch-packages` artefact, because an attestation covers what CI built
and nothing else. A local build stays worth doing -- it is step 12's
reproducibility comparison -- but it is not what ships.

**What a stranger runs**, with nothing but `gh` installed and no key to fetch:

```sh
gh attestation verify cirrove-0.1.0-1-x86_64.pkg.tar.zst --repo Dandiccf/cirrove
```

It answers with the workflow, the commit and the repository that produced the
file. Anyone preferring not to install `gh` can verify the same Sigstore bundle
with `cosign verify-blob-attestation`.

**Proven on 2026-09-17, not assumed.** CI run 35185096527 built the three Arch
packages and attested them; the artefact was downloaded again afterwards and
verified from a directory that had nothing to do with the build:

```
Repository : https://github.com/Dandiccf/cirrove
Commit     : a9e47dbf282b7bd47b73eb1ead5d2ce07a346b09
Workflow   : .github/workflows/ci.yml@refs/pull/48/merge
Subject    : cirrove-0.1.0dev.r522.ga9e47db-1-x86_64.pkg.tar.zst
```

`gh attestation verify` exits 0 and prints nothing on success, which is worth
knowing before someone reads an empty output as a failure; `--format json` is
where the four lines above come from.

## Publishing

13. A GitHub release on the tag with the packages, `SHA256SUMS`, and the
    changelog entry as the body. `SHA256SUMS` itself is unsigned; each package
    carries its own attestation, made by the workflow that built it, and the
    release body says so with the `gh attestation verify` line above. Verify
    every published package against its attestation before publishing, from a
    checkout that is not the one that built it.
14. The AUR recipe (`.SRCINFO` from `makepkg --printsrcinfo`) once there is a
    release to point at; APT and COPR channels are their own milestone-6 rows
    and do not gate this. It is generated into `packaging/arch/.SRCINFO` and
    committed, so the recipe and its index move together and a reader can see
    what the AUR would be given. **Pushing it to the AUR needs an AUR account
    and its ssh key**, which is the maintainer's and is not in this
    repository — so that push is the one part of a release this procedure
    cannot carry out on its own.

## After

15. Bump the workspace version to the next `-dev`, and the three packaging
    sources with it. Four more things belong in the same commit, and each was
    missing from this step until the first release walked through it:
    - **Reset the PKGBUILD's `sha256sums` to `SKIP`.** A development version
      names a tag that does not exist, so the release's digest points at a
      tarball with nothing to do with it. The version test fails otherwise.
    - **Regenerate `.SRCINFO`**, or it keeps indexing the version that shipped.
    - **Regenerate `Cargo.lock`** (`cargo check --offline --workspace`). The
      workspace crates carry their version in it, so `--locked` fails on the
      next build otherwise — which is how this was found.
    - **Open a new `## Unreleased` heading** in the changelog above the one
      just released.
16. Anything that went differently from this document goes into this
    document.
