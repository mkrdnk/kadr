#!/usr/bin/env python3
"""Probe owned local-fixture VLC IPC; never change platform security settings."""
import contextlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import wave


def probe(label, executable, root, index, media):
    endpoint = root / f"ipc{index}"
    log = root / f"log{index}"
    args = [
        executable, "--ignore-config",
        "--no-media-library", "--no-metadata-network-access", "--extraintf=oldrc",
        "--rc-fake-tty", "--verbose=2", "--play-and-exit", f"--rc-unix={endpoint}",
        "--start-time=2", "--intf=dummy", "--vout=dummy", "--aout=dummy",
        "--", str(media),
    ]
    # VLC 3's instance/playlist-reuse switches require Win32, DBus or OS/2;
    # the official macOS build has none of those option registrations.
    if sys.platform != "darwin":
        args[2:2] = ["--no-one-instance", "--no-playlist-enqueue"]
    started = time.monotonic()
    connection = None
    with log.open("wb") as output:
        process = subprocess.Popen(
            args, stdin=subprocess.DEVNULL, stdout=output, stderr=output,
        )
    try:
        while time.monotonic() - started < 5 and process.poll() is None:
            candidate = socket.socket(socket.AF_UNIX)
            candidate.settimeout(0.2)
            try:
                candidate.connect(str(endpoint))
                connection = candidate
                break
            except OSError:
                candidate.close()
                time.sleep(0.02)
        print(
            label, "is_symlink=", os.path.islink(executable),
            "socket_path_bytes=", len(os.fsencode(str(endpoint))),
            "ready=", connection is not None,
            "elapsed=", round(time.monotonic() - started, 3),
            "exit_before_stop=", process.poll(),
            "socket_exists=", endpoint.exists(), flush=True,
        )
        if connection:
            with contextlib.suppress(OSError):
                connection.sendall(b"quit\n")
        elif process.poll() is None and shutil.which("sample"):
            # Inspect only our child; never request privileges or sample users' apps.
            sample = root / f"sample{index}"
            with contextlib.suppress(subprocess.SubprocessError):
                subprocess.run(
                    ["sample", str(process.pid), "1", "1", "-file", str(sample)],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3,
                )
            if sample.exists():
                for line in sample.read_text(errors="replace").splitlines():
                    if re.search(r"Gatekeeper|Breakpad|NSBundle|libvlc_|CFRunLoop|NSAlert|dyld", line):
                        print("STACK", line.replace(str(root), "<diag>"))
    finally:
        if connection:
            connection.close()
        if connection is None and process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)
    for line in log.read_text(errors="replace").splitlines():
        if re.search(r"interface|oldrc|plugins|error|warning|VLC media player|socket|macosx|unknown option", line, re.I):
            line = re.sub(r"(?:https?|file)://\S+", "<fixture-uri>", line)
            print("LOG", line.replace(str(root), "<diag>"))
    print(label, "final_exit=", process.returncode, flush=True)


def main():
    os.umask(0o077)
    bundle = Path("/Applications/VLC.app")
    if bundle.exists():
        with (bundle / "Contents/Info.plist").open("rb") as file:
            print("VLC bundle version:", plistlib.load(file).get("CFBundleShortVersionString"))
        print("oldrc plugin files:", [path.name for path in bundle.rglob("*oldrc*")])
        quarantine = subprocess.run(
            ["xattr", "-p", "com.apple.quarantine", str(bundle)],
            capture_output=True, timeout=3,
        )
        print("bundle has quarantine attribute:", quarantine.returncode == 0)
    with tempfile.TemporaryDirectory(prefix="kadr-vlc-diag-", dir="/tmp") as temporary:
        root = Path(temporary)
        media = root / "own.wav"
        with wave.open(str(media), "wb") as output:
            output.setnchannels(1)
            output.setsampwidth(2)
            output.setframerate(8000)
            output.writeframes(b"\0" * (60 * 8000 * 2))
        variants = [
            ("canonical bundle", str(bundle / "Contents/MacOS/VLC")),
            ("Homebrew wrapper", "/opt/homebrew/bin/vlc"),
            ("PATH executable", shutil.which("vlc")),
        ]
        if bundle.exists():
            alias = root / "vlc"
            alias.symlink_to(bundle / "Contents/MacOS/VLC")
            variants.append(("temporary lowercase symlink", str(alias)))
        seen = set()
        for index, (label, executable) in enumerate(variants):
            if executable and os.path.isfile(executable) and executable not in seen:
                seen.add(executable)
                probe(label, executable, root, index, media)


if __name__ == "__main__":
    main()
