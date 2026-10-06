#!/usr/bin/python3
"""Drive both sides on a private display using a synthetic Cirrove socket.

Run with Strata's E2E venv Python. Requires its built binary and Xvfb on PATH.
No real Cirrove service, mount, account, default application or keyring is used.
"""
import argparse
import importlib.util
import json
import hashlib
import stat
import os
from pathlib import Path
import socketserver
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]

class FixtureDaemon(socketserver.ThreadingUnixStreamServer):
    daemon_threads = False  # server_close joins handlers before the final RPC trace.
    def __init__(self, socket_path, mount):
        self.mount = str(mount)
        self.rows = {}
        self.calls = []
        self.listeners = []
        self.lock = threading.Lock()
        self.mounted = True
        self.generation = 0
        super().__init__(str(socket_path), Handler)
    def event(self):
        return {"event":"account","account_id":"fixture","label":"Validation","state":"connected","mounted":self.mounted,"enabled":True,"kept_generation":self.generation}
    def invalidate(self):
        with self.lock:
            self.generation += 1
            for listener in list(self.listeners):
                try:
                    listener.sendall((json.dumps(self.event())+"\n").encode())
                except OSError:
                    self.listeners.remove(listener)

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        raw = self.rfile.readline(65536)
        verb, _, body = raw.decode().strip().partition(" ")
        body = json.loads(body) if body else None
        server = self.server
        server.calls.append((verb, body))
        if verb == "subscribe":
            with server.lock:
                server.listeners.append(self.request)
                self.wfile.write((json.dumps(server.event())+'\n{"event":"ready"}\n').encode())
                self.wfile.flush()
            self.rfile.read(1)  # EOF when the provider exits.
            return
        if verb == "capabilities":
            reply = {"capabilities":{"paths-cached":1,"events":1}}
        elif verb == "status":
            reply = {"protocol_version":1,"accounts":[{"account_id":"fixture","label":"Validation","mount_path":server.mount,"mounted":server.mounted}]}
        elif verb == "paths-cached":
            reply = {"states":[dict(server.rows.get(p,{"refusal":"not indexed"}),path=p) for p in body["paths"]]}
        elif verb in ("pin","unpin"):
            state = server.rows[body["path"]]
            state["pinned"] = "direct" if verb == "pin" else None
            reply = {"accepted":True,"job":"synthetic-only"}
            server.invalidate()
        else:
            reply = {"refusal":"unexpected verb"}
        self.wfile.write(json.dumps(reply).encode())



