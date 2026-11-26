#!/usr/bin/env python3
"""Exercise the real GPUI binary in an isolated native X11 server.

Requires Xvfb, Openbox, xdotool, wmctrl and a Vulkan software/hardware driver.
This is not an XWayland test and does not validate Wayland or macOS.
"""

import argparse
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import tempfile
import time


def run(*args, env):
    return subprocess.check_output(args, env=env, text=True, timeout=5).strip()


def wait_for(check, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(f"Timed out waiting for {description}")


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def exercise(binary, env, close, logs, screenshot):
    with open(logs / f"app-{close}.log", "w+") as log:
        app = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
        try:
            def find_window():
                if app.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(f"App exited early ({app.returncode}): {log.read()}")
                found = subprocess.run(
                    ["xdotool", "search", "--onlyvisible", "--pid", str(app.pid), "--name", "^kadr$"],
                    env=env, capture_output=True, text=True, timeout=5,
                )
                return found.stdout.splitlines()[0] if found.returncode == 0 else None

            window = wait_for(find_window, "visible kadr window")
            run("xdotool", "windowsize", window, "1000", "720", env=env)

            def resized():
                geometry = run("xdotool", "getwindowgeometry", "--shell", window, env=env)
                return "WIDTH=1000" in geometry.splitlines() and "HEIGHT=720" in geometry.splitlines()

            wait_for(resized, "window resize", timeout=10)
            if screenshot is not None:
                # Explicit opt-in, captures only this test window.
                time.sleep(1)
                run("import", "-window", window, str(screenshot), env=env)
            if close == "wm-delete":
                run("wmctrl", "-i", "-c", hex(int(window)), env=env)
            else:
                run("xdotool", "windowactivate", "--sync", window, env=env)
                run("xdotool", "key", "--clearmodifiers", "ctrl+q", env=env)
            code = app.wait(timeout=15)
            if code != 0:
                log.seek(0)
                raise RuntimeError(f"App failed after {close} ({code}): {log.read()}")
            print(f"PASS: native X11 window map, resize, {close}, exit 0 (pixels not asserted)")
        finally:
            stop(app)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--screenshot", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    for command in ("Xvfb", "openbox", "xdotool", "wmctrl"):
        if shutil.which(command) is None:
            parser.error(f"Missing executable: {command}")
    screenshot = args.screenshot.resolve() if args.screenshot else None
    env = os.environ.copy()
    for key in (
        "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "WAYLAND_DEBUG", "ZED_HEADLESS",
        "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS",
        "DBUS_STARTER_BUS_TYPE", "AT_SPI_BUS_ADDRESS",
    ):
        env.pop(key, None)
    with tempfile.TemporaryDirectory(prefix="kadr-x11-") as directory:
        logs = Path(directory)
        runtime = logs / "runtime"
        runtime.mkdir(mode=0o700)
        # Do not write to the user's real settings/history in a smoke check.
        env.update(
            HOME=str(logs), XDG_CONFIG_HOME=str(logs / "config"),
            XDG_DATA_HOME=str(logs / "data"), XDG_CACHE_HOME=str(logs / "cache"),
            XDG_STATE_HOME=str(logs / "state"),
            XDG_RUNTIME_DIR=str(runtime),
        )
        read_fd, write_fd = os.pipe()
        xvfb = wm = None
        try:
            with open(logs / "xvfb.log", "w") as xlog, open(logs / "wm.log", "w") as wlog:
                xvfb = subprocess.Popen(
                    ["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "1280x900x24", "-nolisten", "tcp"],
                    pass_fds=(write_fd,), stdout=xlog, stderr=xlog,
                )
                os.close(write_fd)
                write_fd = None
                if not select.select([read_fd], [], [], 10)[0]:
                    raise RuntimeError("Xvfb failed to allocate a display")
                display = os.read(read_fd, 64).decode().strip()
                if not display.isdecimal():
                    raise RuntimeError("Invalid Xvfb display number")
                env["DISPLAY"] = f":{display}"
                wm = subprocess.Popen(["openbox"], env=env, stdout=wlog, stderr=wlog)

                def wm_ready():
                    return subprocess.run(
                        ["wmctrl", "-m"], env=env, capture_output=True, timeout=5,
                    ).returncode == 0

                wait_for(wm_ready, "window manager", timeout=10)
                exercise(binary, env, "wm-delete", logs, screenshot)
                exercise(binary, env, "ctrl-q", logs, None)
        finally:
            stop(wm)
            stop(xvfb)
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)


def interrupt(signum, frame):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    try:
        main()
    except KeyboardInterrupt:
        raise SystemExit(130)
