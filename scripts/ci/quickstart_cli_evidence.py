#!/usr/bin/env python3
"""Manually invoked, synthetic CLI evidence. No review or release authority."""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import pty
import re
import secrets
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import termios
import time
import tomllib
import tty
import zipfile


SOURCE_SHA = "a441e0cb755ebb3e1656eeee04e694c070d79214"
SOURCE_TREE = "5c4290c8ba4b62309f8311fa38cf628f120720c0"
REPOSITORY = "https://github.com/zeroclaw-labs/zeroclaw.git"
PACKAGE = "tool-fixture"
PTY_CAP = 128 * 1024
STAGE_CAP = 8 * 1024 * 1024
JSON_CAP = 64 * 1024
CONFIG_CAP = 256 * 1024
IMAGE_CAP = 2 * 1024 * 1024
ARTIFACT_CAP = 16 * 1024 * 1024
CASES = ("A", "B", "C")
SCREENS = ("A-list.png", "A-completion-0.png", "A-completion-1.png",
           "A-completion-2.png", "B-completion-0.png", "B-completion-1.png")
EXPORTS = ("provenance.json", "context.json", "cases.json", "actions.json",
           "affected-output.txt", "checksums.json", "cleanup.json")
ANSI = re.compile(rb"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")
CAVEAT = ("The tool registry registers it when it is next built, unless its name "
          "clashes with a tool already registered or its tool description cannot be read.")
CLI_ARTIFACT = ("cli-target", "debug", "zeroclaw")
COMPONENT_ARTIFACT = ("fixture-target", "wasm32-wasip2", "debug",
                      "zeroclaw_tool_plugin_fixture.wasm")
# The first received cancellation is a process-local fact, not a signal mask.
# Only explicit normal-work checkpoints raise; cleanup and receipt commits defer it.
PENDING_CANCELLATION = None


class Refusal(RuntimeError):
    """Errors contain stable codes, never input, config, or subprocess output."""


def require(condition, code):
    if not condition:
        raise Refusal(code)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def cancellation_pending():
    return PENDING_CANCELLATION is not None


def cancellation_checkpoint():
    require(not cancellation_pending(), "interrupted")


