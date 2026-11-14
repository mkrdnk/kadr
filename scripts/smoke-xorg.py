#!/usr/bin/env python3
"""Check real native X11 rendering and lifecycle on private Xorg + dummy.

Uses only Python's standard library; see docs/x11-smoke.md for system tools.
No build, Xvfb, XWayland, fake pixels, or existing desktop is used.
"""

import argparse
import contextlib
import math
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


CONFIG = """Section "ServerFlags"
 Option "AutoAddDevices" "off"
 Option "AllowMouseOpenFail" "true"
EndSection
Section "Device"
 Identifier "Dummy"
 Driver "dummy"
 VideoRam 256000
EndSection
Section "Monitor"
 Identifier "Monitor"
 HorizSync 28-80
 VertRefresh 48-75
 Modeline "1280x900" 109.0 1280 1368 1496 1712 900 903 909 934 -HSync +Vsync
EndSection
Section "Screen"
 Identifier "Screen"
 Device "Dummy"
 Monitor "Monitor"
 DefaultDepth 24
 SubSection "Display"
  Depth 24
  Modes "1280x900"
 EndSubSection
EndSection
"""


def kill_group(process, sig):
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, sig)


class Processes:
    def __init__(self):
        self.children = []
        self.logs = []

    def start(self, argv, env, log_path, **kwargs):
        log = log_path.open("wb")
        self.logs.append(log)
        process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True, **kwargs)
        self.children.append(process)
        return process

    def close(self):
        # Include descendants even if their original group leader exited.
        for process in reversed(self.children):
            kill_group(process, signal.SIGTERM)
        deadline = time.monotonic() + 3
        for process in reversed(self.children):
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=max(0.01, deadline - time.monotonic()))
        for process in reversed(self.children):
            kill_group(process, signal.SIGKILL)
            process.wait(timeout=3)
        for log in self.logs:
            log.close()
        self.children.clear()
        self.logs.clear()


def command(argv, env, check=True):
    process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=5)
        if check and process.returncode:
            raise RuntimeError(f"{argv!r} failed ({process.returncode}): {stderr}")
        return stdout.strip()
    finally:
        kill_group(process, signal.SIGKILL)
        process.wait(timeout=3)


