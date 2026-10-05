#!/usr/bin/python3
"""Drive both sides on a private display using a synthetic Cirrove socket.

Run with Strata's E2E venv Python. Requires its built binary and Xvfb on PATH.
No real Cirrove service, mount, account, default application or keyring is used.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import socketserver
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]

class FixtureDaemon(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True
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
                mount = {"event":"mount","account_id":"fixture","label":"Validation",
                         "mount_path":server.mount,"mounted":server.mounted}
                self.wfile.write((json.dumps(server.event())+'\n'+json.dumps(mount)+'\n{"event":"ready"}\n').encode())
                self.wfile.flush()
            self.rfile.read(1)  # EOF when the provider exits.
            return
        if verb == "capabilities":
            reply = {"capabilities":{"paths-cached":1,"pin-identity":1,"events":1}}
        elif verb == "status":
            reply = {"protocol_version":1,"accounts":[{"account_id":"fixture","label":"Validation","mount_path":server.mount,"mounted":server.mounted}]}
        elif verb == "paths-cached":
            reply = {"states":[dict(server.rows.get(p,{"refusal":"not indexed"}),path=p) for p in body["paths"]]}
        elif verb in ("pin","unpin"):
            state = server.rows[body["path"]]
            if body.get("expected") != state["identity"]:
                reply = {"accepted":False,"refusal":"selected identity changed"}
            else:
                state["pinned"] = "direct" if verb == "pin" else None
                reply = {"accepted":True,"job":"synthetic-only"}
                server.invalidate()
        else:
            reply = {"refusal":"unexpected verb"}
        self.wfile.write(json.dumps(reply).encode())


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strata-checkout",type=Path,required=True)
    parser.add_argument("--strata-binary",type=Path,help="compiled Strata binary; defaults to checkout/target/debug/strata")
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    binary=(args.strata_binary or args.strata_checkout/"target/debug/strata").resolve()
    if not binary.is_file() or not os.access(binary,os.X_OK):
        parser.error("build the companion Strata branch and pass its executable with --strata-binary")
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
        "Local only.txt":"synthetic", "Project":{},
    })
    daemon=FixtureDaemon(args.output/"control.sock",fixture.root)
    for name in ("Kept offline.txt","Fetching.txt","On demand.txt","Inherited.txt","Project",""):
        daemon.rows[name]={"item":name or "root","kind":"folder" if name in ("Project","") else "file","can_pin":True,"size":100,"resident":100,"pinned":None}
        daemon.rows[name]["identity"]={"item":name or "root","mount":"fixture-mount",
                                       "scope":{"account":"fixture","provider":"fixture","collection":"drive"}}
    daemon.rows["Kept offline.txt"]["pinned"]="direct"
    daemon.rows["Fetching.txt"].update(pinned="direct",resident=30)
    daemon.rows["Inherited.txt"]["pinned"]="inherited"
    daemon.rows["Project"]["pinned"]="direct"
    threading.Thread(target=daemon.serve_forever,daemon=True).start()
    display=HeadlessDisplay();environment=TestEnvironment();app=None;connection=None
    try:
        environment.write_preferences({"browser_mode":"icons"})
        subprocess.run([str(ROOT/"scripts/install-developer.sh"),"--strata-only","--config-home",str(environment.config_home),"--socket",str(args.output/"control.sock")],check=True)
        display.start();os.environ.update(display.environment);tree.connect()
        connection=XTestConnection(display.display)
        app=Application(display,environment,fixture.root)
        window=Strata(app,Keyboard(connection),Pointer(connection),fixture,environment,display)
        app.start()
        window.wait(lambda: window.window.find(role="image",description="cirrove: Kept offline"),"Cirrove kept badge")
        window.wait(lambda: window.window.find(role="image",description="cirrove: Kept offline · fetching 30%"),"Cirrove fetching badge")
        window.screenshot(args.output/"01-badges.png")
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Keep offline · cirrove" in window.menu_items(),"Cirrove pin menu")
        window.screenshot(args.output/"02-keep-menu.png")
        window.choose_menu_item("Keep offline · cirrove")
        window.wait(lambda:any(v=="pin" for v,_ in daemon.calls),"pin reached daemon")
        window.keyboard.press("Escape")
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Stop keeping offline · cirrove" in window.menu_items(),"pin state reflected in menu")
        window.screenshot(args.output/"03-pinned-menu.png")
        window.choose_menu_item("Show availability · cirrove")
        window.wait(lambda:any("cirrove: On demand.txt: Kept offline" in " ".join(node.name.split()) for node in window.window.find_all(role="label")),"availability dialog")
        window.screenshot(args.output/"04-availability.png")
        window.keyboard.press("Escape")
        window.open_context_menu("On demand.txt")
        window.wait(lambda:"Stop keeping offline · cirrove" in window.menu_items(),"Cirrove unpin menu")
        window.choose_menu_item("Stop keeping offline · cirrove")
        window.wait(lambda:any(v=="unpin" for v,_ in daemon.calls),"unpin reached daemon")
        window.wait(lambda:not window.entry("On demand.txt").find(role="image",description="cirrove: Kept offline"),"unpinned item badge withdrawn")
        window.keyboard.press("Escape")
        daemon.mounted=False;daemon.invalidate()
        window.wait(lambda:not window.window.find(role="image",description="cirrove: Kept offline") and not window.window.find(role="image",description="cirrove: Kept offline · fetching 30%"),"badges withdrawn on unmount")
        window.open_context_menu("Kept offline.txt")
        time.sleep(.5)
        assert "Stop keeping offline · cirrove" not in window.menu_items()
        assert "Show availability · cirrove" not in window.menu_items()
        window.screenshot(args.output/"05-unmounted.png")
        assert all(v in ("subscribe","capabilities","status","paths-cached","pin","unpin") for v,_ in daemon.calls)
        assert all(body["expected"]==daemon.rows[body["path"]]["identity"] for verb,body in daemon.calls if verb in ("pin","unpin"))
        (args.output/"result.json").write_text(json.dumps({"synthetic":True,"passed":True,"badge_states":["kept","fetching","on-demand-unbadged"],"pin_activation":True,"unpin_activation":True,"guarded_identity":True,"availability":True,"unmount_withdrawal":True,"calls":{verb:sum(v==verb for v,_ in daemon.calls) for verb in set(v for v,_ in daemon.calls)}},indent=2)+"\n")
    finally:
        if app:
            (args.output/"strata.log").write_text(app.log())
            app.stop()
        if connection: connection.close()
        display.stop()
        daemon.shutdown();daemon.server_close()
        # Keep the isolated HOME/configuration for review alongside its location.
        (args.output/"environment.json").write_text(json.dumps({"home_fixture":str(environment.root)},indent=2)+"\n")
    print(args.output/"result.json")

if __name__=="__main__": main()
