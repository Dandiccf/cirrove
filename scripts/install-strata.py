#!/usr/bin/python3
"""Install/remove only the opt-in Strata provider; no daemon or desktop changes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile

REPO = Path(__file__).resolve().parents[1]
ID = "cirrove"
FILES = ("provider.json", "cirrove-provider.py", "kept.png", "fetching.png", "mark.png")
MARKER = ".cirrove-install.json"


def trusted(path):
    return (not path.is_symlink() and path.stat().st_uid == os.getuid()
            and path.stat().st_mode & 0o022 == 0)


def install(config, remove=False, socket=None):
    if not config.is_absolute():
        raise ValueError("configuration path must be absolute")
    target = config / "strata/providers" / ID
    if remove and not target.exists():
        return
    # Check every configuration component for install and uninstall alike.
    for p in (config, config / "strata", config / "strata/providers", target):
        if p.exists() or p.is_symlink():
            if not p.is_dir() or not trusted(p):
                raise ValueError("untrusted configuration directory")
        elif not remove:
            p.mkdir(mode=0o700, parents=True)
    marker = target / MARKER
    existing = list(target.iterdir())
    if existing:
        if not marker.is_file() or marker.is_symlink():
            raise ValueError("existing provider is not a Cirrove-managed installation")
        owned = json.loads(marker.read_text())
        if set(owned) != set(FILES) or {p.name for p in existing} != set(FILES) | {MARKER}:
            raise ValueError("unexpected files in provider directory; preserving them")
        for name, digest in owned.items():
            p = target / name
            if not trusted(p) or hashlib.sha256(p.read_bytes()).hexdigest() != digest:
                raise ValueError("provider was edited; preserving the installation")
    if remove:
        for name in (*FILES, MARKER):
            (target / name).unlink()
        target.rmdir()  # Empty only; never recursive.
        return
    command = ["/usr/bin/python3", str(target / "cirrove-provider.py")]
    if socket:
        command += ["--socket", str(socket)]
    manifest = {"version": 1, "id": ID, "name": "Cirrove", "command": command,
                "icons": {name: name + ".png" for name in ("kept", "fetching", "mark")}}
    contents = {"provider.json": (json.dumps(manifest, indent=2) + "\n").encode(),
                "cirrove-provider.py": (REPO / "packaging/strata/cirrove-provider.py").read_bytes()}
    contents.update({name + ".png": (REPO / f"packaging/strata/icons/{name}.png").read_bytes()
                     for name in ("kept", "fetching", "mark")})
    contents[MARKER] = (json.dumps({k: hashlib.sha256(v).hexdigest() for k, v in contents.items()}, indent=2) + "\n").encode()
    for name, data in contents.items():
        with tempfile.NamedTemporaryFile(dir=target, prefix=".install-", delete=False) as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
            temp = stream.name
        os.replace(temp, target / name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config-home", type=Path, default=Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config"))
    parser.add_argument("--remove", action="store_true")
    parser.add_argument("--socket", type=Path, help="isolated test daemon socket")
    args = parser.parse_args()
    try:
        install(args.config_home, args.remove, args.socket)
    except (OSError, ValueError) as e:
        print(f"Strata integration: {e}", file=sys.stderr)
        return 1
    print("Cirrove Strata provider removed." if args.remove else "Cirrove Strata provider installed. Restart the provider-enabled Strata build to load it.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