def bounded_read(path, limit, owners=None):
    if owners is None:
        owners = (os.getuid(),)
    st = path.lstat()
    require(stat.S_ISREG(st.st_mode) and st.st_uid in owners
            and st.st_nlink == 1 and st.st_size <= limit, "file_boundary")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        opened = os.fstat(fd)
        require((opened.st_dev, opened.st_ino) == (st.st_dev, st.st_ino), "file_changed")
        data = bytearray()
        while True:
            chunk = os.read(fd, min(4096, limit + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
            require(len(data) <= limit, "read_overflow")
        return bytes(data)
    finally:
        os.close(fd)


def compiler_artifact_spec(relative):
    require(type(relative) in (tuple, list), "artifact_path")
    relative = tuple(relative)
    if relative == CLI_ARTIFACT:
        return 256 * 1024 * 1024, 0o700
    require(relative == COMPONENT_ARTIFACT, "artifact_path")
    return 8 * 1024 * 1024, 0o600


def artifact_identity(st):
    return {key: getattr(st, "st_" + key) for key in
            ("dev", "ino", "uid", "mode", "nlink", "size", "mtime_ns", "ctime_ns")}


def close_artifact_fds(fds):
    failed = False
    for fd in reversed(fds):
        try:
            os.close(fd)
        except BaseException:
            failed = True
    require(not failed, "artifact_close")


def artifact_directory(private, parts, fds, bindings):
    require(private.is_absolute(), "artifact_root")
    before = private.lstat()
    require(stat.S_ISDIR(before.st_mode) and before.st_uid == os.getuid()
            and stat.S_IMODE(before.st_mode) == 0o700, "artifact_root")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    parent = os.open(private, flags)
    fds.append(parent)
    identity = lambda st: (st.st_dev, st.st_ino, st.st_uid, st.st_mode)
    require(identity(os.fstat(parent)) == identity(before), "artifact_directory_changed")
    bindings.append((None, private, parent, identity(before)))
    for part in parts:
        require(type(part) is str and part not in ("", ".", "..") and "/" not in part,
                "artifact_path")
        child = os.open(part, flags, dir_fd=parent)
        fds.append(child)
        current = os.fstat(child)
        require(stat.S_ISDIR(current.st_mode) and current.st_uid == os.getuid()
                and not current.st_mode & 0o022, "artifact_directory")
        bindings.append((parent, part, child, identity(current)))
        parent = child
    return parent


def verify_artifact_directories(bindings):
    for parent, name, fd, wanted in bindings:
        named = os.lstat(name) if parent is None else os.stat(
            name, dir_fd=parent, follow_symlinks=False)
        for current in (named, os.fstat(fd)):
            require((current.st_dev, current.st_ino, current.st_uid, current.st_mode) == wanted,
                    "artifact_directory_changed")


def read_compiler_artifact(private, relative, deadline, consume=None):
    """Only these completed Cargo outputs may be regular multi-link sources.

    All private copies, receipts and exports still use strict bounded_read.
    Held directory/file descriptors and named-path rechecks bind this observation;
    they do not defend against a privileged writer or attest a running compiler.
    """
    limit = compiler_artifact_spec(relative)[0]
    fds, bindings = [], []
    try:
        parent = artifact_directory(private, relative[:-1], fds, bindings)
        named = os.stat(relative[-1], dir_fd=parent, follow_symlinks=False)
        fd = os.open(relative[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                     dir_fd=parent)
        fds.append(fd)
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_uid == os.getuid()
                and before.st_nlink > 0 and 0 < before.st_size <= limit
                and not before.st_mode & (stat.S_ISUID | stat.S_ISGID | 0o022), "artifact_source")
        require(tuple(relative) != CLI_ARTIFACT or before.st_mode & stat.S_IXUSR,
                "artifact_executable")
        require(artifact_identity(named) == artifact_identity(before), "artifact_changed")
        digest, total = hashlib.sha256(), 0
        while True:
            cancellation_checkpoint()
            require(time.monotonic() < deadline, "artifact_deadline")
            request = min(4096, limit + 1 - total)
            chunk = os.read(fd, request)
            require(len(chunk) <= request, "artifact_read")
            if not chunk:
                break
            total += len(chunk)
            require(total <= limit, "artifact_overflow")
            digest.update(chunk)
            if consume is not None:
                consume(chunk)
        require(total == before.st_size and artifact_identity(os.fstat(fd)) == artifact_identity(before)
                and artifact_identity(os.stat(relative[-1], dir_fd=parent, follow_symlinks=False))
                == artifact_identity(before), "artifact_changed")
        verify_artifact_directories(bindings)
        return {"identity": artifact_identity(before), "bytes": total, "sha256": digest.hexdigest()}
    except OSError:
        raise Refusal("artifact_io") from None
    finally:
        close_artifact_fds(fds)


def materialized_artifact_path(private, relative):
    compiler_artifact_spec(relative)
    return private / "materialized" / relative[-1]


def materialize_compiler_artifact(private, relative, deadline):
    limit, mode = compiler_artifact_spec(relative)
    fds, bindings = [], []
    try:
        parent = artifact_directory(private, ("materialized",), fds, bindings)
        fd = os.open(relative[-1], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                     | os.O_CLOEXEC, 0o600, dir_fd=parent)
        fds.append(fd)
        def consume(chunk):
            offset = 0
            while offset < len(chunk):
                cancellation_checkpoint()
                require(time.monotonic() < deadline, "artifact_deadline")
                count = os.write(fd, chunk[offset:])
                require(type(count) is int and 0 < count <= len(chunk) - offset, "artifact_write")
                offset += count
        source = read_compiler_artifact(private, relative, deadline, consume)
        os.fchmod(fd, mode)
        copied = os.fstat(fd)
        require(stat.S_ISREG(copied.st_mode) and copied.st_uid == os.getuid()
                and copied.st_nlink == 1 and stat.S_IMODE(copied.st_mode) == mode
                and copied.st_size == source["bytes"]
                and (copied.st_dev, copied.st_ino) != (source["identity"]["dev"], source["identity"]["ino"]),
                "artifact_copy")
        require(artifact_identity(os.stat(relative[-1], dir_fd=parent, follow_symlinks=False))
                == artifact_identity(copied), "artifact_copy_changed")
        verify_artifact_directories(bindings)
        record = {"relative": list(relative), "source": source["identity"],
                  "copy": artifact_identity(copied), "bytes": source["bytes"], "sha256": source["sha256"]}
    except OSError:
        raise Refusal("artifact_io") from None
    finally:
        close_artifact_fds(fds)
    path = materialized_artifact_path(private, relative)
    data = bounded_read(path, limit)
    require(len(data) == record["bytes"] and sha(data) == record["sha256"]
            and artifact_identity(path.lstat()) == record["copy"], "artifact_copy_changed")
    return record


def verify_materialized_artifact(private, record, deadline):
    require(type(record) is dict and set(record) == {"relative", "source", "copy", "bytes", "sha256"},
            "artifact_ledger")
    relative = record["relative"]
    source = read_compiler_artifact(private, relative, deadline)
    require(source == {"identity": record["source"], "bytes": record["bytes"],
                       "sha256": record["sha256"]}, "artifact_source_changed")
    path = materialized_artifact_path(private, relative)
    require(artifact_identity(path.lstat()) == record["copy"], "artifact_copy_changed")
    data = bounded_read(path, compiler_artifact_spec(relative)[0])
    require(len(data) == record["bytes"] and sha(data) == record["sha256"]
            and artifact_identity(path.lstat()) == record["copy"], "artifact_copy_changed")


def private_write(path, data, replace=False):
    require(not path.parent.is_symlink(), "symlink_parent")
    target = path.with_name(path.name + ".new-" + secrets.token_hex(6)) if replace else path
    fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as out:
        out.write(data)
    if replace:
        os.replace(target, path)


def read_json(path):
    value = json.loads(bounded_read(path, JSON_CAP))
    require(type(value) is dict, "json_schema")
    return value


def write_json(path, value, replace=False, ready=False):
    data = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    require(len(data) <= JSON_CAP, "json_overflow")
    private_write(path, data, replace)
    if ready:
        private_write(path.with_name(path.name + ".ready"), b"")


def owned_path(root, path):
    require(root.is_absolute() and not root.is_symlink(), "root_boundary")
    require(path.is_relative_to(root) and path != root, "path_boundary")
    for parent in (path, *path.parents):
        if parent == root:
            break
        require(not parent.is_symlink(), "symlink_boundary")
    return path


def context_from_env(env):
    keys = ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_JOB", "GITHUB_WORKFLOW_SHA")
    require(all(keys[i] in env for i in range(len(keys))), "missing_context")
    require(re.fullmatch(r"[1-9][0-9]*", env[keys[0]]) is not None
            and re.fullmatch(r"[1-9][0-9]*", env[keys[1]]) is not None
            and re.fullmatch(r"[A-Za-z0-9_-]+", env[keys[2]]) is not None
            and re.fullmatch(r"[0-9a-f]{40}", env[keys[3]]) is not None, "bad_context")
    return {"run_id": env[keys[0]], "attempt": env[keys[1]], "job": env[keys[2]],
            "auxiliary_sha": env[keys[3]], "source_sha": SOURCE_SHA, "source_tree": SOURCE_TREE}


def validate_binding(actual, expected):
    require(type(actual) is dict and type(expected) is dict, "binding_schema")
    for key in ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree",
                "case", "window", "window_pixels", "xterm_pid", "binary_sha256"):
        require(key in actual and actual[key] == expected[key], "binding_" + key)


def validate_terminal(values):
    require(values.get("isatty") == [True, True, True]
            and values.get("rows") == 24 and values.get("columns") == 80, "missing_pty")


def visible(raw):
    return ANSI.sub(b"", raw).replace(b"\r", b"").decode("utf-8", errors="replace")


class OutputGuard:
    def __init__(self, secret, cap=PTY_CAP):
        self.secret = secret.encode()
        self.cap = cap
        self.data = bytearray()
        self.observed = hashlib.sha256()
        self.forwarded = hashlib.sha256()
        self.forwarded_bytes = 0

    def accept(self, chunk):
        require(len(self.data) + len(chunk) <= self.cap, "output_overflow")
        require(self.secret not in bytes(self.data[-len(self.secret):]) + chunk, "secret_echo")
        self.data.extend(chunk)
        self.observed.update(chunk)

    def forward(self, chunk, writer):
        offset = 0
        while offset < len(chunk):
            count = writer(chunk[offset:])
            require(type(count) is int and 0 < count <= len(chunk) - offset, "display_write")
            self.forwarded.update(chunk[offset:offset + count])
            self.forwarded_bytes += count
            offset += count

    def verify(self):
        require(self.forwarded_bytes == len(self.data)
                and self.observed.digest() == self.forwarded.digest(), "display_fidelity")


def validate_case(case, config, text, events):
    require("Install tool-fixture without these settings?" not in text, "settings_fallback")
    require("These settings do not satisfy the schema of tool-fixture:" not in text,
            "settings_validation_failed")
    plugins = config.get("plugins", {})
    require(type(plugins) is dict, "plugins_schema")
    rows = plugins.get("entries", [])
    require(type(rows) is list, "rows_schema")
    require("active as a tool" not in text, "obsolete_active")
    base = {"case": case, "row_count": len(rows), "no_settings_fallback": True}
    if case == "C":
        require("required-mode-escape" in events and "required-mode-enter" not in events,
                "cancel_action")
        require("Configuration of tool-fixture was cancelled." in text and not rows
                and "tool-fixture: admitted as a tool" not in text
                and plugins.get("enabled", False) is False
                and plugins.get("auto_discover", False) is False, "cancel_not_refused")
        return {**base, "configured": False, "cancelled": True, "flags_off": True}
    require(plugins.get("enabled") is True and plugins.get("auto_discover") is True,
            "activation_missing")
    require(CAVEAT in text, "registry_caveat_missing")
    require("Quickstart complete." in text, "completion_missing")
    if case == "B":
        require(not rows and (PACKAGE + ": admitted as a tool. It has nothing to configure, "
                             "so it needs no config entry. " + CAVEAT) in text, "stateless_readiness")
        return {**base, "configured": True, "no_entry_needed": True}
    require(case == "A" and "required-mode-enter" in events
            and "required-mode-escape" not in events, "selection_action")
    require(len(rows) == 1 and type(rows[0]) is dict, "row_count")
    row = rows[0]
    values = row.get("config", {})
    require(type(values) is dict and "mode" in values and type(values["mode"]) is str
            and values["mode"] == "", "mode_not_selected_empty_string")
    require(type(values.get("api_token")) is str and values["api_token"].startswith("enc2:"),
            "secret_not_encrypted")
    require(row.get("egress", {}).get("hosts", []) == [], "network_grant_changed")
    key = row.get("name")
    require(type(key) is str and (PACKAGE + ": admitted as a tool (config entry " + key
                                  + "). " + CAVEAT) in text, "row_readiness")
    return {**base, "configured": True, "mode_present": True, "mode_type": "string",
            "mode_utf8_bytes": 0, "secret_encrypted": True, "egress_withheld": True,
            "entry_key": key}


def process_identity(pid):
    """Linux numeric birth fields only; never export proc comm/argv/environment."""
    require(type(pid) is int and pid > 0, "process_identity")
    with open("/proc/" + str(pid) + "/stat", "rb", buffering=0) as handle:
        raw = handle.read(4097)
    require(len(raw) <= 4096, "process_stat_boundary")
    left, right = raw.find(b"("), raw.rfind(b") ")
    require(left > 0 and right > left, "process_stat_schema")
    fields = raw[right + 2:].split()
    require(len(fields) > 19 and raw[:left].strip() == str(pid).encode(), "process_stat_schema")
    try:
        values = [int(fields[i]) for i in (1, 2, 3, 19)]
    except ValueError:
        raise Refusal("process_stat_schema") from None
    require(all(v >= 0 for v in values) and values[3] > 0, "process_stat_schema")
    return {"pid": pid, "parent_pid": values[0], "pgid": values[1],
            "session": values[2], "start_ticks": values[3], "state": fields[0].decode("ascii")}


def group_exists(pgid, deadline, live_only=False):
    """Bounded observation, not signal0 or ownership authorization."""
    with os.scandir("/proc") as entries:
        for count, entry in enumerate(entries):
            require(count < 4096 and time.monotonic() < deadline, "group_observation_boundary")
            if not entry.name.isdigit():
                continue
            try:
                current = process_identity(int(entry.name))
            except (FileNotFoundError, ProcessLookupError):
                continue
            if current["pgid"] == pgid and (not live_only or current["state"] not in ("Z", "X")):
                return True
    return False


class OwnedChild:
    """Single-threaded Linux direct-child anchor, retained through group cleanup.

    poll/wait below use WNOWAIT. No Popen.poll/wait, SIGCHLD reaper or concurrent
    direct wait may retire this anchor before the last group signal.
    """
    def __init__(self, pid, role, proc=None):
        self.pid, self.role, self.proc = pid, role, proc
        self.stdout = proc.stdout if proc is not None else None
        self.parent = os.getpid()
        self.identity = None
        self.returncode = None
        self.retired = False
        self.direct_waits = 0
        self.direct_reaped = False
        self.signals = []
        self._evidence_cleanup = None

    def bind(self, deadline):
        require(self.role in ("command", "display", "registry", "terminal", "cli"), "process_role")
        require(sys.platform == "linux" and hasattr(os, "WNOWAIT")
                and signal.getsignal(signal.SIGCHLD) == signal.SIG_DFL, "process_wait_contract")
        while True:
            require(time.monotonic() < deadline, "process_bind_timeout")
            self._observe()
            current = process_identity(self.pid)
            require(current["parent_pid"] == self.parent, "process_parent")
            if current["pgid"] == self.pid and current["session"] == self.pid:
                self.identity = {k: current[k] for k in
                                 ("pid", "parent_pid", "pgid", "session", "start_ticks")}
                return self
            time.sleep(0.02)

    def _observe(self):
        require(not self.retired, "process_retired")
        if self.proc is not None and self.proc.returncode is not None:
            self.retired = True
            raise Refusal("process_already_reaped")
        try:
            result = os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError:
            self.retired = True
            raise Refusal("process_already_reaped") from None
        if result is not None:
            require(result.si_pid == self.pid and result.si_code in
                    (os.CLD_EXITED, os.CLD_KILLED, os.CLD_DUMPED), "process_wait_identity")
            self.returncode = result.si_status if result.si_code == os.CLD_EXITED else -result.si_status
        return self.returncode

    def poll(self):
        require(self.identity is not None and not self.retired, "process_unbound_or_retired")
        require(signal.getsignal(signal.SIGCHLD) == signal.SIG_DFL, "process_wait_contract")
        code = self._observe()
        current = process_identity(self.pid)
        require(all(current[k] == v for k, v in self.identity.items()), "process_identity_changed")
        return code

    def wait(self, timeout):
        """Exit observation only. Direct reap belongs exclusively to stop()."""
        end = time.monotonic() + timeout
        while True:
            cancellation_checkpoint()
            code = self.poll()
            if code is not None:
                return code
            require(time.monotonic() < end, "subprocess_timeout")
            time.sleep(0.02)

    def signal_group(self, sig):
        require(sig in (signal.SIGTERM, signal.SIGKILL), "process_signal")
        self.poll()  # Direct waitability + birth/parent/session/PGID immediately before signal.
        os.killpg(self.identity["pgid"], sig)
        self.signals.append(int(sig))

    def stop(self):
        if self._evidence_cleanup is not None:
            return self._evidence_cleanup
        absent, verified, failure = False, False, None
        try:
            self.poll()
            verified = True
            for sig, seconds in ((signal.SIGTERM, 3), (signal.SIGKILL, 2)):
                try:
                    self.signal_group(sig)
                except ProcessLookupError:
                    self.poll()  # ESRCH is not a replacement ownership proof.
                end = time.monotonic() + seconds
                while time.monotonic() < end:
                    exited = self.poll() is not None
                    if exited:
                        if sig == signal.SIGKILL:
                            break
                        try:
                            if not group_exists(self.pid, end, live_only=True):
                                break
                        except Refusal as error:
                            if str(error) != "group_observation_boundary":
                                raise
                            break  # TERM budget exhausted; revalidate before KILL.
                    time.sleep(0.02)
            self.poll()
            require(self.returncode is not None, "process_exit_unobserved")
            # Retire BEFORE direct wait so an interrupt/read/wait fault cannot
            # authorize a later numeric killpg after reap or PID retirement.
            self.retired = True
            self.direct_waits += 1
            waited, status = os.waitpid(self.pid, os.WNOHANG)
            require(waited == self.pid, "process_direct_wait")
            self.direct_reaped = True
            self.returncode = os.waitstatus_to_exitcode(status)
            if self.proc is not None:
                self.proc.returncode = self.returncode
            while group_exists(self.pid, end):
                require(time.monotonic() < end, "process_group_remains")
                time.sleep(0.02)
            absent = True
        except BaseException as error:
            failure = str(error) if isinstance(error, Refusal) else "process_cleanup_fault"
        self._evidence_cleanup = {
            "pid": self.pid, "role": self.role, "identity": self.identity,
            "identity_verified": verified, "ownership_retired": self.retired,
            "direct_waits": self.direct_waits, "direct_reaped": self.direct_reaped,
            "group_absent": absent, "returncode": self.returncode, "failure": failure,
            "signals": self.signals}
        return self._evidence_cleanup


def stop_process(child):
    require(isinstance(child, OwnedChild), "process_owner_required")
    return child.stop()


def cleanup_proved(result):
    return (result.get("direct_reaped") is True and result.get("group_absent") is True
            and result.get("identity_verified") is True and result.get("ownership_retired") is True
            and result.get("direct_waits") == 1 and result.get("failure") is None)


def finish_owned_child(child):
    """No cancellation checkpoint until stop, all owned closes and cache commit finish."""
    result = None
    try:
        result = stop_process(child)
    except BaseException:
        result = {"pid": child.pid, "role": child.role, "direct_reaped": False,
                  "group_absent": False, "failure": "cleanup_error"}
        child._evidence_cleanup = result
    finally:
        if child.stdout is not None:
            try:
                child.stdout.close()
                require(child.stdout.closed is True, "process_pipe_close")
            except BaseException:
                result["failure"] = "process_pipe_close"
    return result


class Processes:
    def __init__(self, deadline):
        self.deadline = deadline
        self.items = []

    def spawn(self, args, env=None, cwd=None, stdout=subprocess.PIPE, role="command"):
        cancellation_checkpoint()
        require(time.monotonic() < self.deadline, "job_deadline")
        proc = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                stdout=stdout, stderr=subprocess.STDOUT, start_new_session=True)
        child = OwnedChild(proc.pid, role, proc)
        self.items.append(child)  # Retain even if identity acquisition refuses.
        # Do not honor a newly latched cancellation between creation/registration
        # and identity acquisition. The owning caller can now enter its finally.
        return child.bind(min(self.deadline, time.monotonic() + 1))

    def run(self, args, seconds, env=None, cwd=None, cap=STAGE_CAP):
        proc = None
        data = bytearray()
        try:
            proc = self.spawn(args, env, cwd)
            end = min(self.deadline, time.monotonic() + seconds)
            cancellation_checkpoint()
            os.set_blocking(proc.stdout.fileno(), False)
            while True:
                cancellation_checkpoint()
                require(time.monotonic() < end, "subprocess_timeout")
                ready, _, _ = select.select([proc.stdout], [], [], min(0.1, end - time.monotonic()))
                if ready:
                    part = os.read(proc.stdout.fileno(), 4096)
                    if not part:
                        break
                    data.extend(part)
                    require(len(data) <= cap, "subprocess_output_overflow")
                elif proc.poll() is not None:
                    continue
            require(proc.wait(timeout=max(0.01, end - time.monotonic())) == 0, "subprocess_failed")
        finally:
            if proc is not None:
                result = finish_owned_child(proc)
                require(cleanup_proved(result), "subprocess_cleanup")
        cancellation_checkpoint()
        return bytes(data)

    def cleanup(self):
        results = []
        for proc in reversed(self.items):
            try:
                results.append(finish_owned_child(proc))
            except BaseException:
                results.append({"pid": proc.pid, "direct_reaped": False,
                                "group_absent": False, "failure": "cleanup_error"})
        return results


