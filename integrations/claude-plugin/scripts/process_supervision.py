"""Bounded cleanup of normal POSIX children in an owned onboarding session.

The caller must launch with start_new_session=True and never poll/wait the
Popen before cleanup. WNOWAIT reserves the owner PID (and therefore its session
ID) until all cleanup signals have been sent. Children creating new sessions
are outside this contract. On systems without pidfds, membership rechecks
reduce PID reuse races; this is ordinary-child supervision, not an adversarial
process isolation boundary.
"""

import errno
import os
import selectors
import signal
import subprocess
import sys
import time

PS_ARGV = ("/bin/ps", "-e", "-o", "pid=")
DISCOVERY_TIMEOUT = 1.0
MAX_PID_BYTES = 1024 * 1024
MAX_PIDS = 65536
INTERRUPT_GRACE = 1.0
KILL_TIMEOUT = 1.0


class SupervisionError(ValueError):
    def __init__(self):
        super().__init__("process_supervision_unavailable")


def _pid_list():
    """Fixed system ps exposes only PIDs; neither argv nor stderr is captured."""
    process = None
    selector = selectors.DefaultSelector()
    deadline = time.monotonic() + DISCOVERY_TIMEOUT
    output = bytearray()
    try:
        process = subprocess.Popen(PS_ARGV, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
                                   shell=False, start_new_session=True, bufsize=0)
        os.set_blocking(process.stdout.fileno(), False)
        selector.register(process.stdout, selectors.EVENT_READ)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SupervisionError()
            if not selector.select(max(0, remaining)):
                continue
            block = os.read(process.stdout.fileno(), 4096)
            if not block:
                break
            if len(output) + len(block) > MAX_PID_BYTES:
                raise SupervisionError()
            output.extend(block)
        if process.wait(timeout=max(0, deadline - time.monotonic())) != 0:
            raise SupervisionError()
        lines = output.splitlines()
        if not lines or len(lines) > MAX_PIDS:
            raise SupervisionError()
        pids = []
        for line in lines:
            value = line.strip()
            if not value or not value.isdigit() or len(value) > 10:
                raise SupervisionError()
            pid = int(value)
            if not 0 < pid <= 2147483647:
                raise SupervisionError()
            pids.append(pid)
        if len(set(pids)) != len(pids):
            raise SupervisionError()
        return pids
    except (OSError, ValueError, subprocess.SubprocessError):
        raise SupervisionError() from None
    finally:
        selector.close()
        if process is not None:
            # This direct child has not been reaped on an error. Its PID cannot
            # be reused between kill and wait; ps itself launches no children.
            if process.returncode is None:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=KILL_TIMEOUT)
                except subprocess.TimeoutExpired:
                    raise SupervisionError() from None
            process.stdout.close()


def _owner_state(process):
    if process.returncode is not None or type(process.pid) is not int or process.pid <= 0:
        raise SupervisionError()
    try:
        state = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        exited = state is not None and state.si_pid == process.pid
        if not exited and (os.getsid(process.pid) != process.pid or
                           os.getpgid(process.pid) != process.pid):
            raise SupervisionError()
        # macOS getsid(zombie) returns ESRCH. The repeatable WNOWAIT result,
        # rather than a getsid call on that zombie, proves its PID is held.
        return exited
    except ProcessLookupError:
        # The owner can exit between the nonblocking waitid and getsid.
        try:
            state = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            # macOS may already hide a dying process from getsid before its
            # exit becomes waitable. A successful waitid still proves ownership.
            return state is not None and state.si_pid == process.pid
        except OSError:
            pass
        raise SupervisionError() from None
    except (OSError, AttributeError):
        raise SupervisionError() from None


def passive_exited(process):
    """Observe exit without reaping or changing Popen.returncode."""
    return _owner_state(process)


