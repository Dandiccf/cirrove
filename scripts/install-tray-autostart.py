#!/usr/bin/env python3
"""Install the tray's autostart entry so that it actually starts at login.

The shipped entry carries `Exec=cirrove-tray`, a bare name. That is right for a
distribution package that puts the binary in /usr/bin, and silently wrong for a
user-local install: `systemd-xdg-autostart-generator` resolves Exec= against its
own PATH at session start, does not find ~/.local/bin there, logs

    Exec binary 'cirrove-tray' does not exist: No such file or directory
    ... not generating unit, executable specified in Exec= does not exist

and writes no unit. Nothing reaches the user. The tray simply never appears,
which is how this was found: a real login, a mount that came back, and no icon.

So this resolves the binary the way the generator will have to, and writes an
absolute Exec= when -- and only when -- the binary is somewhere the generator
would not look. A packaged install gets the bare name it should have.

    scripts/install-tray-autostart.py            # install
    scripts/install-tray-autostart.py --check    # report, change nothing
    scripts/install-tray-autostart.py --remove
"""

import argparse
import os
import pathlib
import shutil
import subprocess
import sys

SOURCE = pathlib.Path(__file__).resolve().parent.parent / (
    "packaging/desktop/io.github.Dandiccf.Cirrove.Tray.desktop"
)
TARGET = pathlib.Path.home() / ".config/autostart/io.github.Dandiccf.Cirrove.Tray.desktop"
BINARY = "cirrove-tray"

# What the generator has when it runs, which is early and sparse. It is not the
# PATH of the shell you are reading this in, and that difference is the bug.
GENERATOR_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"


def found_by_generator() -> str | None:
    return shutil.which(BINARY, path=GENERATOR_PATH)


def found_anywhere() -> str | None:
    found = shutil.which(BINARY)
    # Normalised, because PATH entries are whatever a shell profile put there.
    # This machine's carried ~/.local/share/../bin, which resolves fine and would
    # have gone into the file as a detour through a directory that has nothing to
    # do with it -- and would break outright if that directory ever went away.
    return str(pathlib.Path(found).resolve()) if found else None


def exec_line() -> tuple[str, str]:
    """The Exec= to write, and why."""
    if packaged := found_by_generator():
        return BINARY, f"{packaged} is on the generator's own PATH; the bare name resolves"
    if local := found_anywhere():
        return local, f"{local} is not on the generator's PATH, so a bare name would be skipped"
    raise SystemExit(
        f"{BINARY} is not installed anywhere on PATH. Build and install it first;\n"
        "an autostart entry pointing at nothing is worse than none."
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="report and change nothing")
    parser.add_argument("--remove", action="store_true")
    args = parser.parse_args()

    if args.remove:
        if TARGET.exists():
            TARGET.unlink()
            print(f"removed {TARGET}")
        else:
            print("nothing installed")
        return 0

    if not SOURCE.exists():
        raise SystemExit(f"packaging source missing: {SOURCE}")
    command, why = exec_line()
    print(f"Exec={command}\n  because {why}")

    body = "\n".join(
        f"Exec={command}" if line.startswith("Exec=") else line
        for line in SOURCE.read_text().splitlines()
    ) + "\n"

    if args.check:
        current = TARGET.read_text() if TARGET.exists() else None
        if current == body:
            print(f"\n{TARGET} is already correct")
            return 0
        print(f"\n{TARGET} " + ("differs" if current else "is not installed"))
        return 1

    TARGET.parent.mkdir(parents=True, exist_ok=True)
    TARGET.write_text(body)
    print(f"\ninstalled {TARGET}")

    # Prove it rather than promise it. The generator is the thing that decides,
    # so ask the generator.
    out = pathlib.Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp")) / "cirrove-generator-check"
    for sub in ("normal", "early", "late"):
        (out / sub).mkdir(parents=True, exist_ok=True)
    generator = "/usr/lib/systemd/user-generators/systemd-xdg-autostart-generator"
    if not pathlib.Path(generator).exists():
        print("(generator not found; cannot verify here)")
        return 0
    subprocess.run(
        [generator, str(out / "normal"), str(out / "early"), str(out / "late")],
        capture_output=True,
        text=True,
        timeout=30,
    )
    units = [p.name for p in (out / "late").iterdir() if "Cirrove" in p.name]
    shutil.rmtree(out, ignore_errors=True)
    if units:
        print(f"verified: the generator produces {units[0]}")
        return 0
    print(
        "the generator still produces no unit for this entry.\n"
        "Check `journalctl --user -b | grep xdg-autostart` for its reason.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