def wait_for(description, predicate, processes, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for process in processes:
            if process.poll() is not None:
                raise RuntimeError(f"{process.args[0]} exited early ({process.returncode})")
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError(f"Timed out waiting for {description} ({timeout:g}s)")


def read_display(fd, process, timeout):
    result = bytearray()

    def allocated():
        if select.select([fd], [], [], 0)[0]:
            chunk = os.read(fd, 64)
            if not chunk:
                raise RuntimeError("Xorg closed displayfd without allocating a display")
            result.extend(chunk)
        if b"\n" in result:
            number = result.decode("ascii").strip()
            if not number.isdecimal():
                raise RuntimeError("Invalid Xorg display number")
            return f":{number}"
        return None

    return wait_for("Xorg displayfd", allocated, [process], timeout)


def exercise(binary, env, close, artifacts, pool, infrastructure, timeout):
    app = pool.start([str(binary)], env, artifacts / f"app-{close}.log")
    running = infrastructure + [app]

    def mapped():
        found = command(["xdotool", "search", "--onlyvisible", "--pid",
                         str(app.pid), "--name", "^kadr$"], env, check=False)
        return found.splitlines()[0] if found else None

    window = wait_for("mapped native X11 kadr client", mapped, running, timeout)
    command(["xdotool", "windowsize", window, "1000", "720"], env)

    def resized():
        geometry = command(["xdotool", "getwindowgeometry", "--shell", window], env)
        return {"WIDTH=1000", "HEIGHT=720"}.issubset(geometry.splitlines())

    wait_for("1000x720 client resize", resized, running, timeout)
    screenshot = artifacts / f"client-{close}.png"

    def rendered():
        # No root/frame capture: Openbox decorations cannot pass this assertion.
        command(["import", "-window", window, "-depth", "8", str(screenshot)], env)
        stats = command(["identify", "-format",
                         "%w %h %[fx:mean] %[fx:standard_deviation] %[fx:maxima] %k",
                         str(screenshot)], env).split()
        width, height, mean, deviation, maximum, colors = map(float, stats)
        (artifacts / f"pixels-{close}.txt").write_text(
            f"width height mean stddev max (normalized) colors: {' '.join(stats)}\n")
        # Also reject black clients with solid newly exposed resize strips.
        return (width == 1000 and height == 720
                and all(math.isfinite(v) for v in (mean, deviation, maximum))
                and mean > 0.001 and deviation > 0.005 and maximum > 0.05 and colors > 16)

    wait_for("nonblack, nonuniform decoded client pixels", rendered, running, timeout)
    print(f"PASS: native X11 map, resize, nonblack/nonuniform client: {screenshot}", flush=True)
    if close == "wm-delete":
        protocols = command(["xprop", "-id", window, "WM_PROTOCOLS"], env)
        if "WM_DELETE_WINDOW" not in protocols:
            raise RuntimeError("Client does not advertise WM_DELETE_WINDOW")
        # Ask Openbox to send WM_DELETE_WINDOW, not XKillClient/windowclose.
        command(["wmctrl", "-i", "-c", hex(int(window))], env)
    else:
        command(["xdotool", "windowactivate", "--sync", window], env)
        command(["xdotool", "key", "--clearmodifiers", "ctrl+q"], env)
    code = app.wait(timeout=timeout)
    if code != 0:
        raise RuntimeError(f"kadr-app exited with {code} after {close}, expected 0")
    print(f"PASS: {close} exited kadr-app with code 0", flush=True)


def positive_seconds(value):
    number = float(value)
    if not 0 < number <= 300:
        raise argparse.ArgumentTypeError("timeout must be > 0 and <= 300 seconds")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/kadr-app"),
                        help="existing executable; no Cargo build is performed")
    parser.add_argument("--artifacts", type=Path,
                        help="new directory for logs, pixel statistics and client PNGs")
    parser.add_argument("--timeout", type=positive_seconds, default=30,
                        help="per-wait deadline in seconds (default: 30)")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"not an executable file: {binary}")
    missing = [tool for tool in ("Xorg", "openbox", "xdotool", "wmctrl",
                                "xprop", "import", "identify") if not shutil.which(tool)]
    if missing:
        parser.error("missing tools: " + ", ".join(missing))
    artifacts = args.artifacts.resolve() if args.artifacts else Path(
        tempfile.mkdtemp(prefix="kadr-xorg-artifacts-", dir="/tmp"))
    if args.artifacts:
        try:
            artifacts.mkdir(mode=0o700, parents=True, exist_ok=False)
        except OSError as error:
            parser.error(f"cannot create new artifacts directory: {error}")
    print(f"Artifacts: {artifacts}", flush=True)
    pool = Processes()
    try:
        # Cleanup runs before removing the private HOME and short runtime path.
        with tempfile.TemporaryDirectory(prefix="kadr-xorg-", dir="/tmp") as temporary:
            with contextlib.closing(pool):
                sandbox = Path(temporary)
                env = os.environ.copy()
                for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "WAYLAND_DEBUG",
                            "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS",
                            "DBUS_STARTER_BUS_TYPE", "AT_SPI_BUS_ADDRESS", "ZED_HEADLESS"):
                    env.pop(key, None)
                for key, directory in (("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                                       ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"),
                                       ("XDG_STATE_HOME", "state"), ("XDG_RUNTIME_DIR", "runtime")):
                    path = sandbox / directory
                    path.mkdir(mode=0o700)
                    env[key] = str(path)
                config = sandbox / "xorg.conf"
                config.write_text(CONFIG)
                (artifacts / "xorg.conf").write_text(CONFIG)
                read_fd, write_fd = os.pipe()
                try:
                    xorg = pool.start(
                        ["Xorg", "-noreset", "-nolisten", "tcp", "-config", str(config),
                         "-logfile", str(artifacts / "xorg.log"), "-displayfd", str(write_fd)],
                        env, artifacts / "xorg-stderr.log", pass_fds=(write_fd,))
                    os.close(write_fd)
                    write_fd = None
                    env["DISPLAY"] = read_display(read_fd, xorg, args.timeout)
                finally:
                    os.close(read_fd)
                    if write_fd is not None:
                        os.close(write_fd)
                print(f"Private Xorg display: {env['DISPLAY']}", flush=True)
                wm = pool.start(["openbox"], env, artifacts / "openbox.log")
                infrastructure = [xorg, wm]
                wait_for("Openbox", lambda: "Name: Openbox" in command(
                    ["wmctrl", "-m"], env, check=False), infrastructure, args.timeout)
                for close in ("wm-delete", "ctrl-q"):
                    exercise(binary, env, close, artifacts, pool, infrastructure, args.timeout)
        return 0
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}\nInspect logs and client PNGs in {artifacts}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print(f"Interrupted; inspect {artifacts}", file=sys.stderr)
        return 130
    finally:
        pool.close()


def interrupt(signum, frame):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    sys.exit(main())
