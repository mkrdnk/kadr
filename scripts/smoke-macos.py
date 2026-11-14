#!/usr/bin/env python3
"""Check a real kadr macOS window, frame callback and clean timed smoke exit.

No build of kadr, network input, UI automation or desktop capture is performed.
--screenshot opts into capture of this child application's window only and an
ImageIO nonuniform-pixel check. A macOS WindowServer session is required.
"""

import argparse
import contextlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


FRAME_MARKER = "kadr-ui-smoke: frame"
BACKEND_MARKER = "kadr-ui-smoke: backend-ready"


class SmokeError(Exception):
    pass


def seconds(value):
    number = float(value)
    if not 0 < number <= 30:
        raise argparse.ArgumentTypeError("timeout must be > 0 and <= 30 seconds")
    return number


def remaining(deadline):
    value = deadline - time.monotonic()
    if value <= 0:
        raise SmokeError("Timed out waiting for macOS UI smoke completion")
    return value


def stop(process):
    if process is None:
        return
    # Compiler drivers can exit while a frontend/linker descendant still lives.
    # Own the whole session, even after its original leader has been reaped.
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGTERM)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=3)
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGKILL)
    process.wait(timeout=3)


def probe(helper, operation, argument, env, deadline):
    result = subprocess.run(
        [str(helper), operation, str(argument)],
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=min(2, remaining(deadline)),
    )
    if result.returncode == 1 and operation == "--pid":
        return None
    if result.returncode != 0:
        raise SmokeError(result.stderr.strip() or "macOS window probe failed")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise SmokeError("macOS window probe returned invalid JSON") from error


