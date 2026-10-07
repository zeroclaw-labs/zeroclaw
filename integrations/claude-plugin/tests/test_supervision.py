"""Disposable POSIX processes model native Code's separate group, same session."""

import importlib.util
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("process_supervision", ROOT / "scripts" / "process_supervision.py")
supervision = importlib.util.module_from_spec(spec)
spec.loader.exec_module(supervision)

CHILD = """
import os, signal, sys, time
from pathlib import Path
if os.getsid(0) != os.getpid():
    os.setpgid(0, 0)
signal.signal(signal.SIGINT, signal.SIG_IGN)
Path(sys.argv[1]).write_text(str(os.getpid()))
with open(sys.argv[2], 'ab', buffering=0) as heartbeat:
    while True:
        heartbeat.write(b'.')
        time.sleep(.02)
"""

OWNER = """
import os, signal, subprocess, sys, time
from pathlib import Path
signal.signal(signal.SIGINT, signal.SIG_IGN)
subprocess.Popen([sys.executable, '-I', '-B', '-c', sys.argv[1], sys.argv[2], sys.argv[3]],
                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
Path(sys.argv[4]).write_text(str(os.getpid()))
if len(sys.argv) > 6:
    subprocess.Popen([sys.executable, '-I', '-B', '-c', sys.argv[1], sys.argv[6], sys.argv[7]],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)
if sys.argv[5] == 'exit':
    time.sleep(.15)
    sys.exit(0)
while True:
    time.sleep(.02)
"""


