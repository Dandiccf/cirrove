#!/usr/bin/env python3
"""A release build names the release; a development build names the commit.

The release procedure has always said "the package version carries no suffix
when `pkgver` is a release version", and until 2026-09-18 no script did it. The
first real release build produced `cirrove-0.1.0.r532.gf6ada29-1` from the
commit tagged `v0.1.0`, which would have published a package whose version
disagrees with the tag it came from -- the same failure the four-source version
test exists to prevent, one step further along, where the only symptom is a
user reporting a version that does not exist.

So this drives the three build scripts' version derivation against a throwaway
git repository: tag the commit and the version is bare, leave it untagged and
it carries the count and the hash.
"""

import re
import pathlib
import subprocess
import tempfile
import unittest

REPO = pathlib.Path(__file__).resolve().parent.parent
SCRIPTS = {
    "arch": REPO / "scripts/build-arch-package.sh",
    "deb": REPO / "scripts/build-deb-package.sh",
    "rpm": REPO / "scripts/build-rpm-package.sh",
}


def derivation(script: pathlib.Path) -> str:
    """The `base`/`ver` block, lifted out of the script it belongs to.

    Running the whole script would build a package; what is under test is the
    six lines that decide the version, and they are taken from the file rather
    than copied here so this cannot pass against a script that has changed.
    """
    text = script.read_text()
    start = text.index("base=")
    # To the `fi` that closes the release branch, not to the first newline
    # after the dev version -- that one falls inside the `else` and leaves an
    # unterminated `if`, which is a bug in this extraction and not in the
    # script, and it looked exactly like a broken script.
    end = text.index("\nfi\n", start) + len("\nfi")
    block = text[start:end]
    assert "tag --points-at HEAD" in block, f"{script.name} has no release branch"
    return block


class VersionDependsOnWhetherTheCommitIsTagged(unittest.TestCase):
    def run_derivation(self, family: str, tag: str | None) -> str:
        with tempfile.TemporaryDirectory() as tmp:
            work = pathlib.Path(tmp)
            run = lambda *a: subprocess.run(
                a, cwd=work, check=True, capture_output=True, text=True
            )
            run("git", "init", "-q", "-b", "main")
            run("git", "config", "user.email", "t@example.invalid")
            run("git", "config", "user.name", "t")
            # Only the file this family reads its base version out of.
            for path, body in {
                "packaging/arch/PKGBUILD": "pkgver=0.1.0\n",
                "packaging/debian/changelog": "cirrove (0.1.0-1) unstable; urgency=medium\n",
                "packaging/rpm/cirrove.spec": "Version:        0.1.0\n",
            }.items():
                target = work / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(body)
            run("git", "add", "-A")
            run("git", "commit", "-qm", "one")
            if tag:
                run("git", "tag", tag)
            script = work / "derive.sh"
            script.write_text(
                "set -euo pipefail\nrepo=%s\n%s\necho \"$ver\"\n"
                % (work, derivation(SCRIPTS[family]))
            )
            out = subprocess.run(
                ["bash", str(script)], cwd=work, capture_output=True, text=True
            )
            self.assertEqual(out.returncode, 0, out.stderr)
            return out.stdout.strip()

    def test_a_tagged_commit_is_the_release_version_and_nothing_else(self):
        for family in SCRIPTS:
            with self.subTest(family=family):
                self.assertEqual(self.run_derivation(family, "v0.1.0"), "0.1.0")

    def test_an_untagged_commit_names_the_commit_it_came_from(self):
        for family in SCRIPTS:
            with self.subTest(family=family):
                version = self.run_derivation(family, None)
                self.assertRegex(version, r"^0\.1\.0\.r1\.g[0-9a-f]{7,}$")

    def test_a_tag_for_another_version_does_not_count_as_this_release(self):
        """`v0.2.0` on this commit must not make a 0.1.0 package call itself
        released: the tag has to be the one this version belongs to."""
        for family in SCRIPTS:
            with self.subTest(family=family):
                version = self.run_derivation(family, "v0.2.0")
                self.assertRegex(version, r"^0\.1\.0\.r1\.g[0-9a-f]{7,}$")


class TheDevBuildSkipsADigestItCannotMatch(unittest.TestCase):
    def test_the_arch_script_rewrites_the_checksum_as_well_as_the_version(self):
        """The script substitutes its own archive of HEAD for the release
        tarball, so verifying it against the tag tarball's digest is not a
        check but a guaranteed failure. It broke every dev build the moment
        step 7 filled the sum in."""
        text = SCRIPTS["arch"].read_text()
        self.assertIn("s/^sha256sums=.*/sha256sums=('SKIP')/", text)

    def test_the_recipe_itself_keeps_a_real_digest(self):
        """What an AUR user building from source verifies. `SKIP` there means
        they verify nothing."""
        recipe = (REPO / "packaging/arch/PKGBUILD").read_text()
        sums = re.search(r"^sha256sums=\((.*)\)$", recipe, re.M)
        self.assertIsNotNone(sums, "the recipe has no sha256sums line")
        self.assertRegex(
            sums.group(1).strip(),
            r"^'[0-9a-f]{64}'$",
            "the released recipe must carry the tag tarball's digest, not SKIP",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
