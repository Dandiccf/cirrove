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
        command = [str(DAEMON), "--state-dir", str(root / "state"), "--socket", str(socket)]
        process = None

        def start():
            global process
            process = subprocess.Popen(command, stdout=log, stderr=log)
            deadline = time.monotonic() + 10
            while process.poll() is None and time.monotonic() < deadline:
                result = subprocess.run([str(CLI), "status", "--socket", str(socket)], capture_output=True, timeout=5)
                if result.returncode == 0:
                    return json.loads(result.stdout)
                time.sleep(0.02)
            log.seek(0)
            raise RuntimeError("Daemon did not start: " + log.read())

        try:
            reply = start()
            assert reply["milestone"] == "readonly-preview", reply
            assert reply["active_mounts"] == 0, reply
            other = subprocess.run(command, stdout=log, stderr=log, timeout=5)
            assert other.returncode != 0, "second daemon acquired the same account state"
            assert process.poll() is None, "second daemon interrupted the active service"
            process.kill()
            process.wait(timeout=5)
            assert socket.exists(), "abrupt termination did not leave a recovery fixture"
            reply = start()
            assert reply["active_mounts"] == 0, reply
            process.terminate()
            assert process.wait(timeout=5) == 0
            assert not socket.exists(), "socket left behind after SIGTERM"
            print("Smoke passed: demo, status, ownership, SIGKILL recovery and SIGTERM cleanup; no cloud access.")
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=5)
