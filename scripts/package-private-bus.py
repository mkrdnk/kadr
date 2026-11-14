#!/usr/bin/env python3
"""Run a packaging check with an owned private bus, never a desktop bus."""
import contextlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def stop_owned_group(process):
    """Activation children can survive dbus-run-session's original leader."""
    signals = {signal.SIGTERM, signal.SIGINT}
    mask = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
    handlers = {number: signal.getsignal(number) for number in signals}
    try:
        # Ignore (and discard queued) repeated cancellation until TERM/KILL/
        # reap completes, whether cleanup followed success or an interruption.
        for number in signals:
            signal.signal(number, signal.SIG_IGN)
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.02)
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=2)
    finally:
        # Unmask while still ignoring: no deferred signal may interrupt cleanup
        # or replace the original command/cancellation status on the way out.
        signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        for number, handler in handlers.items():
            signal.signal(number, handler)


def run(command):
    env = os.environ.copy()
    for name in (
        "DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS",
        "DBUS_STARTER_BUS_TYPE", "AT_SPI_BUS_ADDRESS",
    ):
        env.pop(name, None)
    # Activated daemons inherit stdout/stderr. A pipe or PTY here can remain
    # open forever after the test footer. Stream a regular file instead, never
    # wait for descendant EOF, and clean up only our newly-created group.
    with tempfile.TemporaryDirectory(prefix="kadr-private-bus-") as directory:
        path = Path(directory) / "session.log"
        with path.open("wb") as output, path.open("rb") as log:
            process = subprocess.Popen(
                ["dbus-run-session", "--", *command],
                env=env, stdin=subprocess.DEVNULL,
                stdout=output, stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            try:
                while process.poll() is None:
                    sys.stdout.buffer.write(log.read(64 * 1024))
                    sys.stdout.buffer.flush()
                    time.sleep(0.02)
                code = process.wait()
            finally:
                stop_owned_group(process)
                # Snapshot, not an EOF wait: even an unexpectedly detached
                # service cannot extend our output drain indefinitely.
                end = os.fstat(log.fileno()).st_size
                while log.tell() < end:
                    sys.stdout.buffer.write(log.read(min(64 * 1024, end - log.tell())))
                sys.stdout.buffer.flush()
    return code if code >= 0 else 128 - code


def interrupted(number, _frame):
    raise SystemExit(128 + number)


def main():
    command = sys.argv[1:]
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        print("Usage: package-private-bus.py -- COMMAND [ARGUMENT...]", file=sys.stderr)
        return 2
    signal.signal(signal.SIGTERM, interrupted)
    try:
        return run(command)
    except KeyboardInterrupt:
        return 130
    except (OSError, subprocess.SubprocessError) as error:
        print(f"Private packaging session failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