def binary_pin(path):
    """Hash an exact regular ELF; never load it or build the companion."""
    path = path.absolute()
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(descriptor)
        if (not stat.S_ISREG(before.st_mode) or before.st_uid not in (0, os.getuid())
                or before.st_mode & 0o022 or not before.st_mode & 0o111):
            raise ValueError("Strata binary must be a trusted regular executable ELF")
        if os.read(descriptor, 4) != b"\x7fELF":
            raise ValueError("Strata binary must be an ELF")
        os.lseek(descriptor, 0, os.SEEK_SET)
        digest = hashlib.sha256()
        while block := os.read(descriptor, 1024 * 1024):
            digest.update(block)
        fields = ("st_dev", "st_ino", "st_size", "st_mode", "st_uid", "st_nlink", "st_mtime_ns", "st_ctime_ns")
        identity = tuple(getattr(before, field) for field in fields)
        if (identity != tuple(getattr(os.fstat(descriptor), field) for field in fields)
                or identity != tuple(getattr(path.lstat(), field) for field in fields)):
            raise ValueError("Strata binary changed during verification")
        return {"path": str(path), "size": before.st_size, "sha256": digest.hexdigest(), "stamp": list(identity)}
    finally:
        os.close(descriptor)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strata-checkout",type=Path,required=True)
    parser.add_argument("--strata-binary",type=Path,help="exact prebuilt companion ELF; default CHECKOUT/target/debug/strata")
    parser.add_argument("--strata-binary-sha256",help="required digest when --strata-binary is supplied")
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    if bool(args.strata_binary) != bool(args.strata_binary_sha256):
        parser.error("--strata-binary and --strata-binary-sha256 must be supplied together")
    if args.strata_binary_sha256 is not None and (len(args.strata_binary_sha256) != 64
            or any(c not in "0123456789abcdef" for c in args.strata_binary_sha256)):
        parser.error("--strata-binary-sha256 must be a lowercase SHA256")
    binary = (args.strata_binary or args.strata_checkout/"target/debug/strata").absolute()
    try:
        initial_binary_pin = binary_pin(binary)
        if args.strata_binary_sha256 is not None and initial_binary_pin["sha256"] != args.strata_binary_sha256:
            raise ValueError("Strata binary digest differs from its registration")
    except (OSError, ValueError) as error:
        parser.error(str(error))
    args.output.mkdir(parents=True,exist_ok=False)
    sys.path.insert(0,str(args.strata_checkout/"tests/e2e"))
    from harness.application import Application
    from harness.browser import Strata
    from harness.display import HeadlessDisplay
    from harness.environment import TestEnvironment
    from harness.fixtures import FixtureTree
    from harness.interaction import Keyboard,Pointer
    from harness.xtest import XTestConnection
    from harness import tree
    os.environ["STRATA_BINARY"]=str(binary)
    fixture=FixtureTree.create_at(args.output/"Cloud Files", {
        "Kept offline.txt":"synthetic", "Fetching.txt":"synthetic",
        "On demand.txt":"synthetic", "Inherited.txt":"synthetic",
        "Local only.txt":"synthetic", "Project":{}, "Native.numbers":{},
    })
    daemon=FixtureDaemon(args.output/"control.sock",fixture.root)
    for name in ("Kept offline.txt","Fetching.txt","On demand.txt","Inherited.txt","Project","Native.numbers",""):
        daemon.rows[name]={"item":name or "root","kind":"folder" if name in ("Project","Native.numbers","") else "file","can_pin":True,"size":100,"resident":100,"pinned":None}
    daemon.rows["Kept offline.txt"]["pinned"]="direct"
    daemon.rows["Fetching.txt"].update(pinned="direct",resident=30)
    daemon.rows["Inherited.txt"]["pinned"]="inherited"
    daemon.rows["Project"]["pinned"]="direct"
    daemon.rows["Native.numbers"].update(can_pin=False,package=True)
    threading.Thread(target=daemon.serve_forever,daemon=True).start()
    display=HeadlessDisplay();environment=TestEnvironment();app=None;connection=None
    try:
        environment.write_preferences({"browser_mode":"icons"})
        subprocess.run([str(ROOT/"scripts/install-developer.sh"),"--strata-only","--config-home",str(environment.config_home),"--socket",str(args.output/"control.sock")],check=True)
        display.start();os.environ.update(display.environment);tree.connect()
        connection=XTestConnection(display.display)
        app=Application(display,environment,fixture.root)
        window=Strata(app,Keyboard(connection),Pointer(connection),fixture,environment,display)
        assert binary_pin(binary) == initial_binary_pin
        app.start()
        window.wait(lambda: window.window.find(role="image",description="cirrove: Kept offline"),"Cirrove kept badge")
        window.wait(lambda: window.window.find(role="image",description="cirrove: Kept offline · fetching 30%"),"Cirrove fetching badge")
        window.screenshot(args.output/"01-badges.png")
        assert not any(verb in ("pin", "unpin") for verb, _ in daemon.calls)
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Keep offline" in window.menu_items(),"Cirrove pin menu")
        window.screenshot(args.output/"02-keep-menu.png")
        window.choose_menu_item("Keep offline")
        window.wait(lambda:any(v=="pin" for v,_ in daemon.calls),"pin reached daemon")
        window.keyboard.press("Escape")
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Stop keeping offline" in window.menu_items(),"pin state reflected in menu")
        window.screenshot(args.output/"03-pinned-menu.png")
        window.choose_menu_item("Show availability")
        window.wait(lambda:any("cirrove: On demand.txt: Kept offline" in " ".join(node.name.split()) for node in window.window.find_all(role="label")),"availability dialog")
        window.screenshot(args.output/"04-availability.png")
        window.keyboard.press("Escape")
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Stop keeping offline" in window.menu_items(),"direct pin offers Stop")
        before_unpin = sum(verb == "unpin" for verb, _ in daemon.calls)
        window.choose_menu_item("Stop keeping offline")
        window.wait(lambda:sum(verb == "unpin" for verb, _ in daemon.calls) == before_unpin + 1,"one direct unpin reached daemon")
        assert [body for verb, body in daemon.calls if verb == "unpin"] == [{"label":"Validation", "path":"On demand.txt", "recursive":False}]
        window.keyboard.press("Escape")
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Keep offline" in window.menu_items(),"direct unpin event refresh")
        assert "Stop keeping offline" not in window.menu_items()
        window.dismiss_menu()
        window.open_context_menu("Inherited.txt")
        window.wait(lambda:"Keep offline" in window.menu_items(),"inherited pin offers Keep")
        assert "Stop keeping offline" not in window.menu_items()
        window.screenshot(args.output/"05-inherited-menu.png")
        before_pin = sum(verb == "pin" for verb, _ in daemon.calls)
        window.choose_menu_item("Keep offline")
        window.wait(lambda:sum(verb == "pin" for verb, _ in daemon.calls) == before_pin + 1,"one inherited direct pin reached daemon")
        assert [body for verb, body in daemon.calls if verb == "pin"] == [
            {"label":"Validation", "path":"On demand.txt", "recursive":False},
            {"label":"Validation", "path":"Inherited.txt", "recursive":False}]
        window.keyboard.press("Escape")
        window.open_context_menu("Inherited.txt")
        window.wait(lambda:"Stop keeping offline" in window.menu_items(),"inherited-to-direct event refresh")
        window.dismiss_menu()
        before_native = [(verb, body) for verb, body in daemon.calls if verb in ("pin", "unpin")]
        window.open_context_menu("Native.numbers")
        window.wait(lambda:"Show availability" in window.menu_items(),"native can_pin=false availability")
        assert not {"Keep offline", "Keep folder offline", "Keep folders offline", "Stop keeping offline"}.intersection(window.menu_items())
        window.screenshot(args.output/"06-native-availability-only.png")
        window.choose_menu_item("Show availability")
        window.wait(lambda:any("cirrove: Native.numbers: On demand" in " ".join(node.name.split()) for node in window.window.find_all(role="label")),"native availability dialog")
        assert [(verb, body) for verb, body in daemon.calls if verb in ("pin", "unpin")] == before_native
        window.keyboard.press("Escape")
        daemon.mounted=False;daemon.invalidate()
        window.wait(lambda:not window.window.find(role="image",description="cirrove: Kept offline"),"badges withdrawn on unmount")
        window.open_context_menu("Kept offline.txt")
        time.sleep(.5)
        assert "Stop keeping offline" not in window.menu_items()
        assert "Show availability" not in window.menu_items()
        window.screenshot(args.output/"07-unmounted.png")
        assert all(v in ("subscribe","capabilities","status","paths-cached","pin","unpin") for v,_ in daemon.calls)
        assert binary_pin(binary) == initial_binary_pin
    finally:
        if app:
            (args.output/"strata.log").write_text(app.log())
            app.stop()
        if connection: connection.close()
        display.stop()
        daemon.shutdown();daemon.server_close()
        # Keep the isolated HOME/configuration for review alongside its location.
        (args.output/"environment.json").write_text(json.dumps({"home_fixture":str(environment.root)},indent=2)+"\n")
    # The app/display are stopped and server_close has joined all request handlers.
    mutation_sequence = [(verb, body) for verb, body in daemon.calls if verb in ("pin", "unpin")]
    assert mutation_sequence == [
        ("pin", {"label":"Validation", "path":"On demand.txt", "recursive":False}),
        ("unpin", {"label":"Validation", "path":"On demand.txt", "recursive":False}),
        ("pin", {"label":"Validation", "path":"Inherited.txt", "recursive":False}),
    ]
    assert binary_pin(binary) == initial_binary_pin
    (args.output/"result.json").write_text(json.dumps({"synthetic":True,"passed":True,"badge_states":["kept","fetching","on-demand-unbadged"],"pin_activation":True,"direct_unpin_activation":True,"inherited_keep_activation":True,"native_availability_only":True,"event_menu_refresh":True,"mutation_sequence":mutation_sequence,"success_written_after_cleanup":True,"binary_pin":initial_binary_pin,"explicit_binary_digest_supplied":args.strata_binary_sha256 is not None,"production_service":False,"real_provider":False,"installed_acceptance":False,"availability":True,"unmount_withdrawal":True,"calls":{verb:sum(v==verb for v,_ in daemon.calls) for verb in set(v for v,_ in daemon.calls)}},indent=2)+"\n")
    print(args.output/"result.json")

if __name__=="__main__": main()
