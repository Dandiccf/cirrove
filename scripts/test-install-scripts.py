#!/usr/bin/env python3
"""install-developer.sh and switch-to-package.sh must be exact inverses.

Both installs put files in the same places, and a file in the home shadows the
packaged file of the same name: two Files extensions mean every badge and menu
item twice, and a home icon hides the packaged one, so the panel keeps drawing
a version that is no longer installed. The machine this was written on ended up
in exactly that state -- the report was "no it is doubled!", and then eight
stale icons had to be found by hand.

install-developer.sh refuses to run while the packages are installed, which
closes one direction. This closes the other: everything the developer install
writes into the home must be named by the script that moves back to packages,
so a new file added to one is a failure here rather than a duplicate on
someone's desktop.
"""

import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
INSTALL = ROOT / "scripts/install-developer.sh"
SWITCH = ROOT / "scripts/switch-to-package.sh"

# Where a developer install puts things, as the shell writes them. Each entry is
# the shell expression as it appears, and a human name for the failure message.
HOME_LOCATIONS = {
    "bin": "$HOME/.local/bin -- the binaries",
    "systemd/user": "~/.config/systemd/user/cirroved.service -- the unit",
    "autostart": "~/.config/autostart/...Tray.desktop -- the tray autostart entry",
    "applications": "~/.local/share/applications -- the desktop entry",
    "metainfo": "~/.local/share/metainfo -- the AppStream metainfo",
    "icons": "~/.local/share/icons/hicolor -- the icons",
    "nautilus-python": "~/.local/share/nautilus-python/extensions -- the Files extension",
    "qt6/plugins": "~/.local/lib/qt6/plugins -- the Dolphin plugins",
    "environment.d": "~/.config/environment.d -- the user-local Qt plugin path",
}

# What to look for in each script to decide it handles a location. Kept as
# substrings of the resolved text rather than exact lines, so reformatting the
# scripts does not fail this.
MARKERS = {
    "bin": [".local/bin"],
    "systemd/user": [".config/systemd/user"],
    "autostart": [".config/autostart"],
    "applications": [".local/share/applications"],
    "metainfo": [".local/share/metainfo"],
    "icons": [".local/share/icons"],
    "nautilus-python": ["nautilus-python"],
    "qt6/plugins": [".local/lib/qt6/plugins"],
    "environment.d": [".config/environment.d"],
}


def text(path: pathlib.Path) -> str:
    """The script with its variables expanded enough to match on."""
    body = path.read_text()
    # Resolve the handful of path variables the scripts define at the top, so a
    # marker matches whether the script spells the path out or names a variable.
    for name, value in re.findall(r'^(\w+)="([^"]+)"$', body, re.MULTILINE):
        body = body.replace(f"${{{name}}}", value).replace(f"${name}", value)
    # A second pass, because the definitions reference each other.
    for name, value in re.findall(r'^(\w+)="([^"]+)"$', body, re.MULTILINE):
        body = body.replace(f"${{{name}}}", value).replace(f"${name}", value)
    return body


class Inverses(unittest.TestCase):
    def setUp(self):
        # The install delegates the autostart entry to a helper, so the helper
        # is part of what the install writes.
        self.install = text(INSTALL) + text(ROOT / "scripts/install-tray-autostart.py")
        self.switch = text(SWITCH)

    def test_the_developer_install_writes_to_every_place_we_think_it_does(self):
        # Guards the test itself: if the install stops using a location, this
        # fails rather than quietly checking nothing.
        for key, description in HOME_LOCATIONS.items():
            self.assertTrue(
                any(m in self.install for m in MARKERS[key]),
                f"install-developer.sh no longer writes {description}; "
                f"remove it from this test or fix the script",
            )

    def test_switching_to_packages_removes_everything_the_developer_install_wrote(self):
        missing = [
            description
            for key, description in HOME_LOCATIONS.items()
            if not any(m in self.switch for m in MARKERS[key])
        ]
        self.assertEqual(
            missing,
            [],
            "switch-to-package.sh leaves these behind, where they shadow the "
            "packaged files: " + "; ".join(missing),
        )

    def test_the_developer_install_refuses_to_sit_beside_the_packages(self):
        self.assertIn("pacman -Qq cirrove", self.install)
        self.assertIn("exit 1", self.install)

    def test_neither_script_touches_the_state_directory(self):
        # Accounts, credentials, the index, the cache and bytes not yet sent.
        for name, body in (("install-developer.sh", self.install), ("switch-to-package.sh", self.switch)):
            for line in body.splitlines():
                stripped = line.strip()
                if stripped.startswith("#"):
                    continue
                if "state/cirrove" in stripped:
                    self.assertNotIn(
                        "rm ", stripped, f"{name} removes something under the state directory: {stripped!r}"
                    )


