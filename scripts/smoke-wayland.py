#!/usr/bin/env python3
"""Exercise kadr's native Wayland lifecycle in an isolated nested compositor.

No Cargo build is run. Requires Xvfb, Openbox, Weston (X11 backend), xdotool,
and optionally ImageMagick's import. Only Python's standard library is used.
"""

import argparse
import contextlib
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


class SmokeError(Exception):
    pass


class Processes:
    """Own separate process groups, including children of exited launchers."""

    def __init__(self):
        self.children = []
        self.logs = []

    def start(self, argv, env, log_path, **kwargs):
        log = log_path.open("wb")
        self.logs.append(log)
        process = subprocess.Popen(
            argv,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            **kwargs,
        )
        self.children.append(process)
        return process

    @staticmethod
    def signal_group(process, sig):
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, sig)

    def close(self):
        # Signal every group even when its original leader has exited.
        for process in reversed(self.children):
            self.signal_group(process, signal.SIGTERM)
        deadline = time.monotonic() + 3
        for process in reversed(self.children):
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=max(0.01, deadline - time.monotonic()))
        for process in reversed(self.children):
            self.signal_group(process, signal.SIGKILL)
        for process in reversed(self.children):
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                print(f"WARN: process {process.pid} did not reap after SIGKILL", file=sys.stderr)
        for log in self.logs:
            log.close()
        self.children.clear()
        self.logs.clear()


def read_log(path):
    return path.read_text(encoding="utf-8", errors="replace") if path.exists() else ""


