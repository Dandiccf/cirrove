#!/usr/bin/env python3
"""Talk to a QEMU monitor over its QMP socket: one command, one answer.

    qmp.py SOCK screendump OUT.png     a PNG of the guest's screen
    qmp.py SOCK system_powerdown       press the power button
    qmp.py SOCK keys ctrl-alt-t        send a key combination
    qmp.py SOCK click X Y              move the pointer to X,Y (0..0x7fff) and click

Enough for the real-desktop checks: look, press, click, turn off.
"""
import json
import socket
import sys


def talk(path, commands):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(30)
        s.connect(path)
        reader = s.makefile("r")
        json.loads(reader.readline())  # the greeting
        s.sendall(b'{"execute":"qmp_capabilities"}\n')
        json.loads(reader.readline())
        replies = []
        for command in commands:
            s.sendall((json.dumps(command) + "\n").encode())
            while True:
                reply = json.loads(reader.readline())
                if "event" in reply:
                    continue
                replies.append(reply)
                break
        return replies


def main():
    path, verb, *args = sys.argv[1:]
    if verb == "screendump":
        commands = [{"execute": "screendump", "arguments": {"filename": args[0], "format": "png"}}]
    elif verb == "system_powerdown":
        commands = [{"execute": "system_powerdown"}]
    elif verb == "keys":
        keys = [{"type": "qcode", "data": k} for k in args[0].split("-")]
        commands = [{"execute": "send-key", "arguments": {"keys": keys}}]
    elif verb == "click":
        x, y = int(args[0]), int(args[1])
        move = [
            {"type": "abs", "data": {"axis": "x", "value": x}},
            {"type": "abs", "data": {"axis": "y", "value": y}},
        ]
        press = [{"type": "btn", "data": {"down": True, "button": "left"}}]
        release = [{"type": "btn", "data": {"down": False, "button": "left"}}]
        commands = [
            {"execute": "input-send-event", "arguments": {"events": move}},
            {"execute": "input-send-event", "arguments": {"events": press}},
            {"execute": "input-send-event", "arguments": {"events": release}},
        ]
    else:
        print(__doc__, file=sys.stderr)
        return 2
    for reply in talk(path, commands):
        if "error" in reply:
            print(json.dumps(reply["error"]), file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