@unittest.skipUnless(shutil.which("gtk-update-icon-cache"), "GTK icon cache tool is not installed")
class IconCache(unittest.TestCase):
    def setUp(self):
        self.fixture = pathlib.Path(tempfile.mkdtemp(prefix="cirrove-icon-cache-"))
        self.icons = self.fixture / ".local/share/icons/hicolor"
        self.apps = self.icons / "scalable/apps"
        self.apps.mkdir(parents=True)
        self.cirrove = self.apps / "io.github.Dandiccf.Cirrove-kept.svg"
        self.other = self.apps / "unrelated-fixture.svg"
        svg = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16"/></svg>'
        self.cirrove.write_text(svg)
        self.other.write_text(svg)
        self.cache = self.icons / "icon-theme.cache"

    def tearDown(self):
        # Only these fixture files can be removed; never walk a mount or home.
        for path in (self.cirrove, self.other, self.cache):
            path.unlink(missing_ok=True)
        for path in (self.apps, self.apps.parent, self.icons, self.icons.parent,
                     self.icons.parent.parent, self.fixture / ".local", self.fixture):
            path.rmdir()

    def seed_cache(self):
        subprocess.run(["gtk-update-icon-cache", "-f", "-t", str(self.icons)],
                       check=True, capture_output=True)
        self.assertIn(self.cirrove.stem.encode(), self.cache.read_bytes())

    def switch_icons(self):
        # Execute the actual cleanup block, isolated from binaries, units,
        # extensions, mounts and the user's HOME. No fake cache implementation.
        body = SWITCH.read_text().split('icons="$HOME/.local/share/icons/hicolor"', 1)[1]
        body = 'icons="$CIRROVE_TEST_HOME/.local/share/icons/hicolor"' + body.split("# Translations", 1)[0]
        env = dict(os.environ, CIRROVE_TEST_HOME=str(self.fixture))
        subprocess.run(["bash", "-eu", "-c", 'id=io.github.Dandiccf.Cirrove\n' + body],
                       env=env, check=True, capture_output=True)

    def test_developer_install_creates_cache_without_a_theme_index(self):
        command = next(line for line in INSTALL.read_text().splitlines()
                       if line.startswith("gtk-update-icon-cache "))
        env = dict(os.environ, CIRROVE_TEST_ICONS=str(self.icons))
        subprocess.run(["bash", "-eu", "-c", 'icons="$CIRROVE_TEST_ICONS"\n' + command],
                       env=env, check=True, capture_output=True)
        self.assertTrue(self.cache.exists(), "user hicolor has no index.theme; the installer must still build its cache")
        self.assertIn(self.cirrove.stem.encode(), self.cache.read_bytes())

    def test_switch_removes_cached_developer_icons_and_preserves_other_icons(self):
        self.seed_cache()
        self.switch_icons()
        self.assertFalse(self.cirrove.exists())
        self.assertNotIn(self.cirrove.stem.encode(), self.cache.read_bytes())
        self.assertTrue(self.other.exists())
        self.assertIn(self.other.stem.encode(), self.cache.read_bytes())

    def test_switch_repairs_a_stale_cache_even_when_icon_files_are_already_gone(self):
        self.seed_cache()
        self.cirrove.unlink()
        self.switch_icons()
        self.assertNotIn(self.cirrove.stem.encode(), self.cache.read_bytes())
        self.assertIn(self.other.stem.encode(), self.cache.read_bytes())


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v"])