def _pidfd(pid):
    if not (hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal")):
        return None
    try:
        return os.pidfd_open(pid)
    except OSError as error:
        # Python can expose the API while an older kernel lacks the syscall.
        # No permission failure may silently downgrade supervision.
        if error.errno in (errno.ENOSYS, errno.EINVAL):
            return None
        raise


def _signal_members(process, signum):
    _owner_state(process)
    matched = set()
    for pid in _pid_list():
        pidfd = None
        try:
            if os.getsid(pid) != process.pid:
                continue
            pidfd = _pidfd(pid)
            group = os.getpgid(pid)
            if group <= 0 or os.getsid(pid) != process.pid or os.getpgid(pid) != group:
                continue
            if pidfd is not None:
                signal.pidfd_send_signal(pidfd, signum)
            else:
                os.kill(pid, signum)
            matched.add(pid)
        except ProcessLookupError:
            # A naturally exiting member is no longer a cleanup target.
            continue
        except OSError:
            raise SupervisionError() from None
        finally:
            if pidfd is not None:
                os.close(pidfd)
    return matched


def cleanup_and_reap(process):
    """Kill remaining owned-session members, then release the owner PID."""
    _owner_state(process)
    try:
        deadline = time.monotonic() + KILL_TIMEOUT
        previous = None
        while True:
            matched = _signal_members(process, signal.SIGKILL)
            # Linux retains getsid for zombies. Two fresh, stable scans after
            # owner exit prove every remaining discovered member was signaled;
            # orphan zombies need their actual parent's reaper, not more kills.
            if passive_exited(process) and matched == previous:
                break
            previous = matched
            if time.monotonic() >= deadline:
                raise SupervisionError()
            time.sleep(.01)
    finally:
        # Even discovery failure must stop the held owner's own group. Do not
        # report successful containment when other groups could not be checked.
        try:
            if not passive_exited(process):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except OSError:
                    raise SupervisionError() from None
        finally:
            try:
                process.wait(timeout=KILL_TIMEOUT)
            except (OSError, subprocess.SubprocessError):
                raise SupervisionError() from None
    return process.returncode


def interrupt_and_reap(process):
    """Give the owner and ordinary children SIGINT before forced cleanup."""
    _owner_state(process)
    try:
        _signal_members(process, signal.SIGINT)
        deadline = time.monotonic() + INTERRUPT_GRACE
        while not passive_exited(process) and time.monotonic() < deadline:
            time.sleep(.01)
    finally:
        cleanup_and_reap(process)
    return process.returncode


def check_support():
    """Probe PID discovery and WNOWAIT before the caller starts inference."""
    if os.name != "posix" or not all(hasattr(os, name) for name in
                                    ("waitid", "WNOWAIT", "WEXITED", "WNOHANG", "P_PID",
                                     "getsid", "getpgid", "set_blocking")):
        return False
    process = None
    try:
        pids = _pid_list()
        if os.getpid() not in pids:
            return False
        # Validate permission to query membership, not merely ps execution.
        for pid in pids:
            try:
                os.getsid(pid)
                os.getpgid(pid)
            except ProcessLookupError:
                continue
        # Signal zero checks the selected kernel primitive's permissions and
        # capability. The helper's own PID is stable and no signal is delivered.
        pidfd = _pidfd(os.getpid())
        try:
            if pidfd is not None:
                signal.pidfd_send_signal(pidfd, 0)
            else:
                os.kill(os.getpid(), 0)
        finally:
            if pidfd is not None:
                os.close(pidfd)
        process = subprocess.Popen([sys.executable, "-I", "-B", "-c", "pass"],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        deadline = time.monotonic() + DISCOVERY_TIMEOUT
        while not passive_exited(process):
            if time.monotonic() >= deadline:
                return False
            time.sleep(.01)
        if not passive_exited(process) or process.returncode is not None:
            return False
        process.wait(timeout=KILL_TIMEOUT)
        return True
    except (OSError, ValueError, subprocess.SubprocessError):
        return False
    finally:
        if process is not None and process.returncode is None:
            try:
                process.kill()
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=KILL_TIMEOUT)
            except subprocess.TimeoutExpired:
                pass