@unittest.skipUnless(os.name == "posix", "POSIX disposable process fixtures")
class SupervisionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="zeroclaw-session-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.processes = []
        self.children = []
        self.addCleanup(self.stop_fixtures)

    def stop_fixtures(self):
        for child, session in self.children:
            try:
                if os.getsid(child) == session:
                    os.kill(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
        for process in self.processes:
            if process.returncode is None:
                try:
                    os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    process.kill()
                except (ProcessLookupError, ChildProcessError):
                    pass
                process.wait(timeout=2)

    def wait_for(self, condition, timeout=3):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if condition():
                return
            time.sleep(.01)
        self.fail("disposable fixture did not reach its expected state")

    def owner(self, exits=False, detached=False):
        child_pid = self.base / "child.pid"
        heartbeat = self.base / "heartbeat"
        owner_pid = self.base / "owner.pid"
        argv = [sys.executable, "-I", "-B", "-c", OWNER, CHILD,
                str(child_pid), str(heartbeat), str(owner_pid), "exit" if exits else "hold"]
        if detached:
            argv.extend([str(self.base / "detached.pid"), str(self.base / "detached.heartbeat")])
        process = subprocess.Popen(argv,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        self.processes.append(process)
        self.wait_for(lambda: child_pid.exists() and heartbeat.exists() and heartbeat.stat().st_size > 1)
        child = int(child_pid.read_text())
        self.children.append((child, process.pid))
        self.assertEqual(os.getsid(child), process.pid)
        self.assertEqual(os.getpgid(process.pid), process.pid)
        self.assertEqual(os.getpgid(child), child)
        return process, child, heartbeat

    def assert_heartbeat_stopped(self, heartbeat):
        size = heartbeat.stat().st_size
        time.sleep(.15)
        self.assertEqual(heartbeat.stat().st_size, size, "native child kept running after owner cleanup")

    def test_interrupt_stops_child_in_other_group_of_owned_session(self):
        process, child, heartbeat = self.owner()
        started = time.monotonic()
        supervision.interrupt_and_reap(process)
        self.assertLess(time.monotonic() - started, 4)
        self.assertIsNotNone(process.returncode)
        self.assert_heartbeat_stopped(heartbeat)

    def test_forced_cleanup_stops_other_group_and_holds_pid_until_last_scan(self):
        process, child, heartbeat = self.owner()
        observed = []
        original = supervision._pid_list

        def discovery():
            observed.append(process.returncode)
            # No poll/wait call has released the kernel's child ownership.
            os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            return original()

        with patch.object(supervision, "_pid_list", side_effect=discovery):
            self.assertEqual(supervision.cleanup_and_reap(process), -signal.SIGKILL)
        self.assertGreaterEqual(len(observed), 2)
        self.assertEqual(set(observed), {None})
        self.assert_heartbeat_stopped(heartbeat)

    def test_passive_exit_does_not_reap_and_success_cleanup_preserves_exit_code(self):
        process, child, heartbeat = self.owner(exits=True)
        with patch.object(process, "poll", side_effect=AssertionError("must not poll")), \
                patch.object(process, "wait", side_effect=AssertionError("must not reap yet")):
            self.wait_for(lambda: supervision.passive_exited(process))
            self.assertTrue(supervision.passive_exited(process))
            self.assertIsNone(process.returncode)
            self.assertEqual(os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT).si_pid,
                             process.pid)
        with patch.object(supervision, "_pid_list", wraps=supervision._pid_list) as discover:
            self.assertEqual(supervision.cleanup_and_reap(process), 0)
            self.assertGreaterEqual(discover.call_count, 2)
        self.assert_heartbeat_stopped(heartbeat)

    def test_detached_and_unrelated_control_sessions_keep_running(self):
        control_pid = self.base / "control.pid"
        control_heartbeat = self.base / "control.heartbeat"
        control = subprocess.Popen([sys.executable, "-I", "-B", "-c", CHILD,
                                    str(control_pid), str(control_heartbeat)],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        self.processes.append(control)
        process, child, heartbeat = self.owner(detached=True)
        detached_pid = self.base / "detached.pid"
        detached_heartbeat = self.base / "detached.heartbeat"
        self.wait_for(lambda: detached_pid.exists() and detached_heartbeat.exists() and control_heartbeat.exists())
        detached = int(detached_pid.read_text())
        self.children.append((detached, detached))
        self.assertNotEqual(os.getsid(detached), process.pid)
        supervision.interrupt_and_reap(process)
        self.assert_heartbeat_stopped(heartbeat)
        for alive in (detached_heartbeat, control_heartbeat):
            before = alive.stat().st_size
            time.sleep(.1)
            self.assertGreater(alive.stat().st_size, before)
        self.assertIsNone(control.returncode)

    def test_reaped_owner_is_rejected_before_any_signal_or_discovery(self):
        process = subprocess.Popen([sys.executable, "-I", "-B", "-c", "pass"], start_new_session=True)
        self.processes.append(process)
        process.wait(timeout=2)
        with patch.object(supervision.os, "kill") as kill, \
                patch.object(supervision.os, "killpg") as killpg, \
                patch.object(supervision, "_pid_list") as discover, \
                patch.object(supervision.os, "waitid", wraps=os.waitid) as observe:
            for operation in (supervision.passive_exited, supervision.cleanup_and_reap,
                              supervision.interrupt_and_reap):
                with self.assertRaises(supervision.SupervisionError):
                    operation(process)
            kill.assert_not_called()
            killpg.assert_not_called()
            discover.assert_not_called()
            observe.assert_not_called()

    def test_running_child_in_callers_session_is_rejected(self):
        process = subprocess.Popen([sys.executable, "-I", "-B", "-c", "import time; time.sleep(10)"])
        self.processes.append(process)
        with patch.object(supervision.os, "kill") as kill, \
                patch.object(supervision.os, "killpg") as killpg:
            with self.assertRaises(supervision.SupervisionError):
                supervision.cleanup_and_reap(process)
            kill.assert_not_called()
            killpg.assert_not_called()

    def test_runtime_discovery_failure_is_not_reported_as_containment_success(self):
        process, child, heartbeat = self.owner()
        with patch.object(supervision, "_pid_list", side_effect=supervision.SupervisionError()):
            with self.assertRaisesRegex(supervision.SupervisionError, "^process_supervision_unavailable$"):
                supervision.cleanup_and_reap(process)
        self.assertIsNotNone(process.returncode)


class DiscoveryTests(unittest.TestCase):
    def producer(self, program):
        return patch.object(supervision, "PS_ARGV", (sys.executable, "-I", "-B", "-c", program))

    def test_discovery_uses_fixed_pid_only_command_and_sanitized_environment(self):
        with patch.object(supervision.subprocess, "Popen", wraps=subprocess.Popen) as launch:
            pids = supervision._pid_list()
        args, kwargs = launch.call_args
        self.assertEqual(args, (("/bin/ps", "-e", "-o", "pid="),))
        self.assertEqual(kwargs["env"], {"PATH": "/usr/bin:/bin", "LC_ALL": "C"})
        self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
        self.assertFalse(kwargs["shell"])
        self.assertIn(os.getpid(), pids)

    def test_discovery_rejects_malformed_duplicate_out_of_range_and_excessive_pids(self):
        for output in ("", "+1\n", "1\nprivate-argv-sentinel\n", "0\n", "2147483648\n", "1\n1\n"):
            with self.subTest(output=output), self.producer("print(" + repr(output) + ", end='')"):
                with self.assertRaisesRegex(supervision.SupervisionError, "^process_supervision_unavailable$"):
                    supervision._pid_list()
        with self.producer("print('1\\n2\\n3')"), patch.object(supervision, "MAX_PIDS", 2):
            with self.assertRaises(supervision.SupervisionError):
                supervision._pid_list()

    def test_discovery_bounds_bytes_and_runtime_and_reaps_auxiliary_child(self):
        for program in ("print('\\n'.join(map(str, range(1, 501))))", "import time; time.sleep(3)"):
            children = []
            launch_process = subprocess.Popen

            def capture_child(*args, **kwargs):
                child = launch_process(*args, **kwargs)
                children.append(child)
                return child

            with self.subTest(program=program), self.producer(program), \
                    patch.object(supervision, "MAX_PID_BYTES", 1024), \
                    patch.object(supervision, "DISCOVERY_TIMEOUT", .2), \
                    patch.object(supervision.subprocess, "Popen", side_effect=capture_child):
                started = time.monotonic()
                with self.assertRaises(supervision.SupervisionError):
                    supervision._pid_list()
                self.assertLess(time.monotonic() - started, 2)
                self.assertEqual(len(children), 1)
                self.assertIsNotNone(children[0].returncode)
                with self.assertRaises(ChildProcessError):
                    os.waitid(os.P_PID, children[0].pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)

    def test_discovery_nonzero_and_permission_failure_do_not_expose_diagnostics(self):
        with self.producer("import sys; print('1'); sys.stderr.write('private-sentinel'); sys.exit(1)"):
            with self.assertRaisesRegex(supervision.SupervisionError, "^process_supervision_unavailable$"):
                supervision._pid_list()
        with patch.object(supervision.subprocess, "Popen", side_effect=PermissionError("private-sentinel")):
            with self.assertRaisesRegex(supervision.SupervisionError, "^process_supervision_unavailable$"):
                supervision._pid_list()

    def test_support_probe_requires_pid_discovery_membership_permission_and_waitnowait(self):
        self.assertTrue(supervision.check_support())
        with patch.object(supervision, "_pid_list", side_effect=supervision.SupervisionError()):
            self.assertFalse(supervision.check_support())
        with patch.object(supervision, "_pid_list", return_value=[]):
            self.assertFalse(supervision.check_support())
        with patch.object(supervision, "_pid_list", return_value=[os.getpid()]), \
                patch.object(supervision.os, "getsid", side_effect=PermissionError("private-sentinel")):
            self.assertFalse(supervision.check_support())
        with patch.object(supervision.os, "waitid", side_effect=OSError("private-sentinel")):
            self.assertFalse(supervision.check_support())
        with patch.object(supervision.os, "name", "nt"):
            self.assertFalse(supervision.check_support())

    def test_membership_change_between_checks_prevents_signal(self):
        process = type("Owner", (), {"pid": 100, "returncode": None})()
        for sessions, groups in (([100, 200], [101]), ([100, 100], [101, 201]), ([100], [0])):
            with self.subTest(sessions=sessions), \
                    patch.object(supervision, "_owner_state", return_value=False), \
                    patch.object(supervision, "_pid_list", return_value=[101]), \
                    patch.object(supervision.os, "getsid", side_effect=sessions), \
                    patch.object(supervision.os, "getpgid", side_effect=groups), \
                    patch.object(supervision.os, "pidfd_open", return_value=999, create=True), \
                    patch.object(supervision.os, "close") as close, \
                    patch.object(supervision.signal, "pidfd_send_signal", create=True) as pidfd_signal, \
                    patch.object(supervision.os, "kill") as kill:
                self.assertEqual(supervision._signal_members(process, signal.SIGKILL), set())
                kill.assert_not_called()
                pidfd_signal.assert_not_called()
                close.assert_called_once_with(999)

    def test_foreign_session_is_skipped_before_pidfd_open_or_signal(self):
        process = type("Owner", (), {"pid": 100, "returncode": None})()
        with patch.object(supervision, "_owner_state", return_value=False), \
                patch.object(supervision, "_pid_list", return_value=[101]), \
                patch.object(supervision.os, "getsid", return_value=200), \
                patch.object(supervision.os, "getpgid", return_value=101) as group, \
                patch.object(supervision.os, "pidfd_open", return_value=999, create=True) as pidfd_open, \
                patch.object(supervision.os, "close"), \
                patch.object(supervision.signal, "pidfd_send_signal", create=True) as pidfd_signal, \
                patch.object(supervision.os, "kill") as kill:
            self.assertEqual(supervision._signal_members(process, signal.SIGKILL), set())
            pidfd_open.assert_not_called()
            pidfd_signal.assert_not_called()
            kill.assert_not_called()
            group.assert_not_called()

    def test_pidfd_signal_targets_opened_process_and_closes_descriptor(self):
        process = type("Owner", (), {"pid": 100, "returncode": None})()
        with patch.object(supervision, "_owner_state", return_value=False), \
                patch.object(supervision, "_pid_list", return_value=[101]), \
                patch.object(supervision.os, "getsid", return_value=100), \
                patch.object(supervision.os, "getpgid", return_value=101), \
                patch.object(supervision.os, "pidfd_open", return_value=999, create=True) as pidfd_open, \
                patch.object(supervision.os, "close") as close, \
                patch.object(supervision.signal, "pidfd_send_signal", create=True) as pidfd_signal, \
                patch.object(supervision.os, "kill") as kill:
            self.assertEqual(supervision._signal_members(process, signal.SIGKILL), {101})
            pidfd_open.assert_called_once_with(101)
            pidfd_signal.assert_called_once_with(999, signal.SIGKILL)
            close.assert_called_once_with(999)
            kill.assert_not_called()

    def test_unsupported_kernel_pidfd_uses_immediate_membership_recheck_fallback(self):
        process = type("Owner", (), {"pid": 100, "returncode": None})()
        for unsupported in (errno.ENOSYS, errno.EINVAL):
            with self.subTest(errno=unsupported), \
                    patch.object(supervision, "_owner_state", return_value=False), \
                    patch.object(supervision, "_pid_list", return_value=[101]), \
                    patch.object(supervision.os, "getsid", return_value=100), \
                    patch.object(supervision.os, "getpgid", return_value=101), \
                    patch.object(supervision.os, "pidfd_open", side_effect=OSError(unsupported, "unsupported"), create=True), \
                    patch.object(supervision.signal, "pidfd_send_signal", create=True) as pidfd_signal, \
                    patch.object(supervision.os, "kill") as kill:
                self.assertEqual(supervision._signal_members(process, signal.SIGKILL), {101})
                kill.assert_called_once_with(101, signal.SIGKILL)
                pidfd_signal.assert_not_called()

    def test_support_rejects_pidfd_signal_permission_before_probe_child_launch(self):
        real_close = os.close
        with patch.object(supervision, "_pid_list", return_value=[os.getpid()]), \
                patch.object(supervision.os, "pidfd_open", return_value=999, create=True), \
                patch.object(supervision.os, "close", side_effect=lambda fd: None if fd == 999 else real_close(fd)) as close, \
                patch.object(supervision.signal, "pidfd_send_signal", side_effect=PermissionError(), create=True), \
                patch.object(supervision.subprocess, "Popen", wraps=subprocess.Popen) as launch:
            self.assertFalse(supervision.check_support())
            launch.assert_not_called()
            close.assert_called_once_with(999)

    def test_support_rejects_plain_signal_permission_before_probe_child_launch(self):
        with patch.object(supervision, "_pid_list", return_value=[os.getpid()]), \
                patch.object(supervision.os, "pidfd_open", side_effect=OSError(errno.ENOSYS, "unsupported"), create=True), \
                patch.object(supervision.signal, "pidfd_send_signal", create=True), \
                patch.object(supervision.os, "kill", side_effect=PermissionError()), \
                patch.object(supervision.subprocess, "Popen", wraps=subprocess.Popen) as launch:
            self.assertFalse(supervision.check_support())
            launch.assert_not_called()

    def test_pidfd_permission_is_never_downgraded_to_plain_signaling(self):
        process = type("Owner", (), {"pid": 100, "returncode": None})()
        with patch.object(supervision, "_owner_state", return_value=False), \
                patch.object(supervision, "_pid_list", return_value=[101]), \
                patch.object(supervision.os, "getsid", return_value=100), \
                patch.object(supervision.os, "pidfd_open", side_effect=PermissionError(errno.EPERM, "denied"), create=True), \
                patch.object(supervision.signal, "pidfd_send_signal", create=True), \
                patch.object(supervision.os, "kill") as kill:
            with self.assertRaises(supervision.SupervisionError):
                supervision._signal_members(process, signal.SIGKILL)
            kill.assert_not_called()
        with patch.object(supervision, "_pid_list", return_value=[os.getpid()]), \
                patch.object(supervision.os, "pidfd_open", side_effect=PermissionError(errno.EPERM, "denied"), create=True), \
                patch.object(supervision.signal, "pidfd_send_signal", create=True), \
                patch.object(supervision.subprocess, "Popen", wraps=subprocess.Popen) as launch:
            self.assertFalse(supervision.check_support())
            launch.assert_not_called()

    def test_cleanup_deadline_reports_failure_and_still_reaps_held_owner(self):
        process = Mock(pid=100, returncode=None)
        with patch.object(supervision, "_owner_state", return_value=True), \
                patch.object(supervision, "passive_exited", return_value=True), \
                patch.object(supervision, "_signal_members", side_effect=[{1}, {2}, {3}, {4}]), \
                patch.object(supervision.time, "monotonic", side_effect=[0, .25, .5, 1.1, 2]), \
                patch.object(supervision.time, "sleep"):
            with self.assertRaisesRegex(supervision.SupervisionError, "^process_supervision_unavailable$"):
                supervision.cleanup_and_reap(process)
            process.wait.assert_called_once_with(timeout=supervision.KILL_TIMEOUT)

    def test_interrupt_grace_is_bounded_and_always_finishes_with_cleanup(self):
        process = Mock(pid=100, returncode=None)
        with patch.object(supervision, "_owner_state", return_value=False), \
                patch.object(supervision, "passive_exited", side_effect=[False, False, False, AssertionError("unbounded grace")]), \
                patch.object(supervision, "_signal_members", return_value={100}) as interrupt, \
                patch.object(supervision, "cleanup_and_reap") as cleanup, \
                patch.object(supervision.time, "monotonic", side_effect=[0, .25, .5, 1.1]), \
                patch.object(supervision.time, "sleep"):
            supervision.interrupt_and_reap(process)
            interrupt.assert_called_once_with(process, signal.SIGINT)
            cleanup.assert_called_once_with(process)

    def test_owner_exit_between_waitid_and_membership_query_stays_unreaped(self):
        process = type("Owner", (), {"pid": 100, "returncode": None})()
        with patch.object(supervision.os, "waitid", side_effect=[None, None]), \
                patch.object(supervision.os, "getsid", side_effect=ProcessLookupError()):
            self.assertFalse(supervision.passive_exited(process))
        self.assertIsNone(process.returncode)


if __name__ == "__main__":
    unittest.main()
