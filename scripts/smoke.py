#!/usr/bin/env python3
"""Exercise built binaries with private temporary state and no cloud access."""
import json
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
CLI = ROOT / "target/debug/cirrove"
DAEMON = ROOT / "target/debug/cirroved"

with tempfile.TemporaryDirectory(prefix="cirrove-smoke-") as directory:
    root = Path(directory)
    subprocess.run([str(CLI), "demo", "--state-dir", str(root / "demo")], check=True)
    socket = root / "run/control.sock"
    with (root / "daemon.log").open("w+") as log:
        process = subprocess.Popen([str(DAEMON), "--state-dir", str(root / "state"), "--socket", str(socket)], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while not socket.exists():
                if process.poll() is not None or time.monotonic() > deadline:
                    log.seek(0)
                    raise RuntimeError("Daemon did not start: " + log.read())
                time.sleep(0.02)
            reply = json.loads(subprocess.check_output([str(CLI), "status", "--socket", str(socket)], timeout=5))
            assert reply["milestone"] == "readonly-preview", reply
            assert reply["active_mounts"] == 0, reply
            process.terminate()
            assert process.wait(timeout=5) == 0
            assert not socket.exists(), "socket left behind after SIGTERM"
            print("Smoke passed: demo, status, SIGTERM and socket cleanup; no cloud access.")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
