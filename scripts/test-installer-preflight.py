#!/usr/bin/env python3
"""Actual developer installer, synthetic typed systemd replies, no real mutations.

Every first mutation is caught by a PATH fixture and exits97. No daemon is
launched. Temporary fixtures are retained deliberately, without recursive cleanup.
"""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
UNIT = "cirroved.service"
UNIT_PATH = "/org/freedesktop/systemd1/unit/cirroved_2eservice"
POLICY_ANCHOR = "docs/development.md#flat-numbers-source-journal-schema21-held-prerelease-policy"


class InstallerPreflight(unittest.TestCase):
    def setUp(self):
        self.fixture = Path(tempfile.mkdtemp(prefix="cirrove-installer-v4-", dir="/var/tmp"))
        self.fixture.chmod(0o700)
        self.repo, self.home, self.fakes = [self.fixture / x for x in ("repo", "home", "commands")]
        for path in [self.repo / "scripts", self.repo / "packaging/systemd",
                     self.repo / "docs/benchmarks", self.home, self.fakes,
                     self.fixture / "runtime", self.fixture / "ambient-empty"]:
            path.mkdir(parents=True, exist_ok=True)
        # The production installer itself is copied, never rewritten for a test.
        shutil.copyfile(ROOT / "scripts/install-developer.sh", self.repo / "scripts/install-developer.sh")
        shutil.copyfile(ROOT / "packaging/systemd/cirroved.service", self.repo / "packaging/systemd/cirroved.service")
        helper = ROOT / "scripts/install-preflight.py"
        if helper.exists():
            shutil.copyfile(helper, self.repo / "scripts/install-preflight.py")
        self.config = self.fixture / "fake-query-config.json"
        self.marker, self.queries = [self.fixture / x for x in ("first-mutations.jsonl", "queries.jsonl")]
        self.state = self.home / ".local/state/cirrove"
        self.retained = {}
        # Do not inherit shell init hooks, credentials, preload settings or a
        # live runtime route from the developer session.
        self.env = {"LANG": "C.UTF-8"}
        self.env.update(HOME=str(self.home), XDG_STATE_HOME=str(self.fixture / "ambient-empty"),
                        XDG_RUNTIME_DIR=str(self.fixture / "runtime"),
                        CARGO_TARGET_DIR=str(self.fixture / "target"),
                        INSTALLER_FIXTURE_CONFIG=str(self.config),
                        INSTALLER_FIXTURE_MUTATIONS=str(self.marker),
                        INSTALLER_FIXTURE_QUERIES=str(self.queries),
                        PATH=str(self.fakes) + ":/usr/bin:/bin")
        self.command("pacman", "#!/bin/sh\nexit 1\n")
        self.command("pgrep", "#!/bin/sh\nexit 1\n")
        for name in ["cargo", "install", "systemctl", "pkill", "setsid", "nohup", "nautilus",
                     "cmake", "gtk-update-icon-cache", "msgfmt", "sleep"]:
            self.command(name, '#!/bin/sh\nprintf "%s\\n" "' + name + '" >> "$INSTALLER_FIXTURE_MUTATIONS"\nexit 97\n')
        self.command("python3", '''#!/usr/bin/python3
import importlib.util, json, os, pwd, sys
from pathlib import Path
c = json.loads(Path(os.environ["INSTALLER_FIXTURE_CONFIG"]).read_text())
script = Path(sys.argv[1])
if script.name != "install-preflight.py":
    with open(os.environ["INSTALLER_FIXTURE_MUTATIONS"], "a") as f: f.write("python3\\n")
    sys.exit(97)
# Substitute only the OS user-home query. All parser/route/embargo code remains
# production code; no production test environment hook is introduced.
original = pwd.getpwuid
def fixture_passwd(uid):
    row = list(original(uid)); row[5] = c["service_home"]
    return pwd.struct_passwd(row)
pwd.getpwuid = fixture_passwd
sys.argv = sys.argv[1:]
spec = importlib.util.spec_from_file_location("fixture_actual_install_preflight", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
if "process" in c:
    # One arm substitutes the kernel observation only. The actual production
    # route parser still validates that pinned synthetic argv/state binding.
    def fixture_identity(pid):
        row = c["process"]
        if row["pid"] != pid or str(module.route(row["exe"], row["argv"])) != row["state"]:
            raise module.Refusal("synthetic live-process binding changed")
        return row
    module.process_identity = fixture_identity
sys.exit(module.main(sys.argv[1:]))
''')
        self.command("busctl", '''#!/usr/bin/python3
import json, os, sys
from pathlib import Path
c = json.loads(Path(os.environ["INSTALLER_FIXTURE_CONFIG"]).read_text())
log = Path(os.environ["INSTALLER_FIXTURE_QUERIES"])
argv = sys.argv[1:]
previous = [json.loads(x) for x in log.read_text().splitlines()] if log.exists() else []
cycle = sum("ListUnitsByNames" in x for x in previous)
if "ListUnitsByNames" not in argv: cycle = max(cycle - 1, 0)
s = c["snapshots"][min(cycle, len(c["snapshots"]) - 1)]
with log.open("a") as f: f.write(json.dumps(argv) + "\\n")
if "--user" not in argv or "--json=short" not in argv:
    sys.exit(96)
if "ListUnitsByNames" in argv:
    if argv[-3:] != ["as", "1", "cirroved.service"]: sys.exit(96)
    rows = [[
        "cirroved.service", "Synthetic fixture", s["load"],
        "active" if s["pid"] else "inactive", "running" if s["pid"] else "dead", "",
        "/org/freedesktop/systemd1/unit/cirroved_2eservice", 0, "", "/"]]
    result = {"type": "a(ssssssouso)", "data": [rows]}
elif "get-property" in argv:
    prop = argv[-1]
    result = {
        "LoadState": {"type": "s", "data": s["load"]},
        "MainPID": {"type": "u", "data": s["pid"]},
        "InvocationID": {"type": "ay", "data": s["invocation"]},
        "ExecStart": {"type": "a(sasbttttuii)", "data": s["exec"]},
    }.get(prop)
    if result is None or "/org/freedesktop/systemd1/unit/cirroved_2eservice" not in argv:
        sys.exit(96)
else:
    sys.exit(96)
if c.get("malformed_busctl"): result = {"type": "u", "data": "not-a-pid"}
print(json.dumps(result))
''')
        self.policy("held")
        self.query_config([self.snapshot()])

    def command(self, name, contents):
        path = self.fakes / name
        path.write_text(contents)
        path.chmod(0o700)

    def snapshot(self, route=None, argv=None, executable=None, load="loaded", pid=0, invocation=None):
        executable = executable or str(self.home / ".local/bin/cirroved")
        argv = argv if argv is not None else [executable, "--state-dir", str(route or self.state),
                                             "--socket", str(self.fixture / "runtime/cirrove/control.sock")]
        return {"load": load, "pid": pid, "invocation": invocation or [0] * 16,
                "exec": [] if load == "not-found" else [[executable, argv, False, 0, 0, 0, 0, 0, 0, 0]]}

    def query_config(self, snapshots, service_home=None, **extra):
        self.config.write_text(json.dumps({"snapshots": snapshots,
                                          "service_home": str(service_home or self.home), **extra}))

    def policy(self, state):
        (self.repo / "packaging/developer-install-policy.json").write_text(json.dumps({
            "version": 1, "state": state, "journal_schema": 21, "metadata_schema": 8,
            "policy_document": POLICY_ANCHOR}))

    def remember(self, path, data=b"retained synthetic local bytes"):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(0o600)
        self.retained[path] = (data, path.stat().st_ino, path.stat().st_mode)

    def accounts(self, directory=None, provider="microsoft", access="read_only", enabled=True):
        directory = directory or self.state
        self.remember(directory / "accounts.json", json.dumps({"version": 2, "accounts": [{
            "id": "11111111-1111-4111-8111-111111111111", "label": "synthetic-owned",
            "registration": {"provider": provider}, "access": access, "enabled": enabled}]}).encode())
        self.remember(directory / "accounts/11111111-1111-4111-8111-111111111111/unsent-marker")

    def embargo(self, **updates):
        item = {"version": 1, "hostname": socket.gethostname(), "uid": os.getuid(),
                "scope": "user", "unit": UNIT, "state": "active",
                "owner_pid": 123456789, "owner_started_at": "2026-10-03T00:00:00Z"}
        item.update(updates)
        (self.repo / "docs/benchmarks/synthetic-embargo.json").write_text(json.dumps({
            "status": "historical-open", "restart_embargo": item}))

    def run_installer(self, no_build=False):
        argv = ["bash", str(self.repo / "scripts/install-developer.sh")]
        if no_build: argv.append("--no-build")
        result = subprocess.run(argv, cwd=self.repo, env=self.env, capture_output=True, timeout=15)
        for path, (data, inode, mode) in self.retained.items():
            self.assertEqual(path.read_bytes(), data, "retained synthetic bytes changed")
            self.assertEqual(path.stat().st_ino, inode)
            self.assertEqual(path.stat().st_mode, mode)
        return result

    def refused(self, no_build=False):
        result = self.run_installer(no_build)
        boundary = self.marker.read_text().strip() if self.marker.exists() else "none"
        self.assertFalse(self.marker.exists(), "installer reached first fake mutation boundary: " + boundary)
        self.assertEqual(result.returncode, 1, result.stderr.decode(errors="replace"))

    def boundary(self, no_build=False):
        result = self.run_installer(no_build)
        self.assertEqual(result.returncode, 97, result.stderr.decode(errors="replace"))
        self.assertEqual(self.marker.read_text().splitlines(), ["install" if no_build else "cargo"])

    def test_held_ordinary_retained_account(self):
        self.accounts(access="read_write"); self.refused()

    def test_held_readonly_icloud_account(self):
        self.accounts(provider="icloud"); self.refused()

    def test_held_disabled_account(self):
        self.accounts(enabled=False); self.refused()

    def test_stopped_shipped_home_retained_empty_ambient_xdg(self):
        self.remember(self.state / "metadata.db"); self.refused()

    def test_stopped_custom_effective_route_retained(self):
        custom = self.fixture / "old-custom-state"
        self.accounts(custom); self.query_config([self.snapshot(route=custom)]); self.refused()

    def test_future_shipped_home_retained_old_route_fresh(self):
        self.accounts(); self.query_config([self.snapshot(route=self.fixture / "old-fresh-state")]); self.refused()

    def test_current_live_route_retained_effective_and_future_fresh(self):
        live = self.fixture / "live-retained-state"
        self.remember(live / "objects/owned-unsent")
        executable = str(self.home / ".local/bin/cirroved")
        process = {"pid": 12345, "started": 987654, "exe": executable,
                   "exe_device": 42, "exe_inode": 73,
                   "argv": [executable, "--state-dir", str(live)], "state": str(live)}
        self.query_config([self.snapshot(pid=12345)], process=process)
        self.refused()

    def test_retained_spool_without_settings(self):
        self.remember(self.state / "objects/owned-unsent"); self.refused()

    def test_retained_empty_settings(self):
        self.remember(self.state / "accounts.json", b'{"version":2,"accounts":[]}'); self.refused()

    def test_malformed_settings_refused(self):
        self.remember(self.state / "accounts.json", b"{"); self.refused()

    def test_state_symlink_refused(self):
        target = self.fixture / "symlink-target"; self.accounts(target)
        self.state.parent.mkdir(parents=True); self.state.symlink_to(target, target_is_directory=True)
        self.refused()

    def test_settings_symlink_refused(self):
        target = self.fixture / "settings-target"; self.remember(target, b'{"version":2,"accounts":[]}')
        self.state.mkdir(parents=True); (self.state / "accounts.json").symlink_to(target); self.refused()

    def test_missing_source_policy_refused(self):
        (self.repo / "packaging/developer-install-policy.json").unlink(); self.refused()

    def test_malformed_source_policy_refused(self):
        (self.repo / "packaging/developer-install-policy.json").write_text("{"); self.refused()

    def test_source_policy_symlink_refused(self):
        marker = self.repo / "packaging/developer-install-policy.json"
        target = self.fixture / "policy-target"; target.write_bytes(marker.read_bytes())
        marker.unlink(); marker.symlink_to(target); self.refused()

    def test_unknown_effective_route_refused(self):
        self.policy("released"); self.query_config([self.snapshot(argv=[str(self.home / ".local/bin/cirroved")])]); self.refused(True)

    def test_wrapper_effective_route_refused(self):
        self.policy("released"); self.query_config([self.snapshot(executable="/usr/bin/env", argv=["/usr/bin/env", "cirroved", "--state-dir", str(self.state)])]); self.refused(True)

    def test_relative_effective_route_refused(self):
        self.policy("released"); self.query_config([self.snapshot(route="relative-state")]); self.refused(True)

    def test_duplicate_effective_route_refused(self):
        self.policy("released"); exe = str(self.home / ".local/bin/cirroved")
        self.query_config([self.snapshot(argv=[exe, "--state-dir", str(self.state), "--state-dir=" + str(self.state)])]); self.refused(True)

    def test_home_binding_mismatch_refused(self):
        self.policy("released"); self.query_config([self.snapshot()], service_home=self.fixture / "other-user-home"); self.refused(True)

    def test_invocation_drift_refused(self):
        self.policy("released"); self.query_config([self.snapshot(), self.snapshot(invocation=[1] * 16)]); self.refused(True)

    def test_pid_drift_refused(self):
        self.policy("released"); self.query_config([self.snapshot(), self.snapshot(pid=2147483647)]); self.refused(True)

    def test_effective_route_drift_refused(self):
        self.policy("released"); self.query_config([self.snapshot(), self.snapshot(route=self.fixture / "changed-state")]); self.refused(True)

    def test_malformed_typed_service_reply_refused(self):
        self.policy("released"); self.query_config([self.snapshot()], malformed_busctl=True); self.refused(True)

    def test_active_exact_embargo_refused(self):
        self.policy("released"); self.embargo(); self.refused(True)

    def test_malformed_declared_embargo_refused(self):
        self.policy("released")
        (self.repo / "docs/benchmarks/broken.json").write_text('{"restart_embargo":{"state":"active"}}')
        self.refused(True)

    def test_held_no_build_fresh_artifact_provenance_refused(self):
        self.refused(True)

    def test_future_template_changed_state_refused(self):
        path = self.repo / "packaging/systemd/cirroved.service"
        text = path.read_text()
        self.assertIn("--state-dir %h/.local/state/cirrove", text)
        path.write_text(text.replace("--state-dir %h/.local/state/cirrove", "--state-dir %h/.local/state/changed", 1))
        self.refused()

    def test_future_template_reset_execstart_refused(self):
        path = self.repo / "packaging/systemd/cirroved.service"
        text = path.read_text()
        self.assertIn("ExecStart=", text)
        path.write_text(text.replace("ExecStart=", "ExecStart=\nExecStart=", 1))
        self.refused()

    def test_future_template_duplicate_execstart_refused(self):
        path = self.repo / "packaging/systemd/cirroved.service"
        text = path.read_text()
        command = next(line for line in text.splitlines() if line.startswith("ExecStart="))
        path.write_text(text.replace(command, command + "\n" + command, 1))
        self.refused()

    def test_held_fresh_normal_build_reaches_cargo_only(self):
        self.boundary()

    def test_held_fresh_not_found_unit_reaches_cargo_only(self):
        self.query_config([self.snapshot(load="not-found")]); self.boundary()

    def test_released_retained_routine_install_reaches_install_only(self):
        self.policy("released"); self.accounts(); self.boundary(True)

    def test_closed_exact_embargo_permits_install(self):
        self.policy("released"); self.embargo(state="closed"); self.boundary(True)

    def test_foreign_host_embargo_permits_install(self):
        self.policy("released"); self.embargo(hostname="synthetic-foreign.invalid"); self.boundary(True)

    def test_foreign_uid_embargo_permits_install(self):
        self.policy("released"); self.embargo(uid=os.getuid() + 1); self.boundary(True)

    def test_other_unit_embargo_permits_install(self):
        self.policy("released"); self.embargo(unit="synthetic-other.service"); self.boundary(True)

    def test_postbuild_new_exact_embargo_refuses_before_install(self):
        # This single arm is the reviewed exception to first-cargo exit97:
        # fake cargo performs no build, records the boundary, declares a window,
        # then exits0 so the genuine second preflight must prevent copying.
        self.policy("released")
        self.command("find", "#!/bin/sh\nexit 0\n")
        self.command("cargo", '''#!/usr/bin/python3
import json, os, socket, sys
from pathlib import Path
if sys.argv[1:] != ["build", "--release", "--locked", "--workspace"]:
    sys.exit(96)
with open(os.environ["INSTALLER_FIXTURE_MUTATIONS"], "a") as f: f.write("cargo\\n")
repo = Path(os.environ["INSTALLER_FIXTURE_CONFIG"]).parent / "repo"
(repo / "docs/benchmarks/post-build-window.json").write_text(json.dumps({
    "restart_embargo": {"version": 1, "hostname": socket.gethostname(), "uid": os.getuid(),
                       "scope": "user", "unit": "cirroved.service", "state": "active"}}))
sys.exit(0)
''')
        result = self.run_installer()
        self.assertTrue(self.marker.exists(), "fake cargo was not reached")
        self.assertEqual(self.marker.read_text().splitlines(), ["cargo"],
                         "second preflight failed to prevent first installed-file mutation")
        self.assertEqual(result.returncode, 1, result.stderr.decode(errors="replace"))
        self.assertIn(b"declared measurement", result.stderr)

    def test_historical_open_status_permits_install(self):
        self.policy("released")
        (self.repo / "docs/benchmarks/historical.json").write_text('{"status":"live-acceptance-open"}')
        self.boundary(True)


if __name__ == "__main__":
    unittest.main()