def git(manager, repo, *args):
    return manager.run(["git", "--no-optional-locks", "-C", str(repo), *args], 10)


def snapshot(manager, repo):
    records = []
    for entry in git(manager, repo, "ls-files", "--stage", "-z").split(b"\0"):
        cancellation_checkpoint()
        if not entry:
            continue
        header, name = entry.split(b"\t", 1)
        mode, oid, stage = header.decode().split()
        require(stage == "0", "index_conflict")
        path = repo / name.decode()
        if mode == "160000":
            digest = oid
        elif mode == "120000":
            raw = os.readlink(path).encode()
            # Match this legacy Git object format; content receipts use SHA256.
            require(hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw,
                                 usedforsecurity=False).hexdigest() == oid,
                    "source_overlay")
            digest = sha(raw)
        else:
            raw = bounded_read(path, 32 * 1024 * 1024)
            canonical = raw.replace(b"\r\n", b"\n") if path.suffix in (".ps1", ".sln") else raw
            require(hashlib.sha1(b"blob " + str(len(canonical)).encode() + b"\0" + canonical,
                                 usedforsecurity=False).hexdigest() == oid,
                    "source_overlay")
            digest = sha(raw)
        records.append((name.decode(), mode, oid, digest))
    return sha(json.dumps(records, separators=(",", ":")).encode())


def safe_env():
    keys = ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "LANG", "LC_ALL")
    return {key: os.environ[key] for key in keys if key in os.environ}


def fixture_manifest(case):
    prefix = ('name = "tool-fixture"\nversion = "0.1.0"\nwasm_path = "tool-fixture.wasm"\n'
              'capabilities = ["tool"]\n')
    if case == "B":
        return prefix + "permissions = []\n"
    return prefix + '''permissions = ["http_client", "config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
additionalProperties = false
required = ["api_token", "mode"]
[config_schema.properties.api_token]
type = "string"
x-secret = true
[config_schema.properties.mode]
type = "string"
enum = ["", "auto"]
[egress]
hosts = ["api.example.com"]
'''


def fixture_archive(manifest, wasm):
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_STORED) as archive:
        for name, data in (("manifest.toml", manifest.encode()), ("tool-fixture.wasm", wasm)):
            info = zipfile.ZipInfo("tool-fixture/" + name, date_time=(2020, 1, 1, 0, 0, 0))
            info.external_attr = 0o100600 << 16
            archive.writestr(info, data)
    data = out.getvalue()
    require(len(data) <= 8 * 1024 * 1024, "archive_overflow")
    return data


def request_path(data):
    require(len(data) <= 4096 and data.endswith(b"\r\n\r\n"), "request_boundary")
    require(data.find(b"\r\n\r\n") == len(data) - 4, "request_pipeline_refused")
    lines = data.split(b"\r\n")
    require(lines[0] in (b"GET /registry.json HTTP/1.1", b"GET /tool-fixture-0.1.0.zip HTTP/1.1"),
            "request_refused")
    require(not any(line.lower().startswith((b"transfer-encoding:", b"content-length:"))
                    for line in lines[1:]), "request_body_refused")
    return lines[0].split()[1].decode()


