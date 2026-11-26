#!/usr/bin/env python3
"""Linux D-Bus/PTY regressions; no Flatpak, Cargo build or desktop interaction."""
import errno
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


WRAPPER = Path(__file__).with_name("package-private-bus.py").resolve()
AVAILABLE = sys.platform == "linux" and all(
    shutil.which(tool) for tool in ("dbus-run-session", "dbus-send")
)


def wait_for(predicate, timeout=3):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("Timed out waiting for regression fixture")


def stopped(pid):
    status = Path(f"/proc/{pid}/stat")
    return not status.exists() or status.read_text().split()[2] == "Z"


@unittest.skipUnless(AVAILABLE, "requires Linux, dbus-run-session and dbus-send")
class PrivateBusTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="kadr-bus-regression-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.record = self.root / "service.json"
        self.service = self.root / "service.py"
        self.service.write_text(
            f"#!{sys.executable}\n"
            "import json,os,pathlib,signal,subprocess,sys,time\n"
            "if sys.argv[1]=='activate':\n"
            " subprocess.Popen([sys.executable,__file__,'orphan',sys.argv[2]])\n"
            "else:\n"
            " def ignore_term(number,frame): pathlib.Path(sys.argv[2]+'.term').write_text('TERM')\n"
            " signal.signal(signal.SIGTERM,ignore_term)\n"
            " record=pathlib.Path(sys.argv[2]); temporary=record.with_suffix('.tmp')\n"
            " temporary.write_text(json.dumps(dict("
            "pid=os.getpid(),pgid=os.getpgrp(),stdout_tty=os.isatty(1),stderr_tty=os.isatty(2),"
            "bus=os.environ.get('DBUS_SESSION_BUS_ADDRESS'))))\n"
            " temporary.replace(record)\n"
            " print('PRIVATE SERVICE STARTED',flush=True)\n"
            " time.sleep(60)\n"
        )
        self.service.chmod(0o700)
        services = self.root / "data/dbus-1/services"
        services.mkdir(parents=True)
        (services / "dev.kadr.PrivateBusRegression.service").write_text(
            "[D-BUS Service]\nName=dev.kadr.PrivateBusRegression\n"
            f"Exec={self.service} activate {self.record}\n"
        )
        self.command = self.root / "command.py"
        self.command.write_text(
            "import pathlib,subprocess,sys,time\n"
            "subprocess.run(['dbus-send','--session','--print-reply','--reply-timeout=250',"
            "'--dest=dev.kadr.PrivateBusRegression','/','dev.kadr.PrivateBusRegression.Test'])\n"
            f"record=pathlib.Path({str(self.record)!r})\n"
            "deadline=time.monotonic()+2\n"
            "while not record.exists() and time.monotonic()<deadline: time.sleep(.02)\n"
            "assert record.exists()\n"
            "print('TEST FOOTER',flush=True)\n"
            "if len(sys.argv)>1: time.sleep(60)\n"
        )
        self.env = {**os.environ, "XDG_DATA_DIRS": str(self.root / "data")}

    def start(self, command):
        master, slave = pty.openpty()
        process = subprocess.Popen(
            [sys.executable, str(WRAPPER), "--", *command],
            env=self.env, stdin=subprocess.DEVNULL, stdout=slave, stderr=slave,
            start_new_session=True,
        )
        os.close(slave)

        def cleanup():
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
            os.close(master)
            if self.record.exists():
                child = json.loads(self.record.read_text())
                # Only a group created for this synthetic test can be signalled.
                if not stopped(child["pid"]) and child["pgid"] != os.getpgrp():
                    os.killpg(child["pgid"], signal.SIGKILL)
        self.addCleanup(cleanup)
        return process, master

    def drain(self, master, timeout=8):
        output = bytearray()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not select.select([master], [], [], 0.05)[0]:
                continue
            try:
                chunk = os.read(master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    return bytes(output)
                raise
            if not chunk:
                return bytes(output)
            output.extend(chunk)
        self.fail("PTY remained open after test footer/cleanup deadline")

    def test_activated_orphan_cannot_hold_pty_or_kill_existing_bus(self):
        # A real, separate session bus stands in for an existing desktop.
        address = self.root / "desktop-address"
        desktop = subprocess.Popen(
            ["dbus-run-session", "--", sys.executable, "-c",
             "import os,pathlib,time; pathlib.Path(" + repr(str(address)) + ").write_text("
             "os.environ['DBUS_SESSION_BUS_ADDRESS']); time.sleep(60)"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
        )
        self.addCleanup(lambda: self.stop_desktop_fixture(desktop))
        wait_for(address.exists)
        self.env["DBUS_SESSION_BUS_ADDRESS"] = address.read_text()
        started = time.monotonic()
        process, master = self.start([sys.executable, str(self.command)])
        output = self.drain(master)
        self.assertEqual(process.wait(timeout=2), 0, output.decode(errors="replace"))
        self.assertIn(b"TEST FOOTER", output)
        self.assertLess(time.monotonic() - started, 8)
        child = json.loads(self.record.read_text())
        self.assertFalse(child["stdout_tty"])
        self.assertFalse(child["stderr_tty"])
        self.assertNotEqual(child["bus"], self.env["DBUS_SESSION_BUS_ADDRESS"])
        wait_for(lambda: stopped(child["pid"]))
        self.assertIsNone(desktop.poll())
        subprocess.run(
            ["dbus-send", "--bus=" + address.read_text(), "--print-reply", "--reply-timeout=1000",
             "--dest=org.freedesktop.DBus", "/", "org.freedesktop.DBus.ListNames"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=True, timeout=2,
        )

    @staticmethod
    def stop_desktop_fixture(desktop):
        if desktop.poll() is None:
            os.killpg(desktop.pid, signal.SIGTERM)
            desktop.wait(timeout=3)

    def test_signal_cleanup_kills_only_private_activation_descendants(self):
        process, master = self.start([sys.executable, str(self.command), "hold"])
        wait_for(self.record.exists)
        process.terminate()
        self.drain(master)
        self.assertEqual(process.wait(timeout=2), 128 + signal.SIGTERM)
        child = json.loads(self.record.read_text())
        wait_for(lambda: stopped(child["pid"]))

    def test_cancellation_during_normal_cleanup_cannot_skip_kill(self):
        process, master = self.start([sys.executable, str(self.command)])
        wait_for(lambda: Path(str(self.record) + ".term").exists())
        process.terminate()
        self.drain(master)
        self.assertEqual(process.wait(timeout=2), 0)
        child = json.loads(self.record.read_text())
        wait_for(lambda: stopped(child["pid"]))

    def test_repeated_term_and_interrupt_cannot_abort_cancellation_cleanup(self):
        process, master = self.start([sys.executable, str(self.command), "hold"])
        wait_for(self.record.exists)
        process.terminate()
        wait_for(lambda: Path(str(self.record) + ".term").exists())
        process.terminate()
        process.send_signal(signal.SIGINT)
        self.drain(master)
        self.assertEqual(process.wait(timeout=2), 128 + signal.SIGTERM)
        child = json.loads(self.record.read_text())
        wait_for(lambda: stopped(child["pid"]))

    def test_command_failure_is_not_reported_as_success(self):
        process, master = self.start([sys.executable, "-c", "raise SystemExit(7)"])
        self.drain(master)
        self.assertEqual(process.wait(timeout=2), 7)


if __name__ == "__main__":
    unittest.main()
