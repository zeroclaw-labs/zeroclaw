#!/usr/bin/env python3
"""Kill stale processes occupying a TCP port (dev helper).

Used by VS Code dev tasks to free the gateway port before cargo-watch
restarts. Best-effort — exits 0 regardless so the real bind error
surfaces naturally if the port cannot be freed.

Usage:
    python3 dev/kill-port.py [PORT]   # default 42617
"""

import os
import platform
import signal
import socket
import subprocess
import sys
import time

DEFAULT_PORT = 42617


def port_is_occupied(port: int) -> bool:
    """Quick TCP connect probe to 127.0.0.1:<port>."""
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(0.2)
    try:
        s.connect(("127.0.0.1", port))
        s.close()
        return True
    except (ConnectionRefusedError, OSError):
        return False


def kill_unix(port: int) -> None:
    """Discover PIDs via lsof and send SIGTERM (macOS / Linux)."""
    try:
        out = subprocess.check_output(
            ["lsof", "-ti", f"tcp:{port}"],
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return

    my_pid = os.getpid()
    for token in out.split():
        try:
            pid = int(token)
        except ValueError:
            continue
        if pid == my_pid:
            continue
        print(f"  Sending SIGTERM to PID {pid}")
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass


def kill_windows(port: int) -> None:
    """Discover PIDs via PowerShell Get-NetTCPConnection and taskkill."""
    try:
        out = subprocess.check_output(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                f"(Get-NetTCPConnection -LocalPort {port} -ErrorAction SilentlyContinue).OwningProcess",
            ],
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return

    my_pid = os.getpid()
    for token in out.split():
        try:
            pid = int(token)
        except ValueError:
            continue
        if pid == my_pid or pid == 0:
            continue
        print(f"  Sending taskkill to PID {pid}")
        subprocess.call(
            ["taskkill", "/F", "/PID", str(pid)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_PORT

    if not port_is_occupied(port):
        print(f"Port {port} is free.")
        return

    print(f"Port {port} is occupied — killing stale process...")

    if platform.system() == "Windows":
        kill_windows(port)
    else:
        kill_unix(port)

    # Wait with back-off for the port to free (up to ~2 s).
    delay = 0.1
    for _ in range(6):
        time.sleep(delay)
        if not port_is_occupied(port):
            print(f"Port {port} freed successfully.")
            return
        delay = min(delay * 2, 0.5)

    print(f"Port {port} still occupied — bind may fail.")


if __name__ == "__main__":
    main()