def serve(spec_path):
    spec = read_json(spec_path)
    directory = spec_path.parent
    counts = {"index": 0, "archive": 0, "unexpected": 0}
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.settimeout(0.2)
    try:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        port = listener.getsockname()[1]
        archive = bounded_read(directory / "package.zip", 8 * 1024 * 1024)
        index = json.dumps({"plugins": [{"name": PACKAGE, "version": "0.1.0",
                            "capabilities": ["tool"], "url": f"http://127.0.0.1:{port}/tool-fixture-0.1.0.zip",
                            "sha256": sha(archive)}]}).encode()
        require(len(index) <= 16 * 1024, "index_overflow")
        write_json(directory / "server-ready.json", {"pid": os.getpid(), "port": port,
                                                     "index_sha256": sha(index)}, ready=True)
        write_json(directory / "server-counts.json", counts)
        while time.monotonic() < spec["deadline"]:
            cancellation_checkpoint()
            try:
                client, _ = listener.accept()
            except socket.timeout:
                continue
            with client:
                client.settimeout(2)
                try:
                    data = bytearray()
                    end = time.monotonic() + 2
                    while not data.endswith(b"\r\n\r\n"):
                        require(time.monotonic() < end and len(data) < 4096, "request_boundary")
                        part = client.recv(min(4096 - len(data), 4096))
                        require(bool(part), "request_boundary")
                        data.extend(part)
                    path = request_path(bytes(data))
                    require(sum(counts.values()) < 12, "request_count_overflow")
                    key = "index" if path == "/registry.json" else "archive"
                    counts[key] += 1
                    body = index if key == "index" else archive
                    client.sendall(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: "
                                   + str(len(body)).encode() + b"\r\n\r\n" + body)
                except (OSError, Refusal):
                    counts["unexpected"] += 1
                write_json(directory / "server-counts.json", counts, replace=True)
                if counts["unexpected"]:
                    raise Refusal("unexpected_network_request")
    finally:
        listener.close()


def window_binding(spec, window):
    return {**spec["binding"], "window": window["window"], "xterm_pid": window["xterm_pid"],
            "window_pixels": window["window_pixels"]}


def wait_file(path, deadline):
    marker = path.with_name(path.name + ".ready")
    while not marker.exists():
        cancellation_checkpoint()
        require(time.monotonic() < deadline, "ipc_timeout")
        time.sleep(0.02)
    cancellation_checkpoint()
    return read_json(path)


def terminal_info(fd=0):
    rows, columns, _, _ = struct.unpack("HHHH", fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8))
    return {"isatty": [os.isatty(i) for i in (0, 1, 2)], "rows": rows, "columns": columns}


def bounded_write(fd, data, deadline):
    offset = 0
    while offset < len(data):
        cancellation_checkpoint()
        require(time.monotonic() < deadline, "write_timeout")
        _, ready, _ = select.select([], [fd], [], min(0.05, deadline - time.monotonic()))
        if ready:
            try:
                count = os.write(fd, data[offset:offset + 4096])
            except BlockingIOError:
                continue
            require(count > 0, "write_refused")
            offset += count
    return offset


class Bridge:
    def __init__(self, spec_path):
        self.spec_path = spec_path
        self.directory = spec_path.parent
        self.spec = read_json(spec_path)
        self.deadline = min(self.spec["deadline"], time.monotonic() + 120)
        self.guard = OutputGuard(self.spec["secret"])
        self.cursor = 0
        self.events = []
        self.master = None
        self.pid = None
        self.child = None
        self.status = None
        self.binding = None
        self.replies = 0

    def pump(self, deadline):
        cancellation_checkpoint()
        require(time.monotonic() < min(deadline, self.deadline), "prompt_timeout")
        ready, _, _ = select.select([self.master, 0], [], [], min(0.05, deadline - time.monotonic()))
        if 0 in ready:
            reply = os.read(0, 4096)
            self.replies += len(reply)
            require(self.replies <= 4096, "terminal_reply_overflow")
            if reply:
                bounded_write(self.master, reply, min(deadline, self.deadline))
        if self.master in ready:
            try:
                data = os.read(self.master, 4096)
            except OSError as error:
                if error.errno == errno.EIO:
                    data = b""
                else:
                    raise
            if not data:
                return False
            self.guard.accept(data)
            self.guard.forward(data, lambda chunk: bounded_write(1, chunk, min(deadline, self.deadline)))
        return True

    def expect(self, pattern):
        end = min(self.deadline, time.monotonic() + 10)
        while True:
            text = visible(bytes(self.guard.data[self.cursor:]))
            if re.search(pattern, text):
                self.cursor = len(self.guard.data)
                return text
            require(self.pump(end), "early_cli_exit")

    def send(self, action, data):
        cancellation_checkpoint()
        validate_terminal(terminal_info())
        validate_terminal(terminal_info(self.master))
        require(len(data) <= 256, "input_overflow")
        bounded_write(self.master, data, min(self.deadline, time.monotonic() + 10))
        self.events.append({"id": action, "output_offset": len(self.guard.data),
                            "monotonic": time.monotonic()})

    def checklist(self, index, action):
        self.expect(r"Open a selector \(Enter\), or pick Create")
        self.send(action, b"\x1b[B" * index + b"\r")

    def frame(self, name):
        validate_terminal(terminal_info())
        validate_terminal(terminal_info(self.master))
        sequence = len([e for e in self.events if e["id"].startswith("frame:")])
        target = self.directory / ("frame-request-" + str(sequence) + ".json")
        write_json(target, {"name": name, "binding": self.binding,
                            "terminal": terminal_info(),
                            "output_offset": len(self.guard.data)}, ready=True)
        wait_file(self.directory / ("frame-done-" + str(sequence) + ".json"),
                  min(self.deadline, time.monotonic() + 8))
        self.events.append({"id": "frame:" + name, "output_offset": len(self.guard.data)})

    def drive(self):
        self.checklist(0, "provider-selector")
        self.expect(r"Model provider")
        self.send("use-existing-provider", b"\r")
        self.expect(r"Pick a configured provider")
        self.send("select-synthetic-provider", b"\r")
        self.checklist(1, "risk-selector")
        risk = self.expect(r"Locked Down|Pick a preset")
        if "Locked Down" not in risk:
            self.send("fresh-risk-preset", b"\x1b[B\r")
            self.expect(r"Locked Down")
        self.send("locked-down-preset", b"\r")
        self.checklist(2, "memory-selector")
        self.expect(r"Memory backend")
        self.send("memory-none", b"\x1b[B" * 5 + b"\r")
        self.checklist(3, "channels-selector")
        self.expect(r"Channels \(optional")
        self.send("no-channels-done", b"\x1b[B\r")
        self.checklist(5, "plugins-selector")
        self.expect(r"tool-fixture")
        self.send("pick-only-fixture", b" \r")
        self.checklist(7, "create")
        self.expect(r"Install tool-fixture although nothing verifies who published it")
        self.send("explicit-unsigned-install", b"y")
        if self.spec["case"] != "B":
            self.expect(r"(?i)network")
            self.send("withhold-network-default", b"\r")
            self.expect(r"api_token")
            self.send("masked-fake-secret", self.spec["secret"].encode() + b"\r")
            self.expect(r"mode[\s\S]*auto")
            if self.spec["case"] == "A":
                self.frame("A-list.png")
                self.send("required-mode-enter", b"\r")
            else:
                self.send("required-mode-escape", b"\x1b")
        if self.spec["case"] != "C":
            self.expect(r"Activate plugins now")
            self.send("explicit-activation", b"y")

    def run(self):
        old = termios.tcgetattr(0)
        old_flags = fcntl.fcntl(1, fcntl.F_GETFL)
        cleanup = {"direct_reaped": False, "group_absent": False}
        failure = None
        info = terminal_info()
        validate_terminal(info)
        window = wait_file(self.directory / "window.json", min(self.deadline, time.monotonic() + 10))
        self.binding = window_binding(self.spec, window)
        require(sha(bounded_read(Path(self.spec["binary"]), 256 * 1024 * 1024))
                == self.binding["binary_sha256"], "binary_changed")
        read_end, write_end = os.pipe()
        try:
            tty.setraw(0)
            os.set_blocking(1, False)
            cancellation_checkpoint()
            self.pid, self.master = pty.fork()
            if self.pid == 0:
                try:
                    os.close(write_end)
                    require(os.read(read_end, 1) == b"1", "exec_handshake")
                    os.close(read_end)
                    child_info = terminal_info()
                    validate_terminal(child_info)
                    write_json(self.directory / "child-terminal.json", {
                        **child_info, "pid": os.getpid(), "parent_pid": os.getppid()})
                    os.chdir(self.spec["config"])
                    os.execve(self.spec["binary"], self.spec["argv"], self.spec["environment"])
                except BaseException:
                    os._exit(125)
            os.close(read_end)
            read_end = None
            self.child = OwnedChild(self.pid, "cli")
            self.child.bind(min(self.deadline, time.monotonic() + 1))
            fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            os.set_blocking(self.master, False)
            validate_terminal(terminal_info(self.master))
            os.write(write_end, b"1")
            os.close(write_end)
            write_end = None
            self.drive()
            while self.pump(self.deadline):
                pass
            self.status = self.child.wait(max(0, self.deadline - time.monotonic()))
            require(self.status == 0, "cli_nonzero")
            self.guard.verify()
        except Exception as error:
            failure = str(error) if isinstance(error, Refusal) else "bridge_io_failure"
        finally:
            if self.child is not None:
                cleanup = finish_owned_child(self.child)
                self.status = self.child.returncode
            if not cleanup_proved(cleanup):
                failure = "cli_cleanup_incomplete"
            for fd in (self.master, read_end, write_end):
                if fd is not None:
                    try:
                        os.close(fd)
                    except BaseException:
                        failure = "bridge_cleanup_incomplete"
            for restore in (lambda: termios.tcsetattr(0, termios.TCSANOW, old),
                            lambda: fcntl.fcntl(1, fcntl.F_SETFL, old_flags)):
                try:
                    restore()
                except BaseException:
                    failure = "bridge_cleanup_incomplete"
            if cancellation_pending() and failure is None:
                failure = "interrupted"
            try:
                private_write(self.directory / "output.private", bytes(self.guard.data))
            except BaseException:
                failure = "bridge_output_incomplete"
            result = {
                "binding": self.binding, "terminal": info, "cli_pid": self.pid,
                "parent_pid": os.getpid(), "exit": self.status, "failure": failure,
                "observed_sha256": self.guard.observed.hexdigest(),
                "display_sha256": self.guard.forwarded.hexdigest(),
                "bytes": len(self.guard.data), "display_bytes": self.guard.forwarded_bytes,
                "events": self.events, "cleanup": cleanup}
            target = self.directory / "bridge-result.json"
            write_json(target, result)
            # A signal delivered while writing the private result must appear
            # in it before ready admission. Later cancellation still exits1;
            # the ready commit is not a retroactive OS cancellation transaction.
            if cancellation_pending() and result["failure"] is None:
                failure = result["failure"] = "interrupted"
                write_json(target, result, replace=True)
            marker = target.with_name(target.name + ".ready")
            private_write(marker, b"")
            if cancellation_pending() and result["failure"] is None:
                marker.unlink()
                failure = result["failure"] = "interrupted"
                write_json(target, result, replace=True)
                private_write(marker, b"")
            if cancellation_pending():
                failure = failure or "interrupted"
        return 1 if failure else 0


