#!/usr/bin/env python3
"""Which Exec= the autostart entry gets, and why it is not always the same one.

The shipped entry carries a bare `Exec=cirrove-tray`. That is correct for a
package in /usr/bin and silently wrong for a user-local install: the
xdg-autostart generator resolves Exec= against its own sparse PATH at session
start, does not find ~/.local/bin, and writes no unit at all. No error reaches
the user; the tray never appears. Found on a real login.

So the rule has two halves and both matter. Writing an absolute path always
would bake one machine's layout into a packaged file; writing a bare name always
is the bug. These assert that the installer picks by where the binary actually
is.
"""

import importlib.util
import os
import pathlib
import stat
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "installer", pathlib.Path(__file__).with_name("install-tray-autostart.py")
)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class ExecLine(unittest.TestCase):
    def _binary(self, directory: pathlib.Path) -> pathlib.Path:
        directory.mkdir(parents=True, exist_ok=True)
        binary = directory / installer.BINARY
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        return binary

    def test_a_packaged_binary_keeps_the_bare_name(self):
        """An absolute path would bake one prefix into a file every distribution
        installs somewhere else."""
        with tempfile.TemporaryDirectory() as tmp:
            system = pathlib.Path(tmp) / "usr/bin"
            self._binary(system)
            installer.GENERATOR_PATH = str(system)
            os.environ["PATH"] = str(system)
            command, why = installer.exec_line()
            self.assertEqual(command, installer.BINARY)
            self.assertIn("generator", why)

    def test_a_user_local_binary_gets_an_absolute_path(self):
        """The case that produced no tray: on PATH for the user, invisible to the
        generator."""
        with tempfile.TemporaryDirectory() as tmp:
            local = pathlib.Path(tmp) / "home/.local/bin"
            self._binary(local)
            installer.GENERATOR_PATH = str(pathlib.Path(tmp) / "usr/bin")
            os.environ["PATH"] = str(local)
            command, why = installer.exec_line()
            self.assertEqual(command, str(local / installer.BINARY))
            self.assertIn("skipped", why)

    def test_a_path_with_a_detour_is_written_straight(self):
        """PATH entries are whatever a shell profile put there. This machine's
        carried ~/.local/share/../bin, which resolves and would have gone into
        the file as a detour through a directory that has nothing to do with it.
        """
        with tempfile.TemporaryDirectory() as tmp:
            real = pathlib.Path(tmp) / "home/.local/bin"
            self._binary(real)
            (pathlib.Path(tmp) / "home/.local/share").mkdir(parents=True, exist_ok=True)
            detour = pathlib.Path(tmp) / "home/.local/share/../bin"
            installer.GENERATOR_PATH = str(pathlib.Path(tmp) / "usr/bin")
            os.environ["PATH"] = str(detour)
            command, _ = installer.exec_line()
            self.assertEqual(command, str(real / installer.BINARY))
            self.assertNotIn("..", command)

    def test_no_binary_anywhere_refuses_rather_than_installing_a_dead_entry(self):
        """An autostart entry pointing at nothing is worse than none: it fails
        every login, silently, and looks installed."""
        with tempfile.TemporaryDirectory() as tmp:
            empty = pathlib.Path(tmp) / "empty"
            empty.mkdir()
            installer.GENERATOR_PATH = str(empty)
            os.environ["PATH"] = str(empty)
            with self.assertRaises(SystemExit):
                installer.exec_line()


if __name__ == "__main__":
    unittest.main()