def wait_for(description, predicate, processes, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for name, process in processes:
            if process.poll() is not None:
                raise SmokeError(f"{name} exited early with code {process.returncode}")
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise SmokeError(f"Timed out waiting for {description} ({timeout:g}s)")


def command(argv, env, timeout=10, check=True):
    return subprocess.run(
        argv,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout,
        check=check,
    )


def read_display(fd, process, timeout):
    """Xvfb -displayfd allocates a free display without a check/start race."""
    deadline = time.monotonic() + timeout
    result = b""
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise SmokeError(f"Xvfb exited with code {process.returncode}")
        ready, _, _ = select.select([fd], [], [], min(0.1, max(0, deadline - time.monotonic())))
        if ready:
            chunk = os.read(fd, 64)
            if not chunk:
                raise SmokeError("Xvfb closed displayfd without a display number")
            result += chunk
            if b"\n" in result:
                number = result.decode("ascii").strip()
                if not number.isdecimal():
                    raise SmokeError("Xvfb returned an invalid display number")
                return f":{number}"
    raise SmokeError("Timed out waiting for Xvfb display allocation")


PROTOCOL = {
    "configure": re.compile(r"xdg_(?:surface|toplevel)[@#]\d+\.configure\("),
    "buffer attach": re.compile(r"(wl_surface[@#]\d+)\.attach\([^)\n]*wl_buffer[@#]\d+"),
}
FAILURE = re.compile(
    r"panicked at|fatal runtime error|protocol error|already borrowed|SIGSEGV",
    re.IGNORECASE,
)


def protocol_ready(log_path):
    text = read_log(log_path)
    if FAILURE.search(text):
        raise SmokeError("Application log contains a panic/runtime/protocol failure")
    configure = PROTOCOL["configure"].search(text)
    if configure is None:
        return False
    attach = PROTOCOL["buffer attach"].search(text, configure.end())
    if attach is None:
        return False
    # An initial empty commit before configure is not proof that the app
    # committed the rendered buffer. Require the same surface after attachment.
    return re.search(re.escape(attach.group(1)) + r"\.commit\(", text[attach.end():]) is not None


def weston_window(log_path, env):
    match = re.search(r"window id (0x[0-9a-fA-F]+|\d+)", read_log(log_path))
    if match:
        return match.group(1)
    result = command(
        ["xdotool", "search", "--onlyvisible", "--name", "Weston"],
        env,
        timeout=2,
        check=False,
    )
    return result.stdout.splitlines()[0] if result.returncode == 0 and result.stdout.strip() else None


def positive_seconds(value):
    number = float(value)
    if not 0 < number <= 300:
        raise argparse.ArgumentTypeError("timeout must be > 0 and <= 300 seconds")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("target/debug/kadr-app"),
        help="existing kadr-app executable; no build is performed",
    )
    parser.add_argument(
        "--artifacts",
        type=Path,
        help="new directory for logs and optional screenshot (default: new temp directory)",
    )
    parser.add_argument(
        "--timeout", type=positive_seconds, default=30, help="per-startup-wait deadline in seconds"
    )
    parser.add_argument(
        "--screenshot",
        action="store_true",
        help="capture the nested desktop for human inspection; not a pixel assertion",
    )
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"not an executable file: {binary}")
    tools = ["Xvfb", "openbox", "weston", "xdotool"]
    if args.screenshot:
        tools.append("import")
    missing = [tool for tool in tools if shutil.which(tool) is None]
    if missing:
        parser.error("missing tools: " + ", ".join(missing))
    if args.artifacts:
        artifacts = args.artifacts.resolve()
        try:
            artifacts.mkdir(mode=0o700, parents=True, exist_ok=False)
        except OSError as error:
            parser.error(f"cannot create new artifacts directory: {error}")
    else:
        artifacts = Path(tempfile.mkdtemp(prefix="kadr-wayland-smoke-", dir="/tmp"))
    print(f"Artifacts: {artifacts}", flush=True)
    processes = Processes()
    try:
        # Never read the operator's player preferences or change their database.
        # A short runtime path avoids the Unix socket pathname length limit,
        # regardless of how deeply nested the artifacts directory is.
        with (
            tempfile.TemporaryDirectory(prefix="kadr-wl-", dir="/tmp") as temporary,
            contextlib.closing(processes),
        ):
            sandbox = Path(temporary)
            env = os.environ.copy()
            for key in (
                "DISPLAY",
                "WAYLAND_DISPLAY",
                "WAYLAND_SOCKET",
                "WAYLAND_DEBUG",
                "XAUTHORITY",
                "DBUS_SESSION_BUS_ADDRESS",
                "DBUS_STARTER_ADDRESS",
                "DBUS_STARTER_BUS_TYPE",
                "AT_SPI_BUS_ADDRESS",
                "ZED_HEADLESS",
            ):
                env.pop(key, None)
            for variable, directory in (
                ("HOME", "home"),
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_RUNTIME_DIR", "runtime"),
            ):
                path = sandbox / directory
                path.mkdir(mode=0o700)
                env[variable] = str(path)

            display_read, display_write = os.pipe()
            try:
                xvfb = processes.start(
                    [
                        "Xvfb",
                        "-displayfd",
                        str(display_write),
                        "-screen",
                        "0",
                        "1280x900x24",
                        "-nolisten",
                        "tcp",
                        "-noreset",
                    ],
                    env,
                    artifacts / "xvfb.log",
                    pass_fds=(display_write,),
                )
                os.close(display_write)
                display_write = None
                env["DISPLAY"] = read_display(display_read, xvfb, args.timeout)
            finally:
                os.close(display_read)
                if display_write is not None:
                    os.close(display_write)

            wm = processes.start(["openbox"], env, artifacts / "openbox.log")
            weston_log = artifacts / "weston.log"
            socket = "kadr-smoke-wayland"
            weston = processes.start(
                [
                    "weston",
                    "--backend=x11-backend.so",
                    f"--socket={socket}",
                    "--width=1280",
                    "--height=900",
                    "--idle-time=0",
                    "--no-config",
                ],
                env,
                weston_log,
            )
            infrastructure = [("Xvfb", xvfb), ("Openbox", wm), ("Weston", weston)]
            wait_for(
                "Wayland socket",
                lambda: (sandbox / "runtime" / socket).is_socket(),
                infrastructure,
                args.timeout,
            )
            outer_window = wait_for(
                "Weston outer X11 window",
                lambda: weston_window(weston_log, env),
                infrastructure,
                args.timeout,
            )
            app_env = env.copy()
            app_env["WAYLAND_DISPLAY"] = socket
            app_env["WAYLAND_DEBUG"] = "1"
            # No DISPLAY means an accidental X11 fallback cannot pass this test.
            app_env.pop("DISPLAY", None)
            app_log = artifacts / "app-wayland.log"
            app = processes.start([str(binary)], app_env, app_log)
            wait_for(
                "native Wayland configure, buffer attach and surface commit",
                lambda: protocol_ready(app_log),
                infrastructure + [("kadr-app", app)],
                args.timeout,
            )
            print("PASS: native Wayland configure, non-null buffer attach and commit", flush=True)

            command(["xdotool", "windowactivate", "--sync", outer_window], env)
            command(
                ["xdotool", "mousemove", "--window", outer_window, "640", "450", "click", "1"],
                env,
            )
            if args.screenshot:
                # Allow the compositor to consume the committed buffer. The
                # screenshot is evidence for a person, not an automated UI test.
                time.sleep(0.25)
                screenshot = artifacts / "native-wayland.png"
                command(["import", "-window", "root", "-depth", "8", str(screenshot)], env)
                print(f"Screenshot (inspect manually): {screenshot}", flush=True)
            command(["xdotool", "key", "--clearmodifiers", "ctrl+q"], env)
            try:
                code = app.wait(timeout=args.timeout)
            except subprocess.TimeoutExpired as error:
                raise SmokeError("kadr-app did not exit after Ctrl+Q") from error
            if code != 0:
                raise SmokeError(f"kadr-app exited with code {code}, expected 0")
            if not protocol_ready(app_log):
                raise SmokeError("Required native protocol evidence missing from final log")
            print("PASS: Ctrl+Q exited kadr-app with code 0; no panic/protocol failure", flush=True)
            # Stop and reap while the private runtime directories still exist.
            processes.close()
        return 0
    except (SmokeError, OSError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}\nInspect logs in {artifacts}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print(f"Interrupted; inspect logs in {artifacts}", file=sys.stderr)
        return 130
    finally:
        processes.close()


def interrupt(signum, frame):
    # A terminal/service SIGTERM should also unwind the cleanup stack.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    sys.exit(main())