def validate_png(data):
    require(len(data) <= IMAGE_CAP and data[:8] == b"\x89PNG\r\n\x1a\n"
            and data[12:16] == b"IHDR", "image_boundary")
    width, height = struct.unpack(">II", data[16:24])
    require(0 < width <= 1280 and 0 < height <= 1024, "image_dimensions")
    return {"width": width, "height": height, "sha256": sha(data)}


def validate_bridge(result, binding):
    validate_binding(result["binding"], binding)
    validate_terminal(result["terminal"])
    require(result.get("failure") is None and result.get("exit") == 0, "bridge_failed")
    require(result["observed_sha256"] == result["display_sha256"]
            and result["bytes"] == result["display_bytes"], "display_fidelity")
    require(result["cleanup"].get("direct_reaped") is True
            and result["cleanup"].get("group_absent") is True
            and result["cleanup"].get("identity_verified") is True
            and result["cleanup"].get("ownership_retired") is True
            and result["cleanup"].get("direct_waits") == 1
            and type(result.get("cli_pid")) is int and result["cli_pid"] > 0
            and result["cleanup"].get("pid") == result["cli_pid"], "cli_cleanup_missing")
    require(cleanup_proved(result["cleanup"]) and result["cleanup"].get("role") == "cli",
            "cli_cleanup_missing")
    identity = result["cleanup"].get("identity", {})
    require(identity.get("pid") == result["cli_pid"]
            and identity.get("pgid") == result["cli_pid"]
            and identity.get("session") == result["cli_pid"]
            and identity.get("parent_pid") == result.get("parent_pid")
            and type(identity.get("start_ticks")) is int and identity["start_ticks"] > 0,
            "cli_identity_missing")


def verify_export(root, tokens):
    total = 0
    for path in root.rglob("*"):
        if path.is_dir():
            require(path == root / "screens" and not path.is_symlink(), "export_directory")
            continue
        relative = path.relative_to(root).as_posix()
        allowed = relative in EXPORTS or relative in {"screens/" + x for x in SCREENS}
        require(allowed, "export_not_whitelisted")
        require(stat.S_IMODE(path.lstat().st_mode) == 0o600, "export_permissions")
        data = bounded_read(path, IMAGE_CAP if relative.startswith("screens/") else JSON_CAP)
        require(not any(token.encode() in data for token in tokens), "export_secret")
        if relative.endswith(".json"):
            pending = [json.loads(data)]
            while pending:
                value = pending.pop()
                if isinstance(value, dict):
                    require(not set(value).intersection({"config", "environment", "env", "private_key",
                            "private_key_hash", "private_key_sha256", "key_hash", "key_sha256"}),
                            "export_private_fields")
                    pending.extend(value.values())
                elif isinstance(value, list):
                    pending.extend(value)
        total += len(data)
        require(total <= ARTIFACT_CAP, "artifact_overflow")
    return total


def verify_context(actual, expected):
    require(type(actual) is dict and all(actual.get(k) == v for k, v in expected.items()),
            "export_binding")


def bundle_inventory(root):
    expected = set(EXPORTS) | {"screens/" + name for name in SCREENS}
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if not p.is_dir()}
    require(actual == expected, "export_incomplete_whitelist")
    return {name: {"bytes": len(data), "sha256": sha(data)} for name in sorted(actual)
            for data in [bounded_read(root / name, IMAGE_CAP if name.startswith("screens/") else JSON_CAP)]}


def validate_bundle(root, tokens, context):
    require(root.is_dir() and not root.is_symlink() and root.stat().st_uid == os.getuid()
            and stat.S_IMODE(root.stat().st_mode) == 0o700, "export_root")
    verify_export(root, tokens)
    inventory = bundle_inventory(root)
    provenance, display = read_json(root / "provenance.json"), read_json(root / "context.json")
    verify_context(provenance, context)
    verify_context(display, context)
    require(all(re.fullmatch(r"[0-9a-f]{64}", provenance.get(k, "")) is not None for k in
                ("source_before", "source_after", "auxiliary_before", "auxiliary_after",
                 "binary_sha256", "binary_sha256_after", "component_sha256", "component_sha256_after"))
            and provenance.get("execution_started") is True
            and provenance.get("source_before") == provenance.get("source_after")
            and provenance.get("auxiliary_before") == provenance.get("auxiliary_after")
            and provenance["binary_sha256"] == provenance["binary_sha256_after"]
            and provenance["component_sha256"] == provenance["component_sha256_after"],
            "export_source_proof")
    frames = display.get("frames", [])
    require(len(frames) == 6 and {f["name"] for f in frames} == set(SCREENS), "export_frames")
    for frame in frames:
        binding = frame["binding"]
        wanted = {**context, "case": frame["name"][0], "window": binding["window"],
                  "xterm_pid": binding["xterm_pid"], "window_pixels": binding["window_pixels"],
                  "binary_sha256": provenance["binary_sha256"]}
        validate_binding(binding, wanted)
        require(frame.get("capture") == "actual-owned-xterm-window" and frame.get("edited") is False,
                "export_frame_origin")
        data = bounded_read(root / "screens" / frame["name"], IMAGE_CAP)
        pixels = validate_png(data)
        require(pixels == frame["pixels"] and [pixels["width"], pixels["height"]]
                == binding["window_pixels"], "export_frame_bytes")
    cases = read_json(root / "cases.json")
    require(cases.get("failure") is None and len(cases.get("cases", [])) == 3, "export_cases")
    require([c["binding"]["case"] for c in cases["cases"]] == list(CASES), "export_cases")
    for case in cases["cases"]:
        verify_context(case["binding"], context)
    actions = read_json(root / "actions.json")
    require([c["case"] for c in actions.get("cases", [])] == list(CASES), "export_actions")
    for case in actions["cases"]:
        verify_context(case["binding"], context)
    cleanup = read_json(root / "cleanup.json")
    require(cleanup.get("status") == "reaped" and cleanup.get("temporary_material_removed") is True,
            "export_cleanup")
    checksums = read_json(root / "checksums.json").get("files")
    require(checksums == {n: v["sha256"] for n, v in inventory.items() if n != "checksums.json"},
            "export_checksums")
    return inventory


def revoke_success(root):
    # Never copy refused content to a safe-failure path. Missing decision means
    # no rich upload even if publication was interrupted after directory rename.
    (root / "export-decision.json").unlink(missing_ok=True)


def publish_bundle(root, staging, tokens, context):
    require(staging.parent == root and staging.name == "export-staging-private", "export_stage")
    require(not (root / "export").exists() and not (root / "export-decision.json").exists(),
            "export_already_published")
    try:
        inventory = validate_bundle(staging, tokens, context)
        os.rename(staging, root / "export")
        # Detect staging/publication faults or a changed allowed file before the
        # decision. Finalizer rechecks these exact bytes independently before upload.
        require(validate_bundle(root / "export", tokens, context) == inventory, "export_changed")
        write_json(root / "export-decision.json", {"schema": 1, "kind": "success", "safe": True,
                   "binding": context, "files": inventory})
    except BaseException:
        revoke_success(root)
        raise


