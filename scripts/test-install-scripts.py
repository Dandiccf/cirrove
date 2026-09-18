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

import pathlib
import re
import sys
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


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v"])
