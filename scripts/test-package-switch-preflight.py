#!/usr/bin/env python3
"""Package-switch guard checks; optional actual-script filesystem namespace proof.

--namespace runs the real switch script under bubblewrap with the passwd HOME
path unchanged and all mutations replaced by exit97 fixtures. Fixtures remain
on disk. Default portable guard tests do not require namespace privileges.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import shutil
import socket
import stat
from types import SimpleNamespace
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
NAMESPACE = "--namespace" in sys.argv
if NAMESPACE:
    sys.argv.remove("--namespace")


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


if NAMESPACE:
    class PackageSwitchNamespace(unittest.TestCase):
        def setUp(self):
            self.home = Path(pwd.getpwuid(os.getuid()).pw_dir)
            self.assertEqual(os.environ.get("HOME"), str(self.home))
            self.fixture = Path(tempfile.mkdtemp(prefix="package-switch-", dir=os.environ["TMPDIR"]))
            self.backing = self.fixture / "home"
            self.repo = self.fixture / "repo"
            self.bin = self.fixture / "bin"
            self.vendor = self.fixture / "vendor"
            self.etc = self.fixture / "etc-user"
            for p in (self.backing, self.repo / "scripts", self.repo / "packaging/systemd",
                      self.repo / "docs/benchmarks", self.bin, self.vendor, self.etc,
                      self.fixture / "tmp"):
                p.mkdir(parents=True, exist_ok=True)
            for name in ("switch-to-package.sh", "install-preflight.py"):
                shutil.copyfile(ROOT / "scripts" / name, self.repo / "scripts" / name)
            shutil.copyfile(ROOT / "packaging/developer-install-policy.json", self.repo / "packaging/developer-install-policy.json")
            template = (ROOT / "packaging/systemd/cirroved.service").read_text()
            (self.repo / "packaging/systemd/cirroved.service").write_text(template)
            (self.vendor / "cirroved.service").write_text(template.replace("%h/.local/bin/cirroved", "/usr/bin/cirroved"))
            unit = self.backing / ".config/systemd/user/cirroved.service"
            unit.parent.mkdir(parents=True)
            unit.write_text(template)
            (self.backing / "fixture-home-sentinel").write_text("isolated home")
            self.marker = self.fixture / "mutations.jsonl"
            self.config = self.fixture / "query.json"
            runtime = f"/run/user/{os.getuid()}"
            unitpaths = [str(self.home / ".config/systemd/user"), runtime + "/systemd/user", "/etc/systemd/user", "/usr/lib/systemd/user"]
            self.config.write_text(json.dumps({"home": str(self.home), "unitpaths": unitpaths,
                "fragment": str(self.home / ".config/systemd/user/cirroved.service"),
                "state": str(self.home / ".local/state/cirrove")}))
            for name in ("systemctl", "rm", "pkill", "gtk-update-icon-cache", "setsid", "nohup",
                         "nautilus", "cirroved", "cirrove", "cirrove-tray", "cirrove-desktop"):
                path = self.bin / name
                path.write_text('#!/usr/bin/python3\nimport json,os,sys\nwith open("/var/tmp/mutations.jsonl","a") as f: f.write(json.dumps({"command":os.path.basename(sys.argv[0]),"argv":sys.argv[1:]})+"\\n")\nsys.exit(97)\n')
                path.chmod(0o700)
            daemon = self.bin / "cirroved"
            daemon.write_text('#!/usr/bin/python3\nimport json,sys\nif sys.argv[1:] == ["--storage-format-json"]:\n print(json.dumps({"version":1,"product":"cirroved","journal_schema":21,"metadata_schema":8})); sys.exit(0)\nsys.exit(97)\n')
            daemon.chmod(0o700)
            bus = self.bin / "busctl"
            bus.write_text('''#!/usr/bin/python3
import json,sys
from pathlib import Path
c=json.loads(Path("/var/tmp/query.json").read_text()); a=sys.argv[1:]
if a[:2]!=["--user","--json=short"]: sys.exit(96)
if "ListUnitsByNames" in a:
 r={"type":"a(ssssssouso)","data":[[["cirroved.service","Fixture","loaded","inactive","dead","","/org/freedesktop/systemd1/unit/cirroved_2eservice",0,"","/"]]]}
else:
 r={"LoadState":{"type":"s","data":"loaded"},"MainPID":{"type":"u","data":0},"InvocationID":{"type":"ay","data":[0]*16},"ExecStart":{"type":"a(sasbttttuii)","data":[[c["home"]+"/.local/bin/cirroved",[c["home"]+"/.local/bin/cirroved","--state-dir",c["state"],"--socket","/run/cirrove/control.sock"],False,0,0,0,0,0,0,0]]},"UnitPath":{"type":"as","data":c["unitpaths"]},"FragmentPath":{"type":"s","data":c["fragment"]},"DropInPaths":{"type":"as","data":[]},"Transient":{"type":"b","data":False}}.get(a[-1])
 if r is None: sys.exit(96)
print(json.dumps(r))
''')
            bus.chmod(0o700)
            self.command = [shutil.which("bwrap"), "--unshare-all", "--die-with-parent", "--ro-bind", "/", "/",
                "--bind", str(self.fixture), "/var/tmp", "--bind", str(self.backing), str(self.home),
                "--tmpfs", "/run", "--tmpfs", "/tmp", "--dev", "/dev", "--proc", "/proc",
                "--tmpfs", "/usr/bin", "--ro-bind", str(self.vendor), "/usr/lib/systemd/user",
                "--ro-bind", str(self.etc), "/etc/systemd/user"]
            for name in ("bash", "python3", "env", "dirname", "sed"):
                self.command += ["--ro-bind", str(Path("/usr/bin", name).resolve()), "/usr/bin/" + name]
            for path in self.bin.iterdir():
                self.command += ["--ro-bind", str(path), "/usr/bin/" + path.name]
            self.command += ["--clearenv", "--setenv", "HOME", str(self.home), "--setenv", "PATH", "/usr/bin:/bin",
                "--setenv", "LANG", "C.UTF-8", "--setenv", "TMPDIR", "/var/tmp/tmp",
                "--setenv", "SQLITE_TMPDIR", "/var/tmp/tmp", "--setenv", "XDG_RUNTIME_DIR", runtime,
                "--chdir", "/var/tmp/repo", "/usr/bin/bash", "-c",
                'test "$(/usr/bin/python3 -c \'import pwd,os;print(pwd.getpwuid(os.getuid()).pw_dir)\')" = "$HOME" && test -f "$HOME/fixture-home-sentinel" && test ! -e /run/user/' + str(os.getuid()) + '/bus && exec /usr/bin/bash scripts/switch-to-package.sh']

        def run_switch(self, refusal):
            originals = {str(p): (sha(p), p.stat().st_ino, p.stat().st_mode) for p in self.backing.rglob("*") if p.is_file()}
            record = {"command": self.command, "bwrap_sha256": sha(self.command[0]), "runner_pid": os.getpid(),
                      "copied_source_sha256": {str(p.relative_to(self.repo)): sha(p) for p in [self.repo / "scripts/switch-to-package.sh", self.repo / "scripts/install-preflight.py", self.repo / "packaging/systemd/cirroved.service", self.repo / "packaging/developer-install-policy.json"]}, "expected_duration_seconds": 20,
                      "HOME_unchanged": str(self.home), "all_mutations_stubbed": True}
            manifest = self.fixture / "run.json"
            manifest.write_text(json.dumps(record, indent=2) + "\n")
            child = subprocess.Popen(self.command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            record["child_pid"] = child.pid
            manifest.write_text(json.dumps(record, indent=2) + "\n")
            try:
                out, err = child.communicate(timeout=20)
            except subprocess.TimeoutExpired:
                child.kill()
                out, err = child.communicate(timeout=5)
                record["timed_out"] = True
            (self.fixture / "stdout").write_bytes(out); (self.fixture / "stderr").write_bytes(err)
            record.update(exit_code=child.returncode, stdout_sha256=sha(self.fixture / "stdout"), stderr_sha256=sha(self.fixture / "stderr"))
            calls = [json.loads(l) for l in self.marker.read_text().splitlines()] if self.marker.exists() else []
            record["mutation_calls"] = calls
            manifest.write_text(json.dumps(record, indent=2) + "\n")
            for path, pin in originals.items():
                p = Path(path); self.assertEqual((sha(p), p.stat().st_ino, p.stat().st_mode), pin)
            print(json.dumps({"fixture": str(self.fixture), "exit_code": child.returncode, "calls": calls}), flush=True)
            # A namespace/parser failure cannot count as the expected boundary red.
            if record.get("timed_out") or child.returncode not in (1, 97):
                self.fail("namespace/script setup failed: " + err.decode(errors="replace"))
            if refusal is None:
                self.assertEqual(calls, [{"command": "systemctl", "argv": ["--user", "disable", "--now", "cirroved.service"]},
                                         {"command": "rm", "argv": ["-f", str(self.home / ".config/systemd/user/cirroved.service")]}])
                self.assertEqual(child.returncode, 97)
                return
            self.assertEqual(calls, [], "guard must refuse before stubbed stop or removal")
            self.assertEqual(child.returncode, 1)
            self.assertIn(refusal, err.decode(errors="replace"))

        def test_active_embargo_refuses_before_stop_or_remove(self):
            item = {"version":1,"hostname":socket.gethostname(),"uid":os.getuid(),"scope":"user","unit":"cirroved.service","state":"active"}
            (self.repo / "docs/benchmarks/active.json").write_text(json.dumps({"restart_embargo":item}))
            self.run_switch("declared measurement forbids restarting this service")

        def test_empty_known_state_reaches_stubbed_switch(self):
            self.run_switch(None)

        def test_held_retained_state_refuses_before_stop_or_remove(self):
            state = self.backing / ".local/state/cirrove"
            state.mkdir(parents=True)
            (state / "accounts.json").write_text("synthetic retained marker")
            self.run_switch("package schema provenance does not attest retained local state")


class PackageSwitchPermission(unittest.TestCase):
    def test_unreadable_retained_state_is_not_reported_empty(self):
        spec = importlib.util.spec_from_file_location("package_switch_permission_guard", ROOT / "scripts/install-preflight.py")
        guard = importlib.util.module_from_spec(spec); spec.loader.exec_module(guard)
        fixture = Path(tempfile.mkdtemp(prefix="package-state-permission-", dir=os.environ.get("TMPDIR", "/var/tmp")))
        state = fixture / "state"; state.mkdir()
        marker = state / "accounts.json"; marker.write_bytes(b"retained synthetic marker")
        expected = (marker.read_bytes(), marker.stat().st_ino, marker.stat().st_mode)
        state.chmod(0)
        try:
            with self.assertRaisesRegex(guard.Refusal, "state inspection"):
                guard.retained_state(state)
        finally:
            # This only restores our local fixture after the bounded observation;
            # the production helper never chmods, retries or opens marker bytes.
            state.chmod(0o700)
            self.assertEqual((marker.read_bytes(), marker.stat().st_ino, marker.stat().st_mode), expected)


    def test_unreadable_embargo_directory_is_not_reported_empty(self):
        spec = importlib.util.spec_from_file_location("package_switch_embargo_permission", ROOT / "scripts/install-preflight.py")
        guard = importlib.util.module_from_spec(spec); spec.loader.exec_module(guard)
        fixture = Path(tempfile.mkdtemp(prefix="package-embargo-permission-", dir=os.environ.get("TMPDIR", "/var/tmp")))
        directory = fixture / "docs/benchmarks"; directory.mkdir(parents=True)
        declaration = directory / "active.json"
        declaration.write_text(json.dumps({"restart_embargo": {"version":1,"hostname":socket.gethostname(),"uid":os.getuid(),"scope":"user","unit":"cirroved.service","state":"active"}}))
        expected = (declaration.read_bytes(), declaration.stat().st_ino, declaration.stat().st_mode)
        directory.chmod(0)
        try:
            with self.assertRaisesRegex(guard.Refusal, "embargo inspection"):
                guard.embargo(fixture)
        finally:
            directory.chmod(0o700)
            self.assertEqual((declaration.read_bytes(), declaration.stat().st_ino, declaration.stat().st_mode), expected)


class PackageSwitchPortable(unittest.TestCase):
    """Actual guard with synthetic filesystem/query seams, never host services.

    HOME and passwd identity remain real. Only filesystem observations and typed
    service replies are virtual; this is not live-process or package provenance.
    """
    def setUp(self):
        spec = importlib.util.spec_from_file_location("package_switch_actual_preflight", ROOT / "scripts/install-preflight.py")
        self.guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.guard)
        self.home = Path(pwd.getpwuid(os.getuid()).pw_dir)
        self.assertEqual(os.environ.get("HOME"), str(self.home))
        runtime = Path("/run/user") / str(os.getuid())
        self.roots = [self.home / ".config/systemd/user.control", runtime / "systemd/user.control",
                      runtime / "systemd/transient", runtime / "systemd/generator.early",
                      self.home / ".config/systemd/user", Path("/etc/xdg/systemd/user"),
                      Path("/etc/systemd/user"), runtime / "systemd/user", Path("/run/systemd/user"),
                      runtime / "systemd/generator", self.home / ".local/share/systemd/user",
                      Path("/usr/local/share/systemd/user"), Path("/usr/share/systemd/user"),
                      Path("/usr/local/lib/systemd/user"), Path("/usr/lib/systemd/user"),
                      runtime / "systemd/generator.late"]
        self.template = (ROOT / "packaging/systemd/cirroved.service").read_bytes()
        self.target = Path("/usr/lib/systemd/user/cirroved.service")
        self.removable = self.home / ".config/systemd/user/cirroved.service"
        self.files = {str(ROOT / "packaging/systemd/cirroved.service"): self.template,
                      str(self.target): self.template.replace(b"%h/.local/bin/cirroved", b"/usr/bin/cirroved"),
                      str(self.removable): self.template}
        self.files.update({"/usr/bin/" + name: b"synthetic binary" for name in
                           ("cirroved", "cirrove", "cirrove-tray", "cirrove-desktop")})
        self.directories = set(map(str, self.roots))
        self.entries = {}
        self.symlinks = set()
        self.details = {"UnitPath": list(map(str, self.roots)), "FragmentPath": str(self.removable),
                        "DropInPaths": [], "Transient": False}
        self.policy = {"state": "held", "journal_schema": 21, "metadata_schema": 8}
        self.snapshot = {"effective_state": str(self.home / ".local/state/cirrove"), "process": None}
        self.retained = set()
        self.epoch = 0
        self.routing_reads = 0
        self.on_second_routing = None

    def properties(self, path, interface, name, signature):
        expected = {"UnitPath": (self.guard.MANAGER_PATH, "org.freedesktop.systemd1.Manager", "as"),
                    "FragmentPath": (self.guard.UNIT_PATH, "org.freedesktop.systemd1.Unit", "s"),
                    "DropInPaths": (self.guard.UNIT_PATH, "org.freedesktop.systemd1.Unit", "as"),
                    "Transient": (self.guard.UNIT_PATH, "org.freedesktop.systemd1.Unit", "b")}
        self.assertEqual((path, interface, signature), expected[name])
        if name == "UnitPath":
            self.routing_reads += 1
            if self.routing_reads == 2 and self.on_second_routing:
                self.on_second_routing()
        return self.details[name]

    def file_pin(self, path, maximum, executable=False):
        # Pure observation fixture; actual ownership checks have separate tests.
        self.guard.safe_path(path)
        data = self.files[str(path)]
        self.assertLessEqual(len(data), maximum)
        return {"stat": (self.epoch,), "sha256": hashlib.sha256(data).hexdigest()}

    def virtual_status(self, path, description="installed path"):
        if str(path) in self.files or str(path) in self.directories:
            mode = stat.S_IFDIR if str(path) in self.directories else stat.S_IFREG
            return SimpleNamespace(st_mode=mode | 0o755, st_dev=1, st_ino=1, st_uid=os.getuid())
        return None

    def guard_call(self, function=None):
        from contextlib import ExitStack
        with ExitStack() as stack:
            for obj, name, side in [(self.guard, "property_value", self.properties),
                                    (self.guard, "file_identity", self.file_pin),
                                    (self.guard, "strict_status", self.virtual_status),
                                    (Path, "exists", lambda p: str(p) in self.files or str(p) in self.directories),
                                    (Path, "is_symlink", lambda p: str(p) in self.symlinks),
                                    (Path, "is_dir", lambda p: str(p) in self.directories),
                                    (Path, "iterdir", lambda p: iter(self.entries.get(str(p), []))),
                                    (Path, "read_bytes", lambda p: self.files[str(p)])]:
                stack.enter_context(patch.object(obj, name, side))
            stack.enter_context(patch.object(self.guard, "source_policy", return_value=self.policy))
            stack.enter_context(patch.object(self.guard, "embargo"))
            # Only executable observation is synthetic here; strict parser/FD
            # query have separate actual local child fixtures below.
            stack.enter_context(patch.object(self.guard, "package_storage_format",
                return_value={"version": 1, "product": "cirroved", "journal_schema": 21, "metadata_schema": 8}))
            stack.enter_context(patch.object(self.guard, "service_snapshot", return_value=self.snapshot))
            stack.enter_context(patch.object(self.guard, "retained_state", side_effect=lambda p: str(p) in self.retained))
            return (function or (lambda: self.guard.package_preflight(ROOT)))()

    def refusal(self, reason):
        with self.assertRaisesRegex(self.guard.Refusal, reason):
            self.guard_call()

    def test_empty_supported_union_accepts_standard_generator_roots(self):
        self.guard_call()
        self.assertEqual(self.routing_reads, 2)

    def test_retained_home_effective_live_and_released_all_refuse(self):
        for route, released in [(self.home / ".local/state/cirrove", False),
                                (Path("/var/lib/custom-effective"), False),
                                (Path("/var/lib/custom-live"), False),
                                (self.home / ".local/state/cirrove", True)]:
            with self.subTest(route=str(route), released=released):
                self.snapshot = {"effective_state": str(Path("/var/lib/custom-effective")),
                                 "process": {"state": "/var/lib/custom-live"}}
                self.retained = {str(route)}
                self.policy = {"state": "released" if released else "held"}
                self.refusal("package schema provenance does not attest retained local state")

    def test_other_winning_candidates_refuse(self):
        for root in [Path("/etc/systemd/user"), self.roots[2], self.roots[3], self.roots[-1]]:
            with self.subTest(root=str(root)):
                name = str(root / "cirroved.service")
                self.files[name] = self.template
                self.refusal("another winning unit candidate")
                del self.files[name]

    def test_unit_and_type_wide_dropins_refuse(self):
        for name in ("cirroved.service.d", "service.d"):
            with self.subTest(name=name):
                directory = str(self.roots[4] / name)
                self.directories.add(directory)
                self.entries[directory] = [Path(directory) / "override.conf"]
                self.refusal("unit or type-wide drop-ins")
                self.directories.remove(directory)
                del self.entries[directory]

    def test_manager_override_and_custom_roots_refuse(self):
        for field, value in [("Transient", True), ("Transient", 0),
                             ("DropInPaths", ["/etc/override.conf"]),
                             ("FragmentPath", "/run/systemd/transient/cirroved.service"),
                             ("UnitPath", ["/custom", "/usr/lib/systemd/user"]),
                             ("UnitPath", ["/usr/lib/systemd/user", str(self.removable.parent)]),
                             ("UnitPath", ["/usr/lib/systemd/user"] * 2)]:
            with self.subTest(field=field, value=value):
                old = self.details[field]; self.details[field] = value
                self.refusal("unsupported")
                self.details[field] = old

    def test_target_or_source_extra_execution_directives_refuse(self):
        for extra in (b"ExecStartPre=/bin/true", b"ExecStartPost=/bin/true", b"EnvironmentFile=/x", b"RootDirectory=/x"):
            with self.subTest(extra=extra):
                original = self.files[str(self.target)]
                self.files[str(self.target)] = original.replace(b"[Service]", b"[Service]\n" + extra)
                self.refusal("supported exact target")
                # Editing both source and package does not authorize extra semantics.
                self.files[str(ROOT / "packaging/systemd/cirroved.service")] = self.template.replace(b"[Service]", b"[Service]\n" + extra)
                self.refusal("supported exact target")
                self.files[str(self.target)] = original
                self.files[str(ROOT / "packaging/systemd/cirroved.service")] = self.template

    def test_symlink_route_and_target_refuse(self):
        for path in [self.target, self.removable, self.home / ".local/state"]:
            with self.subTest(path=str(path)):
                self.symlinks.add(str(path)); self.refusal("unsafe"); self.symlinks.remove(str(path))

    def test_target_identity_change_is_detected_at_recheck(self):
        self.on_second_routing = lambda: setattr(self, "epoch", 1)
        self.refusal("changed during preflight")

    def test_service_snapshot_change_is_detected(self):
        def call():
            with patch.object(self.guard, "service_snapshot", side_effect=[self.snapshot, {**self.snapshot, "pid": 123}]):
                self.guard.package_preflight(ROOT)
        with self.assertRaisesRegex(self.guard.Refusal, "changed during preflight"):
            self.guard_call(call)

    def test_source_policy_change_is_detected(self):
        def call():
            with patch.object(self.guard, "source_policy", side_effect=[self.policy, {"state": "released"}]):
                self.guard.package_preflight(ROOT)
        with self.assertRaisesRegex(self.guard.Refusal, "policy changed"):
            self.guard_call(call)

    def test_late_embargo_and_retained_marker_are_detected(self):
        for kind in ("embargo", "retained"):
            with self.subTest(kind=kind):
                def call():
                    if kind == "embargo":
                        with patch.object(self.guard, "embargo", side_effect=[None, self.guard.Refusal("declared measurement")]):
                            self.guard.package_preflight(ROOT)
                    else:
                        with patch.object(self.guard, "retained_state", side_effect=[False, True]):
                            self.guard.package_preflight(ROOT)
                reason = "declared measurement" if kind == "embargo" else "retained local state"
                with self.assertRaisesRegex(self.guard.Refusal, reason):
                    self.guard_call(call)

    def test_second_format_query_route_drift_is_rechecked_before_mutation(self):
        current = dict(self.snapshot)
        queries = 0
        declaration = {"version": 1, "product": "cirroved", "journal_schema": 21, "metadata_schema": 8}
        def query(*_args):
            nonlocal current, queries
            queries += 1
            if queries == 2:
                # The former order already finished all service/route rereads.
                # Replace the observation, do not mutate the captured before.
                current = {"effective_state": "/var/lib/new-during-second-query", "process": None}
            return declaration
        def call():
            with patch.object(self.guard, "package_storage_format", side_effect=query), \
                    patch.object(self.guard, "service_snapshot", side_effect=lambda: current):
                self.guard.package_preflight(ROOT)
        with self.assertRaisesRegex(self.guard.Refusal, "changed during preflight"):
            self.guard_call(call)
        self.assertEqual(queries, 2, "did not observe the registered second-query boundary")

    def test_real_local_file_identity_checks_type_owner_mode_and_bytes(self):
        fixture = Path(tempfile.mkdtemp(prefix="package-file-pin-", dir=os.environ.get("TMPDIR", "/var/tmp")))
        path = fixture / "binary"
        path.write_bytes(b"synthetic binary"); path.chmod(0o700)
        first = self.guard.file_identity(path, 1024, True)
        self.assertEqual(first["sha256"], hashlib.sha256(b"synthetic binary").hexdigest())
        path.write_bytes(b"changed bytes")
        self.assertNotEqual(first, self.guard.file_identity(path, 1024, True))
        path.chmod(0o722)
        with self.assertRaisesRegex(self.guard.Refusal, "ownership or type"):
            self.guard.file_identity(path, 1024, True)
        path.chmod(0o600)
        with self.assertRaisesRegex(self.guard.Refusal, "ownership or type"):
            self.guard.file_identity(path, 1024, True)
        alias = fixture / "alias"; alias.symlink_to(path)
        with self.assertRaisesRegex(self.guard.Refusal, "unsafe"):
            self.guard.file_identity(alias, 1024)


if not NAMESPACE:
    class StorageFormatAttestation(unittest.TestCase):
        """Only generated local executables; no installed daemon or HOME edits."""
        def setUp(self):
            spec = importlib.util.spec_from_file_location("format_attestation_guard", ROOT / "scripts/install-preflight.py")
            self.guard = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(self.guard)
            self.policy = {"journal_schema": 21, "metadata_schema": 8}
            self.value = {"version": 1, "product": "cirroved", **self.policy}
            self.fixture = Path(tempfile.mkdtemp(prefix="storage-format-", dir=os.environ.get("TMPDIR", "/var/tmp")))
            self.fixture.chmod(0o700)
            self.child_records = []

        def executable(self, body):
            path = self.fixture / ("query-" + str(len(list(self.fixture.iterdir()))))
            path.write_text("#!/usr/bin/python3\n" + body)
            path.chmod(0o700)
            return path, self.guard.file_identity(path, 128 * 1024 * 1024, True)

        def query(self, path, pin):
            actual = self.guard.subprocess.Popen
            def observe(*args, **kwargs):
                child = actual(*args, **kwargs)
                self.child_records.append((child, args[0], kwargs["pass_fds"]))
                return child
            with patch.object(self.guard.subprocess, "Popen", side_effect=observe):
                return self.guard.package_storage_format(path, pin, self.policy)

        def assert_closed(self):
            for child, argv, descriptors in self.child_records:
                self.assertIsNotNone(child.poll(), "owned query child left alive")
                self.assertEqual(argv[1:], ["--storage-format-json"])
                self.assertEqual(argv[0], f"/proc/self/fd/{descriptors[0]}")
                for descriptor in descriptors:
                    with self.assertRaises(OSError):
                        os.fstat(descriptor)

        def test_storage_format_reply_requires_exact_policy_schema_pair(self):
            self.assertEqual(self.guard.storage_format_reply(json.dumps(self.value).encode(), self.policy), self.value)
            for field, replacement in [("journal_schema",14),("metadata_schema",7),
                    ("version",0),("version",True),("product","cirrove"),
                    ("journal_schema",True),("metadata_schema",True),
                    ("journal_schema",0),("metadata_schema",2**32),
                    ("journal_schema",{}),("metadata_schema","8")]:
                with self.subTest(field=field,replacement=replacement):
                    bad={**self.value,field:replacement}
                    with self.assertRaises(self.guard.Refusal):
                        self.guard.storage_format_reply(json.dumps(bad).encode(),self.policy)

        def test_storage_format_reply_rejects_malformed_duplicate_extra_and_oversize(self):
            raw=json.dumps(self.value).encode()
            cases=[b"",b"\xff",b"[]",raw+b"{}",b"x"*4097,
                b'{"version":1,"version":1,"product":"cirroved","journal_schema":21,"metadata_schema":8}',
                json.dumps({**self.value,"extra":True}).encode()]
            cases.extend(json.dumps({k:v for k,v in self.value.items() if k!=field}).encode() for field in self.value)
            for data in cases:
                with self.subTest(size=len(data)):
                    with self.assertRaises(self.guard.Refusal):
                        self.guard.storage_format_reply(data,self.policy)

        def test_storage_format_query_executes_exact_held_inode_with_clean_environment(self):
            body="import json,os,sys\nassert sys.argv[1:]==['--storage-format-json']\nassert not any(k in os.environ for k in ['HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR','LD_PRELOAD','LD_LIBRARY_PATH'])\nprint("+repr(json.dumps(self.value))+")\n"
            path,pin=self.executable(body)
            self.assertEqual(self.query(path,pin),self.value)
            self.assert_closed()

        def test_storage_format_query_refuses_old_timeout_and_oversize_then_reaps_owned_child(self):
            for body,reason in [("raise SystemExit(2)\n","unsupported"),
                    ("import time\ntime.sleep(30)\n","timed out"),
                    ("print('x'*4097)\n","exceeds bound")]:
                with self.subTest(reason=reason):
                    path,pin=self.executable(body)
                    with patch.object(self.guard,"STORAGE_FORMAT_QUERY_SECONDS",0.2):
                        with self.assertRaisesRegex(self.guard.Refusal,reason):
                            self.query(path,pin)
                    self.assert_closed()

        def test_storage_format_query_refuses_path_substitution_even_when_original_fd_reply_is_valid(self):
            path,pin=self.executable("print("+repr(json.dumps(self.value))+")\n")
            replacement,_=self.executable("raise SystemExit(2)\n")
            actual=os.open
            swapped=False
            def swapped_open(candidate,*args,**kwargs):
                nonlocal swapped
                descriptor=actual(candidate,*args,**kwargs)
                if Path(candidate)==path and not swapped:
                    swapped=True
                    os.replace(replacement,path)
                return descriptor
            with patch.object(self.guard.os,"open",side_effect=swapped_open):
                with self.assertRaisesRegex(self.guard.Refusal,"changed (before|during) storage-format query"):
                    self.query(path,pin)
            self.assertTrue(swapped)
            self.assert_closed()

        def test_storage_format_query_invalid_reply_closes_descriptor_without_logging_output(self):
            path,pin=self.executable("print('invalid synthetic declaration')\n")
            with self.assertRaisesRegex(self.guard.Refusal,"declaration is invalid") as error:
                self.query(path,pin)
            self.assertNotIn("invalid synthetic declaration",str(error.exception))
            self.assert_closed()


if __name__ == "__main__":
    unittest.main()