FAILURE_CODES = ("setup_failed_before_harness", "capture_failed", "capture_refused",
                 "cleanup_incomplete", "export_refused", "finalizer_refused", "interrupted")


def verify_failure(root, context):
    decision = read_json(root / "failure-decision.json")
    require(set(decision) == {"schema", "kind", "safe", "binding", "receipt_sha256"}
            and decision["schema"] == 1 and decision["kind"] == "failure"
            and decision["safe"] is True and decision["binding"] == context, "failure_decision")
    data = bounded_read(root / "failure" / "receipt.json", JSON_CAP)
    require(stat.S_IMODE((root / "failure/receipt.json").lstat().st_mode) == 0o600,
            "failure_permissions")
    require(sha(data) == decision["receipt_sha256"], "failure_changed")
    receipt = json.loads(data)
    require(set(receipt) == {"schema", "kind", "binding", "code", "cleanup_complete", "execution_proof"}
            and receipt["schema"] == 1 and receipt["kind"] == "failure"
            and receipt["binding"] == context and receipt["code"] in FAILURE_CODES
            and type(receipt["cleanup_complete"]) is bool and receipt["execution_proof"] is False,
            "failure_schema")
    return receipt


def minimum_failure(root, context, code, cleanup_complete=False):
    require(code in FAILURE_CODES, "failure_code")
    require(context == context_from_env({"GITHUB_RUN_ID": context["run_id"],
            "GITHUB_RUN_ATTEMPT": context["attempt"], "GITHUB_JOB": context["job"],
            "GITHUB_WORKFLOW_SHA": context["auxiliary_sha"]}), "failure_binding")
    revoke_success(root)
    directory = root / "failure"
    directory.mkdir(mode=0o700, exist_ok=True)
    require(not directory.is_symlink() and directory.stat().st_uid == os.getuid(), "failure_directory")
    write_json(directory / "receipt.json", {"schema": 1, "kind": "failure", "binding": context,
               "code": code, "cleanup_complete": cleanup_complete, "execution_proof": False}, replace=True)
    write_json(root / "failure-decision.json", {"schema": 1, "kind": "failure", "safe": True,
               "binding": context, "receipt_sha256": sha(bounded_read(directory / "receipt.json", JSON_CAP))},
               replace=True)
    verify_failure(root, context)


def export_outputs(success, failure):
    destination = os.environ.get("GITHUB_OUTPUT")
    require(destination is not None, "missing_output_channel")
    with open(destination, "a", encoding="ascii") as out:
        out.write("success_export=" + str(success).lower() + "\n")
        out.write("failure_export=" + str(failure).lower() + "\n")


def remove_owned(root, path):
    owned_path(root, path)
    require(path.is_dir() and not path.is_symlink() and path.stat().st_uid == os.getuid(),
            "cleanup_boundary")
    shutil.rmtree(path)


