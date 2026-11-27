#!/usr/bin/env python3
"""Owned mpv/VLC IPC test double, launched via a per-test symlink."""
import json
import os
from pathlib import Path
import socket
import sys
import time

directory = Path(sys.argv[0]).parent
mode = (directory / "mode").read_text()
if mode == "delayed_start":
    time.sleep(1.1)
args = sys.argv[1:]
path = next(a.split("=", 1)[1] for a in args if a.startswith(("--input-ipc-server=", "--rc-unix=")))
with (directory / "record.json").open("w") as f:
    json.dump(dict(pid=os.getpid(), args=args, socket=path), f)
if mode == "early_exit":
    sys.exit(9)
if mode == "chatty":
    os.write(1, b"x" * 1000000)
    os.write(2, b"x" * 1000000)
if mode == "no_connect":
    time.sleep(30)
    sys.exit()
s = socket.socket(socket.AF_UNIX)
s.bind(path)
s.listen(1)
c, _ = s.accept()
if mode == "exit":
    time.sleep(0.1)
    sys.exit(7)
if mode == "stalled":
    time.sleep(30)
    sys.exit()
if mode.startswith("queued_"):
    time.sleep(0.05)
    if mode == "queued_partial":
        c.sendall(b'{"event":"end-file","reason":"eof"')
    elif mode != "queued_exit0":
        reason = dict(queued_eof="eof", queued_quit="quit", queued_error="error", queued_nonzero="error")[mode]
        c.sendall(json.dumps(dict(event="end-file", reason=reason)).encode() + b"\n")
    sys.exit(7 if mode == "queued_nonzero" else 0)
for line in c.makefile("rb"):
    if line == b"quit\n":
        sys.exit()
    request = json.loads(line)
    if request["command"][0] == "quit":
        if mode == "ignore_quit":
            time.sleep(30)
        sys.exit()
    if mode == "malformed":
        c.sendall(b"not JSON\n")
        continue
    if mode == "oversized":
        c.sendall(b"x" * 70000 + b"\n")
        continue
    if mode == "eof":
        c.shutdown(socket.SHUT_RDWR)
        c.close()
        time.sleep(30)
        continue
    if mode == "exit_on_query":
        sys.exit(7)
    if mode == "eof_on_query":
        c.sendall(b'{"event":"end-file","reason":"eof"}\n')
        sys.exit()
    if mode == "partial":
        c.sendall(b'{"request_id":')
        time.sleep(30)
        continue
    ident = request["request_id"]
    if mode == "flood":
        while True:
            c.sendall(b'{"event":"tick"}\n')
    # Events and wrong IDs must not be mistaken for position/duration.
    c.sendall(b'{"event":"file-loaded"}\n')
    if mode in ("eof_event", "quit_event", "error_event", "unrelated_eof"):
        reason = dict(eof_event="eof", quit_event="quit", error_event="error", unrelated_eof="eof")[mode]
        event = dict(event="playback-restart" if mode == "unrelated_eof" else "end-file",
                     reason=reason, request_id=ident+100)
        c.sendall(json.dumps(event).encode() + b"\n")
    c.sendall(json.dumps(dict(request_id=ident+100, error="success", data=999)).encode() + b"\n")
    prop = request["command"][1]
    reply = dict(request_id=ident, error="success", data=12.5 if prop == "time-pos" else 60)
    if mode == "near_end":
        reply["data"] = 60
    if mode == "unavailable":
        reply = dict(request_id=ident, error="property unavailable")
    if mode == "negative":
        reply["data"] = -1
    c.sendall(json.dumps(reply).encode() + b"\n")
