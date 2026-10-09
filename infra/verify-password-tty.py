#!/usr/bin/env python3
"""Verify CLI terminal echo suppression/restoration without database writes."""
import os
import pty
import select
import subprocess
import termios
import time
from pathlib import Path

binary = Path(__file__).resolve().parents[1] / "target/debug/outreach-cli"
master, slave = pty.openpty()
original = termios.tcgetattr(slave)
environment = dict(os.environ, APP_ENV="invalid-no-database-side-effects")
environment.pop("OUTREACH_ADMIN_PASSWORD", None)
process = subprocess.Popen(
    [str(binary), "bootstrap", "--admin-password-stdin"],
    stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment,
)
try:
    deadline = time.monotonic() + 5
    while termios.tcgetattr(slave)[3] & termios.ECHO:
        if time.monotonic() > deadline or process.poll() is not None:
            raise AssertionError("CLI did not disable terminal echo")
        time.sleep(0.01)
    secret = b"synthetic-tty-test-password-123!"
    os.write(master, secret + b"\n")
    stdout, stderr = process.communicate(timeout=10)
    echoed = os.read(master, 4096) if select.select([master], [], [], 0.1)[0] else b""
    assert secret not in stdout + stderr + echoed, "password was echoed"
    assert termios.tcgetattr(slave) == original, "terminal settings were not restored"
    assert process.returncode != 0, "invalid environment must block database access"
    print("PASS: password hidden, terminal restored, no database access")
finally:
    if process.poll() is None:
        process.kill()
        process.wait()
    termios.tcsetattr(slave, termios.TCSANOW, original)
    os.close(slave)
    os.close(master)