class Capture:
    def __init__(self, source, auxiliary, root):
        self.source, self.auxiliary, self.root = source, auxiliary, root
        self.context = context_from_env(os.environ)
        require(root.name == "quickstart-cli-evidence-" + self.context["run_id"] + "-"
                + self.context["attempt"] and root.parent == Path(os.environ["RUNNER_TEMP"]),
                "root_binding")
        require(root.stat().st_uid == os.getuid() and stat.S_IMODE(root.stat().st_mode) == 0o700
                and not root.is_symlink(), "root_permissions")
        state = read_json(root / "state.json")
        require(state["run_id"] == self.context["run_id"] and state["attempt"] == self.context["attempt"]
                and state["job"] == self.context["job"] and state["started"] is False, "state_binding")
        self.start = state["started_monotonic"]
        self.manager = Processes(self.start + 44 * 60)
        self.private = root / "private"
        self.export = root / "export"
        self.staging = root / "export-staging-private"
        require(not self.export.exists() and not self.staging.exists(), "export_not_fresh")
        self.private.mkdir(mode=0o700)
        self.tokens = []
        self.cases = []
        self.actions = []
        self.frames = []
        self.started_cases = []
        self.slices = []
        self.failure = None
        self.provenance = {**self.context, "execution_started": False,
                           "owner_disposition": "PENDING", "visual_acceptance": "PENDING"}
        write_json(root / "state.json", {**state, "started": True}, replace=True)

    def tool(self, args, seconds=8, cap=JSON_CAP):
        return self.manager.run(args, seconds, env=safe_env(), cap=cap)

    def setup(self):
        require(git(self.manager, self.source, "rev-parse", "HEAD").decode().strip() == SOURCE_SHA,
                "source_sha")
        require(git(self.manager, self.source, "rev-parse", "HEAD^{tree}").decode().strip() == SOURCE_TREE,
                "source_tree")
        require(git(self.manager, self.source, "remote", "get-url", "origin").decode().strip()
                in (REPOSITORY, REPOSITORY.removesuffix(".git")), "source_origin")
        require(git(self.manager, self.auxiliary, "rev-parse", "HEAD").decode().strip()
                == self.context["auxiliary_sha"], "auxiliary_sha")
        require(not git(self.manager, self.source, "status", "--porcelain", "--untracked-files=no"),
                "source_dirty")
        self.source_snapshot = snapshot(self.manager, self.source)
        self.aux_snapshot = snapshot(self.manager, self.auxiliary)
        self.provenance.update({"source_before": self.source_snapshot,
                               "auxiliary_before": self.aux_snapshot,
                               "parents": git(self.manager, self.source, "show", "-s", "--format=%P", "HEAD").decode().strip().split(),
                               "cargo": {p: sha(bounded_read(self.source / p, 4 * 1024 * 1024))
                                         for p in ("Cargo.toml", "Cargo.lock", "crates/zeroclaw-plugins/tests/fixtures/tool-fixture/Cargo.toml")}})
        remaining = self.start + 240 - time.monotonic()
        require(remaining > 0, "setup_budget")
        self.manager.run(["sudo", "-n", "apt-get", "update", "-q"], remaining, env=safe_env())
        self.manager.run(["sudo", "-n", "apt-get", "install", "-y", "--no-install-recommends",
                          "xterm", "xvfb", "xauth", "xdotool", "imagemagick", "fonts-dejavu-core"],
                         max(0, self.start + 240 - time.monotonic()), env=safe_env())
        self.manager.run(["rustup", "target", "add", "--toolchain", "1.98.0", "wasm32-wasip2"],
                         max(0, self.start + 240 - time.monotonic()), env=safe_env())
        version = self.tool(["rustc", "+1.98.0", "-vV"]).decode()
        require("release: 1.98.0" in version and "host: x86_64-unknown-linux-gnu" in version,
                "compiler_binding")
        self.provenance["compiler"] = version
        self.display = ":" + str(100 + int(self.context["run_id"]) % 400)
        number = self.display[1:]
        require(not Path("/tmp/.X" + number + "-lock").exists()
                and not Path("/tmp/.X11-unix/X" + number).exists(), "display_owned_elsewhere")
        auth = self.private / "Xauthority"
        private_write(auth, b"")
        self.tool(["xauth", "-f", str(auth), "add", self.display, "MIT-MAGIC-COOKIE-1", secrets.token_hex(16)])
        self.gui_env = {**safe_env(), "DISPLAY": self.display, "XAUTHORITY": str(auth),
                        "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}
        self.display_proc = self.manager.spawn(["Xvfb", self.display, "-auth", str(auth),
                                                "-nolisten", "tcp", "-screen", "0", "1280x1024x24"],
                                               env=self.gui_env, stdout=subprocess.DEVNULL, role="display")
        end = min(self.manager.deadline, time.monotonic() + 8)
        while not Path("/tmp/.X11-unix/X" + number).exists():
            require(time.monotonic() < end and self.display_proc.poll() is None, "display_setup")
            time.sleep(0.02)
        font = self.tool(["fc-match", "-f", "%{file}", "DejaVu Sans Mono"]).decode().strip()
        require(Path(font).is_relative_to(Path("/usr/share/fonts")), "font_boundary")
        self.provenance["font"] = {"file": font, "sha256": sha(bounded_read(
            Path(font), 4 * 1024 * 1024, owners=(0, os.getuid())))}
        self.provenance["tools"] = self.tool(["dpkg-query", "-W", "-f=${Package} ${Version}\n",
                                              "xterm", "xvfb", "xdotool", "imagemagick", "fonts-dejavu-core"]).decode()

    def build(self):
        cli_target = self.private / "cli-target"
        fixture_target = self.private / "fixture-target"
        cli = ["cargo", "+1.98.0", "build", "--locked", "-p", "zeroclaw", "--bin", "zeroclaw",
               "--features", "plugins-wasm-cranelift", "--target-dir", str(cli_target)]
        fixture = ["cargo", "+1.98.0", "build", "--locked", "--quiet", "--package",
                   "zeroclaw-tool-plugin-fixture", "--target", "wasm32-wasip2",
                   "--target-dir", str(fixture_target)]
        self.provenance["build"] = {"cli_argv": cli, "fixture_argv": fixture,
                                    "profile": "debug", "default_features": True,
                                    "explicit_features": ["plugins-wasm-cranelift"],
                                    "cache_upload": False}
        self.manager.run(cli, 25 * 60, env=safe_env(), cwd=self.source)
        self.manager.run(fixture, 6 * 60, env=safe_env(), cwd=self.source)
        (self.private / "materialized").mkdir(mode=0o700)
        self.compiled_artifacts = {
            "binary": materialize_compiler_artifact(self.private, CLI_ARTIFACT, self.manager.deadline),
            "component": materialize_compiler_artifact(self.private, COMPONENT_ARTIFACT, self.manager.deadline)}
        self.binary = materialized_artifact_path(self.private, CLI_ARTIFACT)
        self.wasm_path = materialized_artifact_path(self.private, COMPONENT_ARTIFACT)
        self.binary_hash = sha(bounded_read(self.binary, 256 * 1024 * 1024))
        self.wasm = bounded_read(self.wasm_path, 8 * 1024 * 1024)
        self.provenance.update({"binary_sha256": self.binary_hash, "component_sha256": sha(self.wasm),
                               "execution_started": True})

    def capture_frame(self, request, binding):
        name = request["name"]
        require(name in SCREENS and name not in [x["name"] for x in self.frames], "frame_scope")
        validate_binding(request["binding"], binding)
        validate_terminal(request["terminal"])
        geometry = self.manager.run(["xdotool", "getwindowgeometry", "--shell", str(binding["window"])],
                                    2, env=self.gui_env, cap=1024).decode().splitlines()
        geometry = dict(line.split("=", 1) for line in geometry)
        dimensions = (int(geometry["WIDTH"]), int(geometry["HEIGHT"]))
        require(dimensions == tuple(binding["window_pixels"]), "window_resized")
        time.sleep(0.1)
        destination = self.private / (name + ".capture")
        private_write(destination, b"")
        self.manager.run(["import", "-display", self.display, "-window", str(binding["window"]),
                          "-limit", "memory", "32MiB", "png:" + str(destination)],
                         8, env=self.gui_env, cap=JSON_CAP)
        data = bounded_read(destination, IMAGE_CAP)
        metrics = validate_png(data)
        require((metrics["width"], metrics["height"]) == dimensions, "pixel_window_binding")
        frame = {"name": name, "binding": binding, "output_offset": request["output_offset"],
                 "monotonic": time.monotonic(), "pixels": metrics,
                 "capture": "actual-owned-xterm-window", "edited": False}
        self.frames.append(frame)
        return destination

    def case(self, case):
        directory = self.private / case
        directory.mkdir(mode=0o700)
        config_dir = directory / "config"
        config_dir.mkdir(mode=0o700)
        (directory / "tmp").mkdir(mode=0o700)
        secret = secrets.token_hex(24)
        self.tokens.append(secret)
        manifest = fixture_manifest(case)
        archive = fixture_archive(manifest, self.wasm)
        private_write(directory / "package.zip", archive)
        deadline = min(self.manager.deadline, time.monotonic() + 120)
        spec_path = directory / "spec.json"
        binding = {**self.context, "case": case, "binary_sha256": self.binary_hash}
        write_json(spec_path, {"deadline": deadline, "case": case})
        script = str(Path(__file__).resolve())
        server = self.manager.spawn([sys.executable, script, "--serve", str(spec_path)],
                                    env=safe_env(), stdout=subprocess.DEVNULL, role="registry")
        server_info = wait_file(directory / "server-ready.json", min(deadline, time.monotonic() + 8))
        require(server_info["pid"] == server.pid, "server_binding")
        url = "http://127.0.0.1:" + str(server_info["port"])
        config = ('schema_version = 3\nlocale = "en"\n[providers.models.ollama.pty-proof]\n'
                  'model = "synthetic-unused"\nuri = ' + json.dumps(url) + '\n'
                  '[plugins]\nenabled = false\nauto_discover = false\nplugins_dir = '
                  + json.dumps(str(config_dir / "plugins")) + '\n[secrets]\nencrypt = true\n')
        private_write(config_dir / "config.toml", config.encode())
        environment = {**self.gui_env, "PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
                       "TMPDIR": str(directory / "tmp"), "ZEROCLAW_PLUGIN_REGISTRY_URL": url + "/registry.json"}
        argv = [str(self.binary), "--config-dir", str(config_dir), "--log-level", "error",
                "quickstart", "--agent", "pty-evidence"]
        write_json(spec_path, {"deadline": deadline, "case": case, "binding": binding,
                              "secret": secret, "binary": str(self.binary), "config": str(config_dir),
                              "argv": argv, "environment": environment}, replace=True)
        title = "quickstart-evidence-" + self.context["run_id"] + "-" + self.context["attempt"] + "-" + case
        xterm = self.manager.spawn(["xterm", "-display", self.display, "-geometry", "80x24", "-u8",
                                   "-fa", "DejaVu Sans Mono", "-fs", "10", "-sl", "200", "-hold",
                                   "-title", title, "-e", sys.executable, script, "--bridge", str(spec_path)],
                                  env=self.gui_env, stdout=subprocess.DEVNULL, role="terminal")
        self.started_cases.append(directory)
        window = None
        search_end = min(deadline, time.monotonic() + 8)
        search_count = 0
        while window is None:
            search_count += 1
            require(time.monotonic() < search_end and search_count <= 16
                    and xterm.poll() is None, "window_setup")
            try:
                ids = self.manager.run(["xdotool", "search", "--onlyvisible", "--pid", str(xterm.pid),
                                        "--name", "^" + title + "$"],
                                       min(2, search_end - time.monotonic()), env=self.gui_env, cap=1024).split()
                require(len(ids) == 1 and ids[0].isdigit(), "window_ambiguous")
                window = int(ids[0])
            except Refusal as error:
                require(str(error) == "subprocess_failed", "window_search_failed")
                time.sleep(0.02)
        actual_pid = self.manager.run(["xdotool", "getwindowpid", str(window)], 2,
                                      env=self.gui_env, cap=1024).decode().strip()
        require(actual_pid == str(xterm.pid), "window_pid")
        geometry = self.manager.run(["xdotool", "getwindowgeometry", "--shell", str(window)], 2,
                                    env=self.gui_env, cap=1024).decode().splitlines()
        geometry = dict(line.split("=", 1) for line in geometry)
        pixels = [int(geometry["WIDTH"]), int(geometry["HEIGHT"])]
        binding = {**binding, "window": window, "xterm_pid": xterm.pid, "window_pixels": pixels}
        write_json(directory / "window.json", {"window": window, "xterm_pid": xterm.pid,
                                                "window_pixels": pixels}, ready=True)
        completed_frames = 0
        while not (directory / "bridge-result.json.ready").exists():
            cancellation_checkpoint()
            require(time.monotonic() < deadline and server.poll() is None, "case_timeout")
            request_pathname = directory / ("frame-request-" + str(completed_frames) + ".json")
            if request_pathname.with_name(request_pathname.name + ".ready").exists():
                request = read_json(request_pathname)
                self.capture_frame(request, binding)
                write_json(directory / ("frame-done-" + str(completed_frames) + ".json"),
                           {"done": True}, ready=True)
                completed_frames += 1
            time.sleep(0.02)
        cancellation_checkpoint()
        result = read_json(directory / "bridge-result.json")
        validate_bridge(result, binding)
        child_terminal = read_json(directory / "child-terminal.json")
        validate_terminal(child_terminal)
        require(child_terminal["pid"] == result["cli_pid"]
                and child_terminal["parent_pid"] == result["parent_pid"], "child_pid_binding")
        raw = bounded_read(directory / "output.private", PTY_CAP)
        require(sha(raw) == result["observed_sha256"] and secret.encode() not in raw, "output_binding")
        counts = read_json(directory / "server-counts.json")
        require(counts == {"index": 1, "archive": 1, "unexpected": 0}, "route_counts")
        text = visible(raw)
        parsed = tomllib.loads(bounded_read(config_dir / "config.toml", CONFIG_CAP).decode())
        events = [e["id"] for e in result["events"]]
        observed = validate_case(case, parsed, text, events)
        if case == "C":
            require(not (config_dir / "plugins" / PACKAGE).exists(), "cancel_published")
        if case != "C":
            self.manager.run(["xdotool", "windowfocus", "--sync", str(window)], 2, env=self.gui_env)
            frame_count = 3 if case == "A" else 2
            for number in range(frame_count):
                if number:
                    self.manager.run(["xdotool", "key", "--clearmodifiers", "shift+Prior"], 2,
                                     env=self.gui_env)
                    time.sleep(0.1)
                self.capture_frame({"name": case + "-completion-" + str(number) + ".png",
                                    "binding": binding, "terminal": child_terminal,
                                    "output_offset": len(raw)}, binding)
        self.actions.append({"case": case, "binding": binding, "events": result["events"],
                             "scrollback_actions": 2 if case == "A" else (1 if case == "B" else 0)})
        affected = [line for line in text.splitlines() if "admitted as a tool" in line
                    or "Configuration of tool-fixture was cancelled." in line]
        require(all(secret not in line for line in affected), "affected_secret")
        self.slices.extend(case + ": " + line for line in affected)
        server_cleanup = stop_process(server)
        terminal_cleanup = stop_process(xterm)
        require(all(cleanup_proved(x) for x in (server_cleanup, terminal_cleanup)), "case_cleanup")
        self.cases.append({"binding": binding, "argv": argv, "terminal": child_terminal,
                           "declared_context": {"locale": "en", "TERM": "xterm-256color",
                                                "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
                                                "PATH": "/usr/bin:/bin", "TMPDIR": str(directory / "tmp"),
                                                "registry_url": url + "/registry.json", "display": self.display},
                           "display_byte_fidelity": True, "output_sha256": sha(raw),
                           "manifest_sha256": sha(manifest.encode()), "archive_sha256": sha(archive),
                           "registry_index_sha256": server_info["index_sha256"], "route_counts": counts,
                           "observed": observed, "cli_exit": result["exit"], "cli_cleanup": result["cleanup"],
                           "server_cleanup": server_cleanup, "terminal_cleanup": terminal_cleanup})

    def export_files(self, cleanup):
        require(self.failure is None, "export_task_failed")
        self.staging.mkdir(mode=0o700)
        target = self.staging
        write_json(target / "provenance.json", self.provenance)
        write_json(target / "context.json", {**self.context, "locale": "en", "rows": 24,
                                                  "columns": 80, "frames": self.frames,
                                                  "renderer": "actual-xterm-window", "pixel_review": "PENDING"})
        write_json(target / "cases.json", {"cases": self.cases, "failure": self.failure})
        write_json(target / "actions.json", {"cases": self.actions})
        private_write(target / "affected-output.txt", ("\n".join(self.slices) + "\n").encode())
        write_json(target / "cleanup.json", cleanup)
        if self.failure is None:
            screens = target / "screens"
            screens.mkdir(mode=0o700)
            for frame in self.frames:
                data = bounded_read(self.private / (frame["name"] + ".capture"), IMAGE_CAP)
                require(sha(data) == frame["pixels"]["sha256"], "frame_changed")
                private_write(screens / frame["name"], data)
        self.seal_staging()
        verify_export(target, self.tokens)

    def seal_staging(self):
        files = {p.relative_to(self.staging).as_posix(): sha(bounded_read(p, IMAGE_CAP))
                 for p in self.staging.rglob("*") if p.is_file() and p.name != "checksums.json"}
        write_json(self.staging / "checksums.json", {"files": files}, replace=True)

    def run(self):
        try:
            cancellation_checkpoint()
            self.setup()
            self.build()
            for case in CASES:
                self.case(case)
            require(snapshot(self.manager, self.source) == self.source_snapshot, "source_changed")
            require(snapshot(self.manager, self.auxiliary) == self.aux_snapshot, "auxiliary_changed")
            require(type(self.compiled_artifacts) is dict
                    and set(self.compiled_artifacts) == {"binary", "component"}
                    and self.compiled_artifacts["binary"]["relative"] == list(CLI_ARTIFACT)
                    and self.compiled_artifacts["component"]["relative"] == list(COMPONENT_ARTIFACT)
                    and self.binary == materialized_artifact_path(self.private, CLI_ARTIFACT)
                    and self.wasm_path == materialized_artifact_path(self.private, COMPONENT_ARTIFACT),
                    "artifact_ledger")
            for record in self.compiled_artifacts.values():
                verify_materialized_artifact(self.private, record, self.manager.deadline)
            require(sha(bounded_read(self.binary, 256 * 1024 * 1024)) == self.binary_hash
                    and sha(bounded_read(self.wasm_path, 8 * 1024 * 1024)) == sha(self.wasm), "binary_changed")
            self.provenance.update({"source_after": self.source_snapshot, "auxiliary_after": self.aux_snapshot,
                                    "binary_sha256_after": self.binary_hash, "component_sha256_after": sha(self.wasm)})
        except BaseException as error:
            self.failure = str(error) if isinstance(error, Refusal) else "capture_failed"
        finally:
            # The signal handler only latches. Cleanup/receipt preparation has
            # no cancellation throw; export admission below follows owned cleanup.
            resources = self.manager.cleanup()
            reaped = all(cleanup_proved(r) for r in resources)
            cli_cleanup = []
            for directory in self.started_cases:
                end = time.monotonic() + 5
                while not (directory / "bridge-result.json.ready").exists() and time.monotonic() < end:
                    time.sleep(0.02)
                try:
                    require((directory / "bridge-result.json.ready").exists(), "missing_reap_receipt")
                    receipt = read_json(directory / "bridge-result.json")
                    record = {"case": directory.name, **receipt["cleanup"]}
                except (OSError, ValueError, KeyError, Refusal):
                    record = {"case": directory.name, "direct_reaped": False, "group_absent": False}
                cli_cleanup.append(record)
                reaped = reaped and cleanup_proved(record)
            display_files_remaining = False
            if hasattr(self, "display"):
                number = self.display[1:]
                display_files_remaining = (Path("/tmp/.X" + number + "-lock").exists()
                                           or Path("/tmp/.X11-unix/X" + number).exists())
                reaped = reaped and not display_files_remaining
            if not reaped:
                self.failure = "cleanup_incomplete"
            elif cancellation_pending():
                self.failure = "interrupted"
            cleanup = {"status": "reaped" if reaped else "incomplete", "harness_started": True,
                       "owned_processes": resources, "CLI_reap_receipts": cli_cleanup,
                       "display_files_remaining": display_files_remaining,
                       "temporary_material_removed": False,
                       "limitations": ["Host death/SIGKILL can prevent finally or direct-reap proof.",
                                       "Group absence does not attest escaped descendants or whole-host network isolation.",
                                       "Pixel acceptance remains a human decision."]}
            try:
                if self.failure is None and reaped:
                    self.export_files(cleanup)  # All rich bytes remain private.
                if cancellation_pending() and self.failure is None:
                    self.failure = "interrupted"
                if reaped:
                    remove_owned(self.root, self.private)
                    cleanup["temporary_material_removed"] = True
                if self.failure is None and reaped:
                    write_json(self.staging / "cleanup.json", cleanup, replace=True)
                    self.seal_staging()
                    cancellation_checkpoint()
                    publish_bundle(self.root, self.staging, self.tokens, self.context)
                    cancellation_checkpoint()
                    write_json(self.root / "finished.json", {"binding": self.context,
                               "complete": True, "cleanup": True})
            except BaseException:
                self.failure = "export_refused"
            if cancellation_pending() and reaped:
                self.failure = "interrupted"
            if self.failure is not None:
                minimum_failure(self.root, self.context,
                                "cleanup_incomplete" if not reaped else (
                                    "interrupted" if cancellation_pending() else "capture_refused"))
        return 1 if self.failure else 0


def finalize(root):
    context = context_from_env(os.environ)
    require(root.name == "quickstart-cli-evidence-" + context["run_id"] + "-" + context["attempt"]
            and root.parent == Path(os.environ["RUNNER_TEMP"]) and not root.is_symlink()
            and root.stat().st_uid == os.getuid() and stat.S_IMODE(root.stat().st_mode) == 0o700,
            "root_binding")
    try:
        cancellation_checkpoint()
        state = read_json(root / "state.json")
        require(state["started"] is True and os.environ.get("EVIDENCE_CAPTURE_OUTCOME") == "success",
                "capture_not_successful")
        done = read_json(root / "finished.json")
        require(done == {"binding": context, "complete": True, "cleanup": True}, "incomplete")
        decision = read_json(root / "export-decision.json")
        require(set(decision) == {"schema", "kind", "safe", "binding", "files"}
                and decision["schema"] == 1 and decision["kind"] == "success"
                and decision["safe"] is True and decision["binding"] == context, "export_decision")
        require(validate_bundle(root / "export", [], context) == decision["files"], "export_changed")
        cancellation_checkpoint()
        export_outputs(True, False)
        cancellation_checkpoint()
    except BaseException:
        try:
            revoke_success(root)
            try:
                verify_failure(root, context)
            except BaseException:
                minimum_failure(root, context, "finalizer_refused")
            # Never admit an unchecked old receipt or an unchecked fallback write.
            verify_failure(root, context)
            export_outputs(False, True)
        except BaseException:
            # No eligible output on a failed revoke/write/recheck. The official
            # failed-step gate also denies rich upload if output I/O was partial.
            return 1
        return 1
    return 0


def interrupted(signum, _frame):
    global PENDING_CANCELLATION
    if signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP) and PENDING_CANCELLATION is None:
        PENDING_CANCELLATION = signum


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--auxiliary", type=Path)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--bridge", type=Path)
    parser.add_argument("--serve", type=Path)
    parser.add_argument("--finalize", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, interrupted)
    try:
        if args.finalize:
            result = finalize(args.finalize.absolute())
        elif args.bridge:
            result = Bridge(args.bridge.absolute()).run()
        elif args.serve:
            serve(args.serve.absolute())
            result = 0
        else:
            require(args.source and args.auxiliary and args.root, "arguments_missing")
            result = Capture(args.source.absolute(), args.auxiliary.absolute(), args.root.absolute()).run()
        return 1 if cancellation_pending() else result
    except Exception as error:
        code = str(error) if isinstance(error, Refusal) else "boundary_failure"
        print(json.dumps({"status": "failure", "code": code}))
        return 1


if __name__ == "__main__":
    sys.exit(main())