def exercise(binary, artifacts, private, timeout, screenshot):
    home = private / "home"
    directories = {
        "HOME": home,
        "CFFIXED_USER_HOME": home,
        "XDG_CONFIG_HOME": home / "config",
        "XDG_DATA_HOME": home / "data",
        "XDG_CACHE_HOME": home / "cache",
        "XDG_STATE_HOME": home / "state",
        "XDG_RUNTIME_DIR": home / "runtime",
        "TMPDIR": private / "tmp",
    }
    env = os.environ.copy()
    for key, directory in directories.items():
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        env[key] = str(directory)
    helper = private / "macos-window"
    source = Path(__file__).resolve().with_name("macos-window.swift")
    # Compile before the app's short auto-quit timer starts. Cache/build files,
    # HOME and state are removed after the run; only safe diagnostics remain.
    with (artifacts / "helper-build.log").open("wb") as log:
        argv = [
            "xcrun", "swiftc", "-O", "-module-cache-path", str(private / "modules"),
            "-framework", "Foundation", "-framework", "CoreGraphics",
            "-framework", "ImageIO", str(source), "-o", str(helper),
        ]
        compiler = None
        try:
            compiler = subprocess.Popen(
                argv, env=env, stdin=subprocess.DEVNULL,
                stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
            )
            code = compiler.wait(timeout=30)
            if code != 0:
                raise subprocess.CalledProcessError(code, argv)
        finally:
            stop(compiler)
    app = None
    started = time.monotonic()
    deadline = started + timeout
    window = None
    frame = False
    backend_ready = False
    image = None
    app_log = artifacts / "app.log"
    try:
        with app_log.open("wb") as log:
            app = subprocess.Popen(
                [str(binary), "--smoke-ui"],
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            while not (frame and backend_ready and window):
                remaining(deadline)
                lines = app_log.read_text(
                    encoding="utf-8", errors="replace"
                ).splitlines()
                frame = FRAME_MARKER in lines
                backend_ready = BACKEND_MARKER in lines
                if app.poll() is not None:
                    raise SmokeError(
                        f"Application exited before window/frame evidence (code {app.returncode})"
                    )
                if window is None:
                    window = probe(helper, "--pid", app.pid, env, deadline)
                    if window is not None:
                        if (
                            window.get("pid") != app.pid
                            or not isinstance(window.get("window_id"), int)
                            or window["window_id"] <= 0
                            or window.get("width", 0) < 100
                            or window.get("height", 0) < 100
                        ):
                            raise SmokeError("Window probe did not identify the child window")
                        (artifacts / "window.json").write_text(
                            json.dumps(window, indent=2) + "\n", encoding="utf-8"
                        )
                if not (frame and backend_ready and window):
                    time.sleep(min(0.05, remaining(deadline)))
            if screenshot:
                # No whole-desktop fallback: never capture another app's window.
                if app.poll() is not None:
                    raise SmokeError("Application exited before window-only capture")
                current_window = probe(helper, "--pid", app.pid, env, deadline)
                if current_window is None or current_window.get("window_id") != window["window_id"]:
                    raise SmokeError("Child window disappeared or changed before capture")
                with (artifacts / "screencapture.log").open("wb") as capture_log:
                    subprocess.run(
                        [
                            "/usr/sbin/screencapture", "-x", "-o", "-l",
                            str(window["window_id"]), str(artifacts / "window.png"),
                        ],
                        env=env,
                        stdin=subprocess.DEVNULL,
                        stdout=capture_log,
                        stderr=subprocess.STDOUT,
                        timeout=min(5, remaining(deadline)),
                        check=True,
                    )
                image = probe(helper, "--check-image", artifacts / "window.png", env, deadline)
                if (
                    image.get("width", 0) < 100
                    or image.get("height", 0) < 100
                    or image.get("unique_visible_rgb_colors_at_least", 0) <= 16
                ):
                    raise SmokeError("Screenshot dimensions/color evidence is insufficient")
            code = app.wait(timeout=remaining(deadline))
            if code != 0:
                raise SmokeError(f"Application did not quit cleanly (code {code})")
        return {
            "status": "passed",
            "platform": "macOS",
            "binary": str(binary),
            "frame_marker": FRAME_MARKER,
            "window": window,
            "image": image,
            "exit_code": code,
            "runtime_seconds": round(time.monotonic() - started, 3),
        }
    finally:
        stop(app)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary", type=Path, default=Path("target/debug/kadr-app"),
        help="existing executable, including kadr.app/Contents/MacOS/kadr; no Cargo build",
    )
    parser.add_argument(
        "--artifacts", type=Path,
        help="NEW directory for logs, JSON evidence and optional app-window PNG",
    )
    parser.add_argument("--timeout", type=seconds, default=30)
    parser.add_argument(
        "--screenshot", action="store_true",
        help="capture only the child window and require >16 decoded visible RGB colors",
    )
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("requires macOS and an active WindowServer session; no Linux substitute")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"not an executable file: {binary}")
    if shutil.which("xcrun") is None:
        parser.error("xcrun is required to compile the Swift window probe")
    if args.screenshot and not Path("/usr/sbin/screencapture").is_file():
        parser.error("macOS screencapture is unavailable")
    if args.artifacts:
        artifacts = args.artifacts.resolve()
        try:
            artifacts.mkdir(mode=0o700, parents=True, exist_ok=False)
        except FileExistsError:
            parser.error("--artifacts must name a NEW directory")
    else:
        artifacts = Path(tempfile.mkdtemp(prefix="kadr-macos-smoke-"))
    print(f"Artifacts: {artifacts}", flush=True)
    try:
        with tempfile.TemporaryDirectory(prefix="private-", dir=artifacts) as directory:
            result = exercise(binary, artifacts, Path(directory), args.timeout, args.screenshot)
        (artifacts / "result.json").write_text(
            json.dumps(result, indent=2) + "\n", encoding="utf-8"
        )
        print("PASS: native macOS child window, frame callback, normal auto-quit exit 0")
        if result["image"]:
            print("PASS: window-only screenshot decoded with >16 visible RGB colors")
        return 0
    except (
        SmokeError, OSError, subprocess.SubprocessError, ValueError, TypeError, KeyboardInterrupt
    ) as error:
        result = {"status": "failed", "error": str(error) or type(error).__name__}
        (artifacts / "result.json").write_text(
            json.dumps(result, indent=2) + "\n", encoding="utf-8"
        )
        print(f"FAIL: {result['error']}; inspect logs in {artifacts}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
