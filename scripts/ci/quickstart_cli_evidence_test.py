#!/usr/bin/env python3
"""Focused capture-contract controls; they do not produce CLI visual evidence."""

import copy
import contextlib
import importlib.util
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "quickstart_cli_evidence", Path(__file__).with_name("quickstart_cli_evidence.py"))
evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evidence
SPEC.loader.exec_module(evidence)


def binding():
    return {"run_id": "123", "attempt": "1", "job": "evidence", "auxiliary_sha": "b" * 40,
            "source_sha": evidence.SOURCE_SHA, "source_tree": evidence.SOURCE_TREE,
            "case": "A", "window": 42, "window_pixels": [800, 480],
            "xterm_pid": 301, "binary_sha256": "c" * 64}


def admitted_fixture():
    config = {"plugins": {"enabled": True, "auto_discover": True, "entries": [
        {"name": "tool:fixture-key", "config": {"mode": "", "api_token": "enc2:fake"},
         "egress": {"hosts": []}}]}}
    text = ("Quickstart complete.\n" + evidence.PACKAGE
            + ": admitted as a tool (config entry tool:fixture-key). " + evidence.CAVEAT)
    return config, text, ["required-mode-enter"]


def cleanup_receipt(pid=302, parent=303, role="cli"):
    return {"pid": pid, "role": role, "identity_verified": True, "ownership_retired": True,
            "direct_waits": 1, "direct_reaped": True, "group_absent": True, "failure": None,
            "identity": {"pid": pid, "parent_pid": parent, "pgid": pid,
                         "session": pid, "start_ticks": 17}}


def owned_child():
    proc = mock.Mock(pid=54321, returncode=None, stdout=None)
    child = evidence.OwnedChild(proc.pid, "command", proc)
    child.parent = 111
    child.identity = {"pid": proc.pid, "parent_pid": 111, "pgid": proc.pid,
                      "session": proc.pid, "start_ticks": 17}
    return child


def child_mocks(child, exited=True):
    stack = contextlib.ExitStack()
    result = types.SimpleNamespace(si_pid=child.pid, si_code=os.CLD_EXITED, si_status=0)
    stack.enter_context(mock.patch.object(evidence.os, "waitid", return_value=result if exited else None))
    stack.enter_context(mock.patch.object(evidence, "process_identity",
                                         return_value={**child.identity, "state": "Z" if exited else "S"}))
    stack.enter_context(mock.patch.object(evidence.signal, "getsignal", return_value=evidence.signal.SIG_DFL))
    stack.enter_context(mock.patch.object(evidence, "group_exists", return_value=False))
    return stack


def compiler_outputs(private, linked=False):
    """Benign unit bytes, not compiler/native/Wasm execution or provenance proof."""
    values = ((evidence.CLI_ARTIFACT, b"fake contract binary", 0o700),
              (evidence.COMPONENT_ARTIFACT, b"fake contract component", 0o600))
    for relative, data, mode in values:
        path = private.joinpath(*relative)
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        evidence.private_write(path, data)
        path.chmod(mode)
        if linked:
            os.link(path, path.with_name(path.name + ".deps-alias"))


def export_fixture(root):
    """Synthetic contract data only; not pixels, CLI, native process or guest proof."""
    context = {k: v for k, v in binding().items() if k in
               ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree")}
    capture = object.__new__(evidence.Capture)
    capture.root, capture.context = root, context
    capture.private, capture.export = root / "private", root / "export"
    capture.staging = root / "export-staging-private"
    capture.private.mkdir(mode=0o700)
    capture.tokens, capture.failure = ["synthetic-export-secret"], None
    capture.slices, capture.started_cases = ["A: synthetic admitted output"], []
    capture.manager = mock.Mock()
    capture.manager.deadline = evidence.time.monotonic() + 60
    capture.manager.cleanup.return_value = []
    capture.setup, capture.build, capture.case = mock.Mock(), mock.Mock(), mock.Mock()
    capture.source = capture.auxiliary = root / "unused-source"
    capture.source_snapshot = capture.aux_snapshot = "a" * 64
    compiler_outputs(capture.private)
    (capture.private / "materialized").mkdir(mode=0o700)
    capture.compiled_artifacts = {
        "binary": evidence.materialize_compiler_artifact(
            capture.private, evidence.CLI_ARTIFACT, capture.manager.deadline),
        "component": evidence.materialize_compiler_artifact(
            capture.private, evidence.COMPONENT_ARTIFACT, capture.manager.deadline)}
    capture.binary = evidence.materialized_artifact_path(capture.private, evidence.CLI_ARTIFACT)
    capture.wasm_path = evidence.materialized_artifact_path(capture.private, evidence.COMPONENT_ARTIFACT)
    capture.wasm = b"fake contract component"
    capture.binary_hash = evidence.sha(capture.binary.read_bytes())
    capture.provenance = {**context, "execution_started": True,
                          "source_before": "a" * 64, "source_after": "a" * 64,
                          "auxiliary_before": "a" * 64, "auxiliary_after": "a" * 64,
                          "binary_sha256": capture.binary_hash, "binary_sha256_after": capture.binary_hash,
                          "component_sha256": evidence.sha(capture.wasm),
                          "component_sha256_after": evidence.sha(capture.wasm)}
    png = b"\x89PNG\r\n\x1a\n" + b"\0\0\0\rIHDR" + struct.pack(">II", 800, 480)
    capture.frames = []
    for name in evidence.SCREENS:
        record = {**binding(), "case": name[0], "binary_sha256": capture.binary_hash}
        evidence.private_write(capture.private / (name + ".capture"), png)
        capture.frames.append({"name": name, "binding": record, "pixels": evidence.validate_png(png),
                               "capture": "actual-owned-xterm-window", "edited": False})
    capture.cases = [{"binding": {**binding(), "case": case, "binary_sha256": capture.binary_hash}}
                     for case in evidence.CASES]
    capture.actions = [{"case": case, "binding": c["binding"], "events": []}
                       for case, c in zip(evidence.CASES, capture.cases)]
    evidence.write_json(root / "state.json", {"started": True})
    return capture


def evidence_environment(root, outcome="success"):
    return {"RUNNER_TEMP": str(root.parent), "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_JOB": "evidence", "GITHUB_WORKFLOW_SHA": "b" * 40,
            "EVIDENCE_CAPTURE_OUTCOME": outcome}


@contextlib.contextmanager
def bridge_control(directory):
    """Actual Bridge.run caller with OS/PTY/process operations controlled, not run."""
    with contextlib.ExitStack() as stack:
        bridge = object.__new__(evidence.Bridge)
        bridge.directory = directory
        bridge.deadline = evidence.time.monotonic() + 10
        bridge.guard = evidence.OutputGuard("synthetic-secret")
        bridge.events, bridge.master, bridge.pid = [], None, None
        bridge.child, bridge.status, bridge.binding = None, None, None
        raw = b"fake contract binary"
        wanted = {**binding(), "binary_sha256": evidence.sha(raw)}
        bridge.spec = {"binding": wanted, "binary": "unused", "config": str(directory),
                       "argv": [], "environment": {}}
        bridge.drive, bridge.pump = mock.Mock(), mock.Mock(return_value=False)
        terminal = {"isatty": [True] * 3, "rows": 24, "columns": 80}
        for name, value in [("terminal_info", terminal), ("wait_file", {"window": 42,
                            "xterm_pid": 301, "window_pixels": [800, 480]}), ("bounded_read", raw)]:
            stack.enter_context(mock.patch.object(evidence, name, return_value=value))
        stack.enter_context(mock.patch.object(evidence.termios, "tcgetattr", return_value=[]))
        restore = stack.enter_context(mock.patch.object(evidence.termios, "tcsetattr"))
        stack.enter_context(mock.patch.object(evidence.tty, "setraw"))
        flags = stack.enter_context(mock.patch.object(evidence.fcntl, "fcntl", return_value=0))
        stack.enter_context(mock.patch.object(evidence.fcntl, "ioctl"))
        stack.enter_context(mock.patch.object(evidence.os, "pipe", return_value=(10010, 10011)))
        stack.enter_context(mock.patch.object(evidence.pty, "fork", return_value=(54321, 10012)))
        close = stack.enter_context(mock.patch.object(evidence.os, "close"))
        stack.enter_context(mock.patch.object(evidence.os, "set_blocking"))
        stack.enter_context(mock.patch.object(evidence.os, "write", return_value=1))
        stack.enter_context(mock.patch.object(evidence.signal, "getsignal", return_value=evidence.signal.SIG_DFL))
        stack.enter_context(mock.patch.object(evidence.os, "waitid", return_value=types.SimpleNamespace(
            si_pid=54321, si_code=os.CLD_EXITED, si_status=0)))
        stack.enter_context(mock.patch.object(evidence.os, "waitpid", return_value=(54321, 0)))
        stack.enter_context(mock.patch.object(evidence.os, "killpg"))
        stack.enter_context(mock.patch.object(evidence, "group_exists", return_value=False))
        identities = {}
        def bind_child(child, deadline):
            child.identity = {"pid": child.pid, "parent_pid": child.parent,
                              "pgid": child.pid, "session": child.pid, "start_ticks": 17}
            identities[child.pid] = {**child.identity, "state": "Z"}
            return child
        stack.enter_context(mock.patch.object(evidence.OwnedChild, "bind", autospec=True, side_effect=bind_child))
        stack.enter_context(mock.patch.object(evidence, "process_identity", side_effect=lambda pid: identities[pid]))
        yield bridge, {"close": close, "restore": restore, "flags": flags}


@contextlib.contextmanager
def capture_case_wait_control(directory):
    """Actual Capture.case wait with controlled OS/validation boundaries, not capture proof."""
    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch.object(evidence, "PENDING_CANCELLATION", None))
        capture = object.__new__(evidence.Capture)
        capture.private = directory / "private"
        capture.private.mkdir(mode=0o700)
        capture.context = {key: value for key, value in binding().items() if key in
                           ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree")}
        capture.tokens, capture.started_cases, capture.actions = [], [], []
        capture.slices, capture.cases = [], []
        capture.binary, capture.wasm = directory / "unused-binary", b"synthetic-unit-component"
        capture.binary_hash = evidence.sha(b"synthetic-unit-binary")
        capture.display, capture.gui_env = ":controlled-unit", {}
        capture.capture_frame = mock.Mock()
        server, terminal = mock.Mock(pid=302), mock.Mock(pid=301)
        server.poll.return_value = terminal.poll.return_value = None
        capture.manager = mock.Mock(deadline=1000)
        capture.manager.spawn.side_effect = [server, terminal]
        capture.manager.run.side_effect = [b"42\n", b"301\n", b"WIDTH=800\nHEIGHT=480\n"]
        state = types.SimpleNamespace(clock=100.0, on_window=None, on_sleep=None, events=[])
        raw = b"synthetic case wait\n"
        result = {"cli_pid": 304, "parent_pid": 305, "observed_sha256": evidence.sha(raw),
                  "events": [], "exit": 0, "cleanup": cleanup_receipt(304, 305)}
        values = {"bridge-result.json": result,
                  "child-terminal.json": {"pid": 304, "parent_pid": 305},
                  "server-counts.json": {"index": 1, "archive": 1, "unexpected": 0}}
        def read(path):
            state.events.append("read-" + path.name)
            return copy.deepcopy(values[path.name])
        real_write = evidence.write_json
        def write(path, *args, **kwargs):
            value = real_write(path, *args, **kwargs)
            if path.name == "window.json":
                state.events.append("window")
                evidence.private_write(path.parent / "output.private", raw)
                if state.on_window is not None:
                    state.on_window(path.parent)
            return value
        def sleep(seconds):
            state.events.append("sleep")
            if state.on_sleep is None:
                raise AssertionError("unexpected wait without a controlled disposition")
            state.on_sleep(capture.private / "C")
        stack.enter_context(mock.patch.object(evidence.time, "monotonic", side_effect=lambda: state.clock))
        stack.enter_context(mock.patch.object(evidence.time, "sleep", side_effect=sleep))
        stack.enter_context(mock.patch.object(evidence, "write_json", side_effect=write))
        state.read = stack.enter_context(mock.patch.object(evidence, "read_json", side_effect=read))
        stack.enter_context(mock.patch.object(evidence, "wait_file", return_value={
            "pid": 302, "port": 12345, "index_sha256": "d" * 64}))
        state.bridge = stack.enter_context(mock.patch.object(evidence, "validate_bridge"))
        stack.enter_context(mock.patch.object(evidence, "validate_terminal"))
        stack.enter_context(mock.patch.object(evidence, "validate_case", return_value={"unit_wait_only": True}))
        state.stop = stack.enter_context(mock.patch.object(evidence, "stop_process", side_effect=lambda child:
            cleanup_receipt(child.pid, 111, "registry" if child is server else "terminal")))
        yield capture, state, server, terminal


class CaptureControls(unittest.TestCase):
    def setUp(self):
        pending = mock.patch.object(evidence, "PENDING_CANCELLATION", None)
        pending.start()
        self.addCleanup(pending.stop)

    def test_capture_case_no_frame_wait_observes_cancellation_before_timeout(self):
        with tempfile.TemporaryDirectory() as directory, capture_case_wait_control(Path(directory)) as controlled:
            capture, state, _server, _terminal = controlled
            def cancel(_case_directory):
                evidence.interrupted(evidence.signal.SIGTERM, None)
                state.clock = 221.0  # Old loop reaches case_timeout instead of observing cancellation.
            state.on_sleep = cancel
            with self.assertRaisesRegex(evidence.Refusal, "^interrupted$"):
                capture.case("C")
            self.assertEqual(state.events, ["window", "sleep"])
            state.read.assert_not_called()
            state.bridge.assert_not_called()
            capture.capture_frame.assert_not_called()
            self.assertEqual(capture.cases, [])

    def test_capture_case_ready_never_masks_pending_cancellation(self):
        for ready_at_window in (True, False):
            with self.subTest(ready_at_window=ready_at_window), tempfile.TemporaryDirectory() as directory, \
                    capture_case_wait_control(Path(directory)) as controlled:
                capture, state, _server, _terminal = controlled
                def ready_and_cancel(case_directory):
                    evidence.private_write(case_directory / "bridge-result.json.ready", b"")
                    evidence.interrupted(evidence.signal.SIGINT, None)
                if ready_at_window:
                    state.on_window = ready_and_cancel
                else:
                    state.on_sleep = ready_and_cancel
                with self.assertRaisesRegex(evidence.Refusal, "^interrupted$"):
                    capture.case("C")
                self.assertEqual(state.events, ["window"] if ready_at_window else ["window", "sleep"])
                state.read.assert_not_called()
                state.bridge.assert_not_called()
                self.assertEqual(capture.cases, [])

    def test_capture_case_ready_without_cancellation_consumes_result(self):
        for ready_at_window in (True, False):
            with self.subTest(ready_at_window=ready_at_window), tempfile.TemporaryDirectory() as directory, \
                    capture_case_wait_control(Path(directory)) as controlled:
                capture, state, server, terminal = controlled
                def ready(case_directory):
                    evidence.private_write(case_directory / "bridge-result.json.ready", b"")
                if ready_at_window:
                    state.on_window = ready
                else:
                    state.on_sleep = ready
                self.assertIsNone(capture.case("C"))
                self.assertEqual(state.events[-3:], ["read-bridge-result.json", "read-child-terminal.json",
                                                     "read-server-counts.json"])
                state.bridge.assert_called_once()
                self.assertEqual(len(capture.cases), 1)
                self.assertEqual(capture.cases[0]["binding"]["case"], "C")
                self.assertEqual(state.stop.call_args_list, [mock.call(server), mock.call(terminal)])

    def test_exact_selected_empty_value_is_the_only_positive_mode(self):
        config, text, events = admitted_fixture()
        result = evidence.validate_case("A", config, text, events)
        self.assertTrue(result["mode_present"])
        self.assertEqual(result["mode_utf8_bytes"], 0)
        for value in ([], [""], "auto", None, False, "<empty>", " "):
            with self.subTest(value=value):
                broken = copy.deepcopy(config)
                broken["plugins"]["entries"][0]["config"]["mode"] = value
                with self.assertRaisesRegex(evidence.Refusal, "mode_not_selected"):
                    evidence.validate_case("A", broken, text, events)
        absent = copy.deepcopy(config)
        del absent["plugins"]["entries"][0]["config"]["mode"]
        with self.assertRaises(evidence.Refusal):
            evidence.validate_case("A", absent, text, events)

    def test_no_settings_route_cannot_be_laundered_by_good_disk_state(self):
        config, text, events = admitted_fixture()
        with self.assertRaisesRegex(evidence.Refusal, "settings_fallback"):
            evidence.validate_case("A", config, text + "\nInstall tool-fixture without these settings?", events)
        for wrong in ([], ["skip"], ["required-mode-enter", "required-mode-escape"]):
            with self.assertRaises(evidence.Refusal):
                evidence.validate_case("A", config, text, wrong)
        with self.assertRaisesRegex(evidence.Refusal, "validation_failed"):
            evidence.validate_case("A", config, text + "These settings do not satisfy the schema of tool-fixture:", events)

    def test_obsolete_active_or_missing_registry_caveat_refuses(self):
        config, text, events = admitted_fixture()
        for broken in (text.replace("admitted as", "active as"), text.replace(evidence.CAVEAT, "")):
            with self.assertRaises(evidence.Refusal):
                evidence.validate_case("A", config, broken, events)

    def test_readiness_must_name_the_actual_single_disk_row(self):
        config, text, events = admitted_fixture()
        for change in ("different-key", None):
            broken = copy.deepcopy(config)
            broken["plugins"]["entries"][0]["name"] = change
            with self.assertRaises(evidence.Refusal):
                evidence.validate_case("A", broken, text, events)
        broken = copy.deepcopy(config)
        broken["plugins"]["entries"].append(copy.deepcopy(broken["plugins"]["entries"][0]))
        with self.assertRaises(evidence.Refusal):
            evidence.validate_case("A", broken, text, events)

    def test_plain_secret_or_new_network_grant_refuses(self):
        config, text, events = admitted_fixture()
        for property_name, replacement in (("config", {"mode": "", "api_token": "plaintext"}),
                                           ("egress", {"hosts": ["api.example.com"]})):
            broken = copy.deepcopy(config)
            broken["plugins"]["entries"][0][property_name] = replacement
            with self.assertRaises(evidence.Refusal):
                evidence.validate_case("A", broken, text, events)

    def test_stateless_is_distinct_from_a_missing_configured_row(self):
        text = ("Quickstart complete.\ntool-fixture: admitted as a tool. It has nothing to configure, "
                "so it needs no config entry. " + evidence.CAVEAT)
        config = {"plugins": {"enabled": True, "auto_discover": True}}
        self.assertTrue(evidence.validate_case("B", config, text, [])["no_entry_needed"])
        with self.assertRaises(evidence.Refusal):
            evidence.validate_case("A", config, text, ["required-mode-enter"])

    def test_escape_refuses_publication_and_activation(self):
        config = {"plugins": {"enabled": False, "auto_discover": False}}
        text = "Configuration of tool-fixture was cancelled."
        result = evidence.validate_case("C", config, text, ["required-mode-escape"])
        self.assertFalse(result["configured"])
        for broken in ({"plugins": {"enabled": True}},
                       {"plugins": {"entries": [{"config": {"mode": ""}}]}}):
            with self.assertRaises(evidence.Refusal):
                evidence.validate_case("C", broken, text, ["required-mode-escape"])
        with self.assertRaises(evidence.Refusal):
            evidence.validate_case("C", config, text, ["required-mode-enter"])

    def test_every_source_run_and_window_binding_is_required(self):
        wanted = binding()
        evidence.validate_binding(wanted, wanted)
        for field in wanted:
            if field not in ("case", "window", "window_pixels", "xterm_pid", "source_sha", "source_tree", "run_id",
                             "attempt", "job", "auxiliary_sha", "binary_sha256"):
                continue
            for broken in ({k: v for k, v in wanted.items() if k != field},
                           {**wanted, field: "wrong"}):
                with self.subTest(field=field):
                    with self.assertRaises(evidence.Refusal):
                        evidence.validate_binding(broken, wanted)

    def test_no_tty_or_different_dimensions_never_qualify(self):
        evidence.validate_terminal({"isatty": [True, True, True], "rows": 24, "columns": 80})
        for broken in ({"isatty": [True, True, False], "rows": 24, "columns": 80},
                       {"isatty": [True, True, True], "rows": 24, "columns": 81},
                       {"isatty": [True, True, True], "rows": 25, "columns": 80}, {}):
            with self.assertRaises(evidence.Refusal):
                evidence.validate_terminal(broken)

    def test_forwarding_preserves_ansi_and_partial_writes_byte_for_byte(self):
        guard = evidence.OutputGuard("private-test-token")
        raw = b"\x1b[2Jmode\r\n> \r\n  auto\r\n\x1b[6n"
        sink = bytearray()
        def partial(chunk):
            sink.extend(chunk[:2])
            return min(2, len(chunk))
        guard.accept(raw)
        guard.forward(raw, partial)
        guard.verify()
        self.assertEqual(bytes(sink), raw)
        rebuilt = evidence.OutputGuard("private-test-token")
        rebuilt.accept(raw)
        rebuilt.forward(evidence.visible(raw).encode(), lambda chunk: len(chunk))
        with self.assertRaisesRegex(evidence.Refusal, "display_fidelity"):
            rebuilt.verify()

    def test_secret_boundary_guard_matches_actual_forward_order(self):
        guard = evidence.OutputGuard("secret-value")
        sink = bytearray()
        guard.accept(b"prefix secret-")
        guard.forward(b"prefix secret-", lambda data: (sink.extend(data), len(data))[1])
        with self.assertRaisesRegex(evidence.Refusal, "secret_echo"):
            guard.accept(b"value suffix")
        self.assertEqual(bytes(sink), b"prefix secret-")
        self.assertNotIn(b"secret-value", sink)  # Earlier prefix display is not denied.

    def test_output_overflow_and_zero_write_refuse(self):
        guard = evidence.OutputGuard("secret", cap=4)
        guard.accept(b"1234")
        with self.assertRaisesRegex(evidence.Refusal, "overflow"):
            guard.accept(b"5")
        with self.assertRaisesRegex(evidence.Refusal, "display_write"):
            guard.forward(b"1234", lambda _chunk: 0)

    def test_only_finite_get_routes_without_body_are_accepted(self):
        self.assertEqual(evidence.request_path(b"GET /registry.json HTTP/1.1\r\nHost: local\r\n\r\n"),
                         "/registry.json")
        for bad in (b"GET /api/tags HTTP/1.1\r\n\r\n", b"POST /registry.json HTTP/1.1\r\n\r\n",
                    b"GET /registry.json HTTP/1.1\r\nContent-Length: 1\r\n\r\n",
                    b"GET /registry.json HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
                    b"GET /registry.json HTTP/1.1\r\n\r\nGET /api/tags HTTP/1.1\r\n\r\n",
                    b"x" * 4097):
            with self.assertRaises(evidence.Refusal):
                evidence.request_path(bad)

    def test_pixel_envelope_rejects_unbounded_or_wrong_format(self):
        header = b"\x89PNG\r\n\x1a\n" + b"\0\0\0\rIHDR" + struct.pack(">II", 800, 480)
        self.assertEqual(evidence.validate_png(header)["width"], 800)
        for bad in (b"reconstructed-text", header[:16] + struct.pack(">II", 1281, 480),
                    header + b"x" * evidence.IMAGE_CAP):
            with self.assertRaises(evidence.Refusal):
                evidence.validate_png(bad)

    def test_cleanup_requires_wait_and_group_absence_and_is_not_repeated(self):
        child = owned_child()
        events = []
        with child_mocks(child), mock.patch.object(evidence.os, "killpg", side_effect=lambda *_: events.append("signal")), \
                mock.patch.object(evidence.os, "waitpid", side_effect=lambda *_: (events.append("reap"), (child.pid, 0))[1]) as wait:
            result = evidence.stop_process(child)
            self.assertTrue(evidence.cleanup_proved(result))
            self.assertEqual(events, ["signal", "signal", "reap"])
            self.assertIs(evidence.stop_process(child), result)
            wait.assert_called_once()
            child.proc.poll.assert_not_called()
            child.proc.wait.assert_not_called()

    def test_timeout_or_unreaped_group_cannot_be_cleanup_success(self):
        child = owned_child()
        ticks = iter(range(100))
        with child_mocks(child, exited=False), mock.patch.object(evidence.time, "monotonic", side_effect=lambda: next(ticks)), \
                mock.patch.object(evidence.time, "sleep"), mock.patch.object(evidence.os, "killpg") as kill:
            result = evidence.stop_process(child)
            self.assertFalse(evidence.cleanup_proved(result))
            self.assertFalse(result["direct_reaped"])
            self.assertEqual(kill.call_count, 2)

    def test_bridge_receipt_cannot_substitute_exit_fidelity_or_cleanup(self):
        wanted = binding()
        good = {"binding": wanted, "terminal": {"isatty": [True] * 3, "rows": 24, "columns": 80},
                "failure": None, "exit": 0, "observed_sha256": "a", "display_sha256": "a",
                "bytes": 12, "display_bytes": 12, "cli_pid": 302,
                "parent_pid": 303, "cleanup": cleanup_receipt()}
        evidence.validate_bridge(good, wanted)
        changes = ({"exit": 130}, {"failure": "prompt_timeout"}, {"display_sha256": "different"},
                   {"display_bytes": 11}, {"cleanup": {"direct_reaped": False, "group_absent": True}},
                   {"cleanup": {"pid": 303, "direct_reaped": True, "group_absent": True}})
        for change in changes:
            with self.assertRaises(evidence.Refusal):
                evidence.validate_bridge({**good, **change}, wanted)

    def test_export_rejects_configs_unknown_paths_and_secret_content(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence.private_write(root / "context.json", b'{"safe": true}')
            evidence.verify_export(root, ["private-test-token"])
            evidence.private_write(root / "config.toml", b"private")
            with self.assertRaisesRegex(evidence.Refusal, "not_whitelisted"):
                evidence.verify_export(root, [])
            (root / "config.toml").unlink()
            evidence.private_write(root / "actions.json", b'"private-test-token"')
            with self.assertRaisesRegex(evidence.Refusal, "export_secret"):
                evidence.verify_export(root, ["private-test-token"])

    def test_exclusive_file_and_symlink_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "data.json"
            evidence.private_write(path, b"{}")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                evidence.private_write(path, b"replace")
            alias = root / "alias"
            alias.symlink_to(path)
            with self.assertRaises(evidence.Refusal):
                evidence.bounded_read(alias, 64)
            with self.assertRaises(evidence.Refusal):
                evidence.owned_path(root, root.parent / "peer")

    def test_false_finished_state_does_not_qualify_interrupted_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "quickstart-cli-evidence-123-1"
            root.mkdir(mode=0o700)
            evidence.write_json(root / "state.json", {"started": True})
            environment = {"RUNNER_TEMP": directory, "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1",
                           "GITHUB_JOB": "evidence", "GITHUB_WORKFLOW_SHA": "b" * 40}
            with mock.patch.dict(os.environ, environment), mock.patch.object(evidence, "export_outputs") as outputs:
                self.assertEqual(evidence.finalize(root), 1)
            outputs.assert_called_once_with(False, True)
            receipt = evidence.read_json(root / "failure/receipt.json")
            self.assertEqual(receipt["code"], "finalizer_refused")
            self.assertFalse(receipt["execution_proof"])

    def test_stage_refusal_reaches_cleanup_and_never_becomes_success(self):
        for stage in ("setup", "build", "case"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                capture = object.__new__(evidence.Capture)
                capture.root = root
                capture.context = {k: v for k, v in binding().items() if k in
                                   ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree")}
                capture.export = root / "export"
                capture.private = root / "private"
                capture.private.mkdir()
                capture.manager = mock.Mock()
                capture.manager.cleanup.return_value = []
                capture.started_cases = []
                capture.failure = None
                capture.setup = mock.Mock()
                capture.build = mock.Mock()
                capture.case = mock.Mock()
                getattr(capture, stage).side_effect = evidence.Refusal("injected_" + stage)
                capture.export_files = mock.Mock()
                self.assertEqual(capture.run(), 1)
                capture.manager.cleanup.assert_called_once()
                capture.export_files.assert_not_called()
                self.assertFalse((root / "finished.json").exists())
                self.assertFalse((root / "export-decision.json").exists())
                self.assertFalse(evidence.read_json(root / "failure/receipt.json")["execution_proof"])
                self.assertFalse(capture.private.exists())

    def test_tracked_object_pointer_cannot_hide_a_private_workfile_overlay(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "fixture.rs"
            raw = b"public-source\n"
            oid = evidence.hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
            path.write_bytes(raw)
            index = ("100644 " + oid + " 0\tfixture.rs\0").encode()
            with mock.patch.object(evidence, "git", return_value=index):
                before = evidence.snapshot(None, root)
                path.write_bytes(b"private-overlay\n")
                with self.assertRaisesRegex(evidence.Refusal, "source_overlay"):
                    evidence.snapshot(None, root)
                path.write_bytes(raw)
                self.assertEqual(evidence.snapshot(None, root), before)

    def test_changed_birth_parent_group_or_session_never_authorizes_a_signal(self):
        for key in ("start_ticks", "parent_pid", "pgid", "session"):
            child = owned_child()
            with self.subTest(key=key), child_mocks(child), \
                    mock.patch.object(evidence, "process_identity", return_value={
                        **child.identity, key: child.identity[key] + 1, "state": "Z"}), \
                    mock.patch.object(evidence.os, "killpg") as kill, \
                    mock.patch.object(evidence.os, "waitpid") as reap:
                result = evidence.stop_process(child)
                self.assertFalse(evidence.cleanup_proved(result))
                kill.assert_not_called()
                reap.assert_not_called()

    def test_retired_or_already_reaped_leader_with_live_group_is_safe_refusal(self):
        for retired in (True, False):
            child = owned_child()
            child.retired = retired
            child.proc.returncode = 0 if not retired else None
            with self.subTest(retired=retired), child_mocks(child), \
                    mock.patch.object(evidence, "group_exists", return_value=True), \
                    mock.patch.object(evidence.os, "killpg") as kill, \
                    mock.patch.object(evidence.os, "waitpid") as reap:
                self.assertFalse(evidence.cleanup_proved(evidence.stop_process(child)))
                kill.assert_not_called()
                reap.assert_not_called()

    def test_external_direct_reap_waitid_fault_never_signals(self):
        child = owned_child()
        with child_mocks(child), mock.patch.object(evidence.os, "waitid", side_effect=ChildProcessError), \
                mock.patch.object(evidence.os, "killpg") as kill:
            result = evidence.stop_process(child)
            self.assertTrue(result["ownership_retired"])
            self.assertFalse(result["direct_reaped"])
            self.assertFalse(evidence.cleanup_proved(result))
            kill.assert_not_called()

    def test_exited_unreaped_leader_keeps_anchor_until_ordinary_group_cleanup(self):
        child, events = owned_child(), []
        clock = [0.0]
        def sleep(seconds):
            clock[0] += seconds
        def kill(_pgid, sig):
            self.assertFalse(child.retired)
            events.append(sig)
        def group(_pgid, _deadline, live_only=False):
            return live_only and evidence.signal.SIGKILL not in events
        def reap(*_):
            self.assertTrue(child.retired)
            self.assertEqual(events, [evidence.signal.SIGTERM, evidence.signal.SIGKILL])
            events.append("direct-reap")
            return child.pid, 0
        with child_mocks(child), mock.patch.object(evidence.time, "monotonic", side_effect=lambda: clock[0]), \
                mock.patch.object(evidence.time, "sleep", side_effect=sleep), \
                mock.patch.object(evidence, "group_exists", side_effect=group), \
                mock.patch.object(evidence.os, "killpg", side_effect=kill), \
                mock.patch.object(evidence.os, "waitpid", side_effect=reap):
            self.assertTrue(evidence.cleanup_proved(evidence.stop_process(child)))
        self.assertEqual(events[-1], "direct-reap")

    def test_member_still_present_after_direct_reap_gets_no_further_signal(self):
        child = owned_child()
        clock = [0.0]
        def sleep(seconds):
            clock[0] += seconds
        def group(_pid, _end, live_only=False):
            return not live_only
        with child_mocks(child), mock.patch.object(evidence.time, "monotonic", side_effect=lambda: clock[0]), \
                mock.patch.object(evidence.time, "sleep", side_effect=sleep), \
                mock.patch.object(evidence, "group_exists", side_effect=group), \
                mock.patch.object(evidence.os, "killpg") as kill, \
                mock.patch.object(evidence.os, "waitpid", return_value=(child.pid, 0)):
            result = evidence.stop_process(child)
            self.assertTrue(result["direct_reaped"])
            self.assertFalse(result["group_absent"])
            self.assertEqual(kill.call_count, 2)

    def test_read_signal_and_reap_faults_are_incomplete_not_cleanup_success(self):
        for fault in ("read", "TERM", "KILL", "reap"):
            child = owned_child()
            with self.subTest(fault=fault), child_mocks(child), contextlib.ExitStack() as stack:
                kill = stack.enter_context(mock.patch.object(evidence.os, "killpg"))
                reap = stack.enter_context(mock.patch.object(evidence.os, "waitpid", return_value=(child.pid, 0)))
                if fault == "read":
                    stack.enter_context(mock.patch.object(evidence, "process_identity", side_effect=OSError))
                elif fault in ("TERM", "KILL"):
                    kill.side_effect = OSError if fault == "TERM" else [None, OSError()]
                else:
                    reap.side_effect = KeyboardInterrupt
                result = evidence.stop_process(child)
                self.assertFalse(evidence.cleanup_proved(result))
                if fault == "read":
                    kill.assert_not_called()
                if fault == "reap":
                    self.assertTrue(child.retired)
                    before = kill.call_count
                    evidence.stop_process(child)
                    self.assertEqual(kill.call_count, before)

    def test_nonreaping_poll_and_wait_never_call_popen_reaping_methods(self):
        child = owned_child()
        with child_mocks(child), mock.patch.object(evidence.os, "waitid", return_value=types.SimpleNamespace(
                si_pid=child.pid, si_code=os.CLD_EXITED, si_status=0)) as peek:
            self.assertEqual(child.poll(), 0)
            self.assertEqual(child.wait(1), 0)
            self.assertTrue(all(call.args[2] & os.WNOWAIT for call in peek.call_args_list))
            child.proc.poll.assert_not_called()
            child.proc.wait.assert_not_called()
            self.assertFalse(child.retired)

    def test_spawn_identity_refusal_still_retains_owned_cleanup_obligation(self):
        manager = evidence.Processes(100)
        proc = mock.Mock(pid=54321, returncode=None, stdout=None)
        with mock.patch.object(evidence.time, "monotonic", return_value=0), \
                mock.patch.object(evidence.subprocess, "Popen", return_value=proc) as spawn, \
                mock.patch.object(evidence.OwnedChild, "bind", side_effect=evidence.Refusal("process_identity")):
            with self.assertRaises(evidence.Refusal):
                manager.spawn(["synthetic-not-executed"])
        self.assertEqual(len(manager.items), 1)
        self.assertTrue(spawn.call_args.kwargs["start_new_session"])
        with mock.patch.object(evidence.os, "killpg") as kill:
            self.assertFalse(evidence.cleanup_proved(manager.cleanup()[0]))
            kill.assert_not_called()

    def test_group_observation_is_bounded_and_uses_no_signal_zero(self):
        entries = [types.SimpleNamespace(name="not-a-pid")] * 4097
        with mock.patch.object(evidence.os, "scandir") as scan, \
                mock.patch.object(evidence.time, "monotonic", return_value=0), \
                mock.patch.object(evidence.os, "killpg") as kill:
            scan.return_value.__enter__.return_value = iter(entries)
            with self.assertRaisesRegex(evidence.Refusal, "group_observation_boundary"):
                evidence.group_exists(54321, 1)
            kill.assert_not_called()

    def test_secret_allowed_actions_file_stays_private_and_only_minimum_is_eligible(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "quickstart-cli-evidence-123-1"
            root.mkdir(mode=0o700)
            capture = export_fixture(root)
            capture.actions[0]["secret-bearing-test-field"] = capture.tokens[0]
            with self.assertRaisesRegex(evidence.Refusal, "export_secret"):
                capture.export_files({"status": "reaped", "temporary_material_removed": True})
            self.assertFalse(capture.export.exists())
            self.assertFalse((root / "export-decision.json").exists())
            with mock.patch.dict(os.environ, evidence_environment(root, "failure")), \
                    mock.patch.object(evidence, "export_outputs") as outputs:
                self.assertEqual(evidence.finalize(root), 1)
            outputs.assert_called_once_with(False, True)
            receipt = evidence.verify_failure(root, capture.context)
            self.assertFalse(receipt["execution_proof"])
            self.assertNotIn(capture.tokens[0].encode(), (root / "failure/receipt.json").read_bytes())

    def test_unsafe_screenshot_bytes_never_get_an_export_decision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            capture = export_fixture(root)
            first = capture.frames[0]
            path = capture.private / (first["name"] + ".capture")
            data = path.read_bytes() + capture.tokens[0].encode()
            evidence.private_write(path, data, replace=True)
            first["pixels"] = evidence.validate_png(data)
            with self.assertRaisesRegex(evidence.Refusal, "export_secret"):
                capture.export_files({"status": "reaped", "temporary_material_removed": True})
            self.assertFalse(capture.export.exists())
            self.assertFalse((root / "export-decision.json").exists())

    def test_success_export_requires_full_six_frames_and_bound_current_decision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "quickstart-cli-evidence-123-1"
            root.mkdir(mode=0o700)
            capture = export_fixture(root)
            with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                self.assertEqual(capture.run(), 0)
            self.assertEqual(len(list((root / "export/screens").iterdir())), 6)
            with mock.patch.dict(os.environ, evidence_environment(root)), \
                    mock.patch.object(evidence, "export_outputs") as outputs:
                self.assertEqual(evidence.finalize(root), 0)
            outputs.assert_called_once_with(True, False)

    def test_missing_invalid_or_wrong_binding_decision_never_selects_rich_upload(self):
        for mutation in ("missing", "unsafe", "source_sha", "run_id", "attempt", "auxiliary_sha", "hash"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                capture = export_fixture(root)
                with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                    self.assertEqual(capture.run(), 0)
                path = root / "export-decision.json"
                decision = evidence.read_json(path)
                if mutation == "missing":
                    path.unlink()
                else:
                    if mutation == "unsafe": decision["safe"] = False
                    elif mutation == "hash": decision["files"]["actions.json"]["sha256"] = "0" * 64
                    else: decision["binding"][mutation] = "wrong"
                    evidence.write_json(path, decision, replace=True)
                with mock.patch.dict(os.environ, evidence_environment(root)), \
                        mock.patch.object(evidence, "export_outputs") as outputs:
                    self.assertEqual(evidence.finalize(root), 1)
                outputs.assert_called_once_with(False, True)
                self.assertFalse(path.exists())

    def test_missing_frame_changed_source_or_byte_overflow_refuses_staged_publication(self):
        for mutation in ("frame", "source", "oversize"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                capture = export_fixture(root)
                capture.export_files({"status": "reaped", "temporary_material_removed": True})
                if mutation == "frame": (capture.staging / "screens" / evidence.SCREENS[0]).unlink()
                elif mutation == "source":
                    value = evidence.read_json(capture.staging / "context.json")
                    value["source_sha"] = "0" * 40
                    evidence.write_json(capture.staging / "context.json", value, replace=True)
                else:
                    evidence.private_write(capture.staging / "actions.json", b"x" * (evidence.JSON_CAP + 1), replace=True)
                with self.assertRaises(evidence.Refusal):
                    evidence.publish_bundle(root, capture.staging, capture.tokens, capture.context)
                self.assertFalse(capture.export.exists())
                self.assertFalse((root / "export-decision.json").exists())

    def test_refusal_between_staging_and_publication_has_no_success_decision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            capture = export_fixture(root)
            capture.export_files({"status": "reaped", "temporary_material_removed": True})
            rename = os.rename
            def changed(source, target):
                rename(source, target)
                evidence.private_write(target / "actions.json", capture.tokens[0].encode(), replace=True)
            with mock.patch.object(evidence.os, "rename", side_effect=changed):
                with self.assertRaises(evidence.Refusal):
                    evidence.publish_bundle(root, capture.staging, capture.tokens, capture.context)
            self.assertFalse((root / "export-decision.json").exists())

    def test_interrupt_after_publication_before_finished_revokes_rich_eligibility(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "quickstart-cli-evidence-123-1"
            root.mkdir(mode=0o700)
            capture = export_fixture(root)
            write = evidence.write_json
            def interrupted(path, *args, **kwargs):
                if path.name == "finished.json": raise KeyboardInterrupt
                return write(path, *args, **kwargs)
            with mock.patch.object(evidence, "snapshot", return_value="a" * 64), \
                    mock.patch.object(evidence, "write_json", side_effect=interrupted):
                self.assertEqual(capture.run(), 1)
            self.assertFalse((root / "export-decision.json").exists())
            self.assertTrue((root / "failure/receipt.json").is_file())

    def test_failed_capture_outcome_cannot_be_laundered_by_valid_finished_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "quickstart-cli-evidence-123-1"
            root.mkdir(mode=0o700)
            capture = export_fixture(root)
            with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                self.assertEqual(capture.run(), 0)
            with mock.patch.dict(os.environ, evidence_environment(root, "failure")), \
                    mock.patch.object(evidence, "export_outputs") as outputs:
                self.assertEqual(evidence.finalize(root), 1)
            outputs.assert_called_once_with(False, True)

    def test_minimum_failure_rejects_unknown_codes_or_private_extra_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            context = {k: v for k, v in binding().items() if k in
                       ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree")}
            with self.assertRaises(evidence.Refusal):
                evidence.minimum_failure(root, context, "synthetic-secret-as-error")
            evidence.minimum_failure(root, context, "capture_failed")
            receipt = evidence.read_json(root / "failure/receipt.json")
            receipt["output"] = "synthetic secret"
            evidence.write_json(root / "failure/receipt.json", receipt, replace=True)
            decision = evidence.read_json(root / "failure-decision.json")
            decision["receipt_sha256"] = evidence.sha((root / "failure/receipt.json").read_bytes())
            evidence.write_json(root / "failure-decision.json", decision, replace=True)
            with self.assertRaisesRegex(evidence.Refusal, "failure_schema"):
                evidence.verify_failure(root, context)

    def test_workflow_success_and_minimum_failure_upload_gates_are_separate(self):
        workflow = (Path(__file__).parents[2] / ".github/workflows/quickstart-cli-evidence-manual.yml").read_text()
        rich = workflow.split("      - name: Upload explicit affected evidence files\n", 1)[1]
        rich, failure = rich.split("      - name: Upload minimum safe failure receipt\n", 1)
        for fragment in ("!cancelled()", "steps.capture.outcome == 'success'",
                         "steps.finalize.outcome == 'success'", "steps.finalize.outputs.success_export == 'true'"):
            self.assertIn(fragment, rich)
        self.assertNotIn("if: always()", rich)
        self.assertIn("steps.finalize.outputs.failure_export == 'true'", failure)
        self.assertIn("/failure/receipt.json", failure)
        self.assertNotIn("/export/", failure)
        self.assertIn("timeout-minutes: 5", workflow)
        self.assertIn("timeout-minutes: 45", workflow)

    def test_interrupt_before_publication_keeps_staged_screens_ineligible(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            capture = export_fixture(root)
            capture.export_files({"status": "reaped", "temporary_material_removed": True})
            with mock.patch.object(evidence.os, "rename", side_effect=KeyboardInterrupt):
                with self.assertRaises(KeyboardInterrupt):
                    evidence.publish_bundle(root, capture.staging, capture.tokens, capture.context)
            self.assertTrue(capture.staging.is_dir())
            self.assertFalse(capture.export.exists())
            self.assertFalse((root / "export-decision.json").exists())

    def test_finalizer_read_or_interrupt_fault_exports_only_safe_minimum(self):
        for error in (OSError(), KeyboardInterrupt()):
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                capture = export_fixture(root)
                with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                    self.assertEqual(capture.run(), 0)
                with mock.patch.dict(os.environ, evidence_environment(root)), \
                        mock.patch.object(evidence, "validate_bundle", side_effect=error), \
                        mock.patch.object(evidence, "export_outputs") as outputs:
                    self.assertEqual(evidence.finalize(root), 1)
                outputs.assert_called_once_with(False, True)
                self.assertFalse((root / "export-decision.json").exists())
                self.assertFalse(evidence.verify_failure(root, capture.context)["execution_proof"])

    def test_cleanup_fault_prevents_success_evidence_even_with_complete_cases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            capture = export_fixture(root)
            capture.manager.cleanup.return_value = [{**cleanup_receipt(), "group_absent": False}]
            with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                self.assertEqual(capture.run(), 1)
            self.assertFalse(capture.export.exists())
            self.assertFalse((root / "export-decision.json").exists())
            receipt = evidence.verify_failure(root, capture.context)
            self.assertEqual(receipt["code"], "cleanup_incomplete")
            self.assertFalse(receipt["cleanup_complete"])

    def test_failure_decision_wrong_source_run_attempt_and_unsafe_extra_never_qualify(self):
        for field in ("source_sha", "run_id", "attempt", "private_extra"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                context = {k: v for k, v in binding().items() if k in
                           ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree")}
                evidence.minimum_failure(root, context, "capture_failed")
                decision = evidence.read_json(root / "failure-decision.json")
                if field == "private_extra": decision[field] = "synthetic secret"
                else: decision["binding"][field] = "wrong"
                evidence.write_json(root / "failure-decision.json", decision, replace=True)
                with self.assertRaises(evidence.Refusal):
                    evidence.verify_failure(root, context)

    def test_raw_config_environment_or_private_key_fields_are_never_success_exports(self):
        for field in ("config", "environment", "private_key_sha256"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                capture = export_fixture(root)
                capture.actions[0][field] = "not-a-synthetic-token"
                with self.assertRaisesRegex(evidence.Refusal, "export_private_fields"):
                    capture.export_files({"status": "reaped", "temporary_material_removed": True})
                self.assertFalse(capture.export.exists())
                self.assertFalse((root / "export-decision.json").exists())

    def test_actual_bridge_finally_does_not_skip_retired_leader_live_group(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            bridge = object.__new__(evidence.Bridge)
            bridge.directory = Path(directory)
            bridge.deadline = 10
            bridge.guard = evidence.OutputGuard("synthetic-secret")
            bridge.events, bridge.master, bridge.pid = [], None, None
            bridge.child, bridge.status, bridge.binding = None, None, None
            raw = b"fake contract binary"
            wanted = {**binding(), "binary_sha256": evidence.sha(raw)}
            bridge.spec = {"binding": wanted, "binary": "unused", "config": directory,
                           "argv": [], "environment": {}}
            bridge.drive, bridge.pump = mock.Mock(), mock.Mock(return_value=False)
            terminal = {"isatty": [True] * 3, "rows": 24, "columns": 80}
            for name, value in [("terminal_info", terminal), ("wait_file", {"window": 42,
                                "xterm_pid": 301, "window_pixels": [800, 480]}), ("bounded_read", raw)]:
                stack.enter_context(mock.patch.object(evidence, name, return_value=value))
            stack.enter_context(mock.patch.object(evidence.time, "monotonic", return_value=0))
            stack.enter_context(mock.patch.object(evidence.termios, "tcgetattr", return_value=[]))
            stack.enter_context(mock.patch.object(evidence.termios, "tcsetattr"))
            stack.enter_context(mock.patch.object(evidence.tty, "setraw"))
            stack.enter_context(mock.patch.object(evidence.fcntl, "fcntl", return_value=0))
            stack.enter_context(mock.patch.object(evidence.fcntl, "ioctl"))
            stack.enter_context(mock.patch.object(evidence.os, "pipe", return_value=(10, 11)))
            stack.enter_context(mock.patch.object(evidence.pty, "fork", return_value=(54321, 12)))
            for name in ("close", "set_blocking", "write"):
                stack.enter_context(mock.patch.object(evidence.os, name))
            stack.enter_context(mock.patch.object(evidence, "private_write"))
            result = stack.enter_context(mock.patch.object(evidence, "write_json"))
            kill = stack.enter_context(mock.patch.object(evidence.os, "killpg"))
            stack.enter_context(mock.patch.object(evidence, "group_exists", return_value=True))
            def bind_child(child, _deadline):
                child.identity = {"pid": child.pid, "parent_pid": child.parent,
                                  "pgid": child.pid, "session": child.pid, "start_ticks": 17}
                return child
            def external_retirement(child, _timeout):
                child.retired, child.returncode = True, 0
                return 0
            stack.enter_context(mock.patch.object(evidence.OwnedChild, "bind", autospec=True, side_effect=bind_child))
            stack.enter_context(mock.patch.object(evidence.OwnedChild, "wait", autospec=True, side_effect=external_retirement))
            self.assertEqual(bridge.run(), 1)
            kill.assert_not_called()
            receipt = result.call_args.args[1]
            self.assertEqual(receipt["failure"], "cli_cleanup_incomplete")
            self.assertFalse(receipt["cleanup"]["direct_reaped"])
            self.assertFalse(receipt["cleanup"]["group_absent"])


    def test_build_materializes_single_and_multilink_compiler_outputs(self):
        for linked in (False, True):
            with self.subTest(linked=linked), tempfile.TemporaryDirectory() as directory:
                capture = object.__new__(evidence.Capture)
                capture.private = Path(directory) / "private"
                capture.private.mkdir(mode=0o700)
                capture.source, capture.provenance = Path(directory) / "source", {}
                capture.manager = mock.Mock(deadline=evidence.time.monotonic() + 60)
                calls = []
                def build(args, seconds, **kwargs):
                    calls.append((args, seconds, kwargs))
                    self.assertFalse((capture.private / "materialized").exists())
                    if len(calls) == 2:
                        compiler_outputs(capture.private, linked)
                    return b""
                capture.manager.run.side_effect = build
                capture.build()
                self.assertEqual([row[1] for row in calls], [25 * 60, 6 * 60])
                self.assertEqual(calls[0][0][:8], ["cargo", "+1.98.0", "build", "--locked",
                                                 "-p", "zeroclaw", "--bin", "zeroclaw"])
                self.assertIn("plugins-wasm-cranelift", calls[0][0])
                self.assertNotIn("--no-default-features", calls[0][0])
                self.assertIn("wasm32-wasip2", calls[1][0])
                for relative, destination, key in [(evidence.CLI_ARTIFACT, capture.binary, "binary"),
                                                   (evidence.COMPONENT_ARTIFACT, capture.wasm_path, "component")]:
                    original = capture.private.joinpath(*relative)
                    self.assertEqual(original.stat().st_nlink, 2 if linked else 1)
                    self.assertEqual(destination.stat().st_nlink, 1)
                    self.assertFalse(os.path.samefile(original, destination))
                    self.assertEqual(original.read_bytes(), destination.read_bytes())
                    self.assertEqual(capture.compiled_artifacts[key]["sha256"],
                                     evidence.sha(original.read_bytes()))
                    evidence.verify_materialized_artifact(capture.private, capture.compiled_artifacts[key],
                                                          capture.manager.deadline)
                self.assertEqual(capture.binary.stat().st_mode & 0o777, 0o700)
                self.assertEqual(capture.wasm_path.stat().st_mode & 0o777, 0o600)

    def test_compiler_copy_refuses_source_boundaries_and_instability(self):
        for fault in ("outside", "symlink", "parent", "owner", "nonregular", "size", "empty",
                      "changed", "named-swap", "read"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
                private = Path(directory) / "private"
                private.mkdir(mode=0o700)
                compiler_outputs(private)
                (private / "materialized").mkdir(mode=0o700)
                source = private.joinpath(*evidence.CLI_ARTIFACT)
                relative = evidence.CLI_ARTIFACT
                if fault == "outside":
                    relative = ("..", "debug", "zeroclaw")
                elif fault == "symlink":
                    source.rename(source.with_name("real"))
                    source.symlink_to(source.with_name("real"))
                elif fault == "parent":
                    source.parent.rename(private / "saved-debug")
                    source.parent.symlink_to(private / "saved-debug", target_is_directory=True)
                elif fault == "changed":
                    read = evidence.os.read
                    changed = []
                    def changing(fd, size):
                        chunk = read(fd, size)
                        if not changed:
                            changed.append(True)
                            source.write_bytes(b"changed during copy")
                        return chunk
                    stack.enter_context(mock.patch.object(evidence.os, "read", side_effect=changing))
                elif fault == "named-swap":
                    open_file, swapped = evidence.os.open, []
                    def opening(path, flags, *args, **kwargs):
                        fd = open_file(path, flags, *args, **kwargs)
                        if path == "zeroclaw" and not flags & os.O_WRONLY and not swapped:
                            swapped.append(True)
                            source.rename(source.with_name("old-source"))
                            evidence.private_write(source, b"replacement source")
                        return fd
                    stack.enter_context(mock.patch.object(evidence.os, "open", side_effect=opening))
                elif fault == "read":
                    stack.enter_context(mock.patch.object(evidence.os, "read", side_effect=OSError))
                else:
                    fstat = evidence.os.fstat
                    def metadata(fd):
                        st = fstat(fd)
                        if not evidence.stat.S_ISREG(st.st_mode):
                            return st
                        values = {"st_" + k: v for k, v in evidence.artifact_identity(st).items()}
                        if fault == "owner": values["st_uid"] += 1
                        elif fault == "nonregular": values["st_mode"] = evidence.stat.S_IFIFO | 0o600
                        elif fault == "size": values["st_size"] = 256 * 1024 * 1024 + 1
                        else: values["st_size"] = 0
                        return types.SimpleNamespace(**values)
                    stack.enter_context(mock.patch.object(evidence.os, "fstat", side_effect=metadata))
                with self.assertRaises(evidence.Refusal):
                    evidence.materialize_compiler_artifact(private, relative, evidence.time.monotonic() + 10)

    def test_compiler_copy_partial_io_and_close_failures_never_grant_a_copy(self):
        for fault in ("partial", "short-read", "zero", "write", "source-close", "destination-close",
                      "existing", "linked-destination"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
                private = Path(directory) / "private"
                private.mkdir(mode=0o700)
                compiler_outputs(private, linked=True)
                (private / "materialized").mkdir(mode=0o700)
                destination = private / "materialized/zeroclaw"
                if fault in ("existing", "linked-destination"):
                    evidence.private_write(destination, b"untouched destination")
                    if fault == "linked-destination":
                        os.link(destination, destination.with_name("alias"))
                write, close, open_file, read = evidence.os.write, evidence.os.close, evidence.os.open, evidence.os.read
                labels, closed, injected, opened_fds, closed_fds = {}, [], [], [], []
                def opened(path, flags, *args, **kwargs):
                    fd = open_file(path, flags, *args, **kwargs)
                    opened_fds.append(fd)
                    if evidence.stat.S_ISREG(os.fstat(fd).st_mode):
                        labels[fd] = "destination-close" if flags & os.O_WRONLY else "source-close"
                    return fd
                def closing(fd):
                    label = labels.get(fd, "directory")
                    close(fd)
                    closed.append(label)
                    closed_fds.append(fd)
                    if label == fault and not injected:
                        injected.append(True)
                        raise OSError("controlled close fault")
                stack.enter_context(mock.patch.object(evidence.os, "open", side_effect=opened))
                stack.enter_context(mock.patch.object(evidence.os, "close", side_effect=closing))
                if fault in ("partial", "zero", "write"):
                    def writing(fd, data):
                        if fault == "zero": return 0
                        if fault == "write": raise OSError("controlled write fault")
                        return write(fd, data[:1])
                    stack.enter_context(mock.patch.object(evidence.os, "write", side_effect=writing))
                if fault == "short-read":
                    stack.enter_context(mock.patch.object(evidence.os, "read", side_effect=lambda fd, size: read(fd, min(size, 1))))
                if fault in ("partial", "short-read"):
                    record = evidence.materialize_compiler_artifact(private, evidence.CLI_ARTIFACT,
                                                                   evidence.time.monotonic() + 10)
                    self.assertEqual(destination.read_bytes(), b"fake contract binary")
                    self.assertEqual(record["sha256"], evidence.sha(destination.read_bytes()))
                else:
                    with self.assertRaises(evidence.Refusal):
                        evidence.materialize_compiler_artifact(private, evidence.CLI_ARTIFACT,
                                                               evidence.time.monotonic() + 10)
                if fault in ("source-close", "destination-close"):
                    self.assertIn("source-close", closed)
                    self.assertIn("destination-close", closed)
                    self.assertEqual(len(injected), 1)
                    self.assertEqual(set(opened_fds), set(closed_fds))
                if fault in ("existing", "linked-destination"):
                    self.assertEqual(destination.read_bytes(), b"untouched destination")

    def test_actual_build_bridge_and_final_recheck_bind_both_original_and_copy(self):
        for mutation in ("source", "copy", "component", "ledger"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                capture = export_fixture(root)
                def case(name):
                    if name != "C": return
                    if mutation == "ledger":
                        capture.compiled_artifacts["component"] = capture.compiled_artifacts["binary"]
                    else:
                        path = (capture.private.joinpath(*evidence.CLI_ARTIFACT) if mutation == "source"
                                else capture.binary if mutation == "copy" else capture.wasm_path)
                        path.write_bytes(b"changed fixture bytes")
                capture.case.side_effect = case
                with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                    self.assertEqual(capture.run(), 1)
                self.assertFalse((root / "export-decision.json").exists())
                self.assertFalse(evidence.verify_failure(root, capture.context)["execution_proof"])
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            root = Path(directory)
            capture = export_fixture(root)
            bridge = object.__new__(evidence.Bridge)
            bridge.directory, bridge.deadline = root, evidence.time.monotonic() + 10
            bridge.spec = {"binding": {**binding(), "binary_sha256": capture.binary_hash},
                           "binary": str(capture.binary)}
            bridge.guard = evidence.OutputGuard("synthetic-secret")
            terminal = {"isatty": [True] * 3, "rows": 24, "columns": 80}
            stack.enter_context(mock.patch.object(evidence, "terminal_info", return_value=terminal))
            stack.enter_context(mock.patch.object(evidence, "wait_file", return_value={
                "window": 42, "xterm_pid": 301, "window_pixels": [800, 480]}))
            stack.enter_context(mock.patch.object(evidence.termios, "tcgetattr", return_value=[]))
            stack.enter_context(mock.patch.object(evidence.fcntl, "fcntl", return_value=0))
            pipe = stack.enter_context(mock.patch.object(evidence.os, "pipe", side_effect=evidence.Refusal("before_fork")))
            with self.assertRaisesRegex(evidence.Refusal, "before_fork"):
                bridge.run()
            pipe.assert_called_once()
            pipe.reset_mock()
            capture.binary.write_bytes(b"changed fixture bytes")
            with self.assertRaisesRegex(evidence.Refusal, "binary_changed"):
                bridge.run()
            pipe.assert_not_called()

    def test_private_export_and_failure_readers_keep_their_single_link_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "context.json"
            evidence.private_write(path, b"{}")
            os.link(path, root / "private-alias")
            with self.assertRaisesRegex(evidence.Refusal, "file_boundary"):
                evidence.bounded_read(path, 64)
            (root / "private-alias").unlink()
            export = root / "export"
            export.mkdir(mode=0o700)
            evidence.private_write(export / "context.json", b"{}")
            os.link(export / "context.json", root / "export-alias")
            with self.assertRaisesRegex(evidence.Refusal, "file_boundary"):
                evidence.verify_export(export, [])
            context = {k: v for k, v in binding().items() if k in
                       ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree")}
            evidence.minimum_failure(root, context, "capture_failed")
            os.link(root / "failure/receipt.json", root / "failure-alias")
            with self.assertRaisesRegex(evidence.Refusal, "file_boundary"):
                evidence.verify_failure(root, context)


    def test_pending_cancellation_during_term_reaches_kill_reap_and_cache_commit(self):
        for sig in (evidence.signal.SIGTERM, evidence.signal.SIGINT, evidence.signal.SIGHUP):
            with self.subTest(signal=sig), mock.patch.object(evidence, "PENDING_CANCELLATION", None):
                child, events, clock = owned_child(), [], [0.0]
                def sleep(seconds):
                    clock[0] += seconds
                    evidence.interrupted(sig, None)
                    evidence.interrupted(evidence.signal.SIGTERM, None)
                def kill(pgid, number):
                    self.assertFalse(child.retired)
                    self.assertEqual(pgid, child.pid)
                    events.append(number)
                def group(pgid, deadline, live_only=False):
                    return live_only and evidence.signal.SIGKILL not in events
                def reap(*args):
                    self.assertTrue(child.retired)
                    self.assertEqual(events, [evidence.signal.SIGTERM, evidence.signal.SIGKILL])
                    events.append("reap")
                    return child.pid, 0
                with child_mocks(child), mock.patch.object(evidence.time, "monotonic", side_effect=lambda: clock[0]), \
                        mock.patch.object(evidence.time, "sleep", side_effect=sleep), \
                        mock.patch.object(evidence.os, "killpg", side_effect=kill), \
                        mock.patch.object(evidence.os, "waitpid", side_effect=reap) as wait, \
                        mock.patch.object(evidence, "group_exists", side_effect=group):
                    result = evidence.stop_process(child)
                    self.assertTrue(evidence.cleanup_proved(result))
                    self.assertIs(evidence.stop_process(child), result)
                    wait.assert_called_once()
                self.assertEqual(evidence.PENDING_CANCELLATION, sig)
                with self.assertRaisesRegex(evidence.Refusal, "interrupted"):
                    evidence.cancellation_checkpoint()
                self.assertEqual(events[-1], "reap")

    def test_pending_cancellation_does_not_rearm_retired_or_uncertain_anchors(self):
        for fault in ("retired", "identity", "read", "wait", "sigchld"):
            with self.subTest(fault=fault), mock.patch.object(evidence, "PENDING_CANCELLATION", None):
                child = owned_child()
                evidence.interrupted(evidence.signal.SIGINT, None)
                with child_mocks(child), contextlib.ExitStack() as stack:
                    kill = stack.enter_context(mock.patch.object(evidence.os, "killpg"))
                    wait = stack.enter_context(mock.patch.object(evidence.os, "waitpid", return_value=(child.pid, 0)))
                    if fault == "retired": child.retired = True
                    elif fault == "identity":
                        stack.enter_context(mock.patch.object(evidence, "process_identity", return_value={
                            **child.identity, "start_ticks": 18, "state": "Z"}))
                    elif fault == "read":
                        stack.enter_context(mock.patch.object(evidence, "process_identity", side_effect=OSError))
                    elif fault == "wait": wait.side_effect = KeyboardInterrupt
                    else:
                        stack.enter_context(mock.patch.object(evidence.signal, "getsignal", return_value=evidence.signal.SIG_IGN))
                    self.assertFalse(evidence.cleanup_proved(evidence.stop_process(child)))
                    signals = kill.call_count
                    evidence.stop_process(child)
                    self.assertEqual(kill.call_count, signals)
                    if fault != "wait":
                        kill.assert_not_called()
                        wait.assert_not_called()
                    else:
                        self.assertTrue(child.retired)

    def test_processes_real_finally_and_multi_cleanup_close_after_pending_or_fault(self):
        child, events = owned_child(), []
        child.stdout = mock.Mock(closed=False)
        def close():
            child.stdout.closed = True
            events.append("close")
        child.stdout.close.side_effect = close
        child.stdout.fileno.return_value = 10009
        manager = evidence.Processes(100)
        manager.items = [child]
        def reading(*args):
            evidence.interrupted(evidence.signal.SIGTERM, None)
            return b""
        with child_mocks(child), mock.patch.object(manager, "spawn", return_value=child), \
                mock.patch.object(evidence.time, "monotonic", return_value=0), \
                mock.patch.object(evidence.os, "set_blocking"), \
                mock.patch.object(evidence.select, "select", return_value=([child.stdout], [], [])), \
                mock.patch.object(evidence.os, "read", side_effect=reading), \
                mock.patch.object(evidence.os, "killpg", side_effect=lambda *args: events.append("signal")), \
                mock.patch.object(evidence.os, "waitpid", side_effect=lambda *args: (events.append("reap"), (child.pid, 0))[1]):
            with self.assertRaisesRegex(evidence.Refusal, "interrupted"):
                manager.run(["synthetic-not-executed"], 1)
            self.assertEqual(events, ["signal", "signal", "reap", "close"])
            result = manager.cleanup()
            self.assertTrue(evidence.cleanup_proved(result[0]))
            self.assertEqual(events.count("signal"), 2)
        evidence.PENDING_CANCELLATION = None
        children = [owned_child(), owned_child()]
        children[1].pid += 1
        children[1].proc.pid = children[1].pid
        children[1].identity.update(pid=children[1].pid, pgid=children[1].pid, session=children[1].pid)
        children[0].stdout = mock.Mock(closed=True)
        children[1].stdout = mock.Mock(closed=False)
        def bad_close():
            evidence.interrupted(evidence.signal.SIGINT, None)
            raise OSError("controlled close failure")
        children[1].stdout.close.side_effect = bad_close
        manager.items = children
        identities = {item.pid: {**item.identity, "state": "Z"} for item in children}
        with mock.patch.object(evidence.os, "killpg") as kill, \
                mock.patch.object(evidence.os, "waitid", side_effect=lambda kind, pid, flags: types.SimpleNamespace(
                    si_pid=pid, si_code=os.CLD_EXITED, si_status=0)), \
                mock.patch.object(evidence.os, "waitpid", side_effect=lambda pid, flags: (pid, 0)), \
                mock.patch.object(evidence, "process_identity", side_effect=lambda pid: identities[pid]), \
                mock.patch.object(evidence, "group_exists", return_value=False), \
                mock.patch.object(evidence.signal, "getsignal", return_value=evidence.signal.SIG_DFL):
            records = manager.cleanup()
        self.assertEqual(len(records), 2)
        children[0].stdout.close.assert_called_once()
        children[1].stdout.close.assert_called_once()
        self.assertFalse(evidence.cleanup_proved(records[0]))
        self.assertTrue(evidence.cleanup_proved(records[1]))
        self.assertEqual(kill.call_count, 4)

    def test_bridge_and_capture_pending_cancellation_across_receipt_boundaries(self):
        for stage in ("stop", "output", "result", "ready", "close"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as directory, \
                    mock.patch.object(evidence, "PENDING_CANCELLATION", None), \
                    bridge_control(Path(directory)) as (bridge, controls):
                write_json, private_write = evidence.write_json, evidence.private_write
                def writing_json(path, value, **kwargs):
                    write_json(path, value, **kwargs)
                    if stage == "result" and path.name == "bridge-result.json":
                        evidence.interrupted(evidence.signal.SIGHUP, None)
                def writing_private(path, data, replace=False, **kwargs):
                    private_write(path, data, replace, **kwargs)
                    if (stage == "output" and path.name == "output.private") or (
                            stage == "ready" and path.name == "bridge-result.json.ready"):
                        evidence.interrupted(evidence.signal.SIGINT, None)
                if stage == "close":
                    def closing(fd):
                        if fd == 10012: raise OSError("controlled close failure")
                    controls["close"].side_effect = closing
                with mock.patch.object(evidence, "write_json", side_effect=writing_json), \
                        mock.patch.object(evidence, "private_write", side_effect=writing_private), \
                        mock.patch.object(evidence.os, "killpg", side_effect=(
                            (lambda *args: evidence.interrupted(evidence.signal.SIGTERM, None)) if stage == "stop" else None)):
                    self.assertEqual(bridge.run(), 1)
                receipt = evidence.json.loads((Path(directory) / "bridge-result.json").read_bytes())
                self.assertIsNotNone(receipt["failure"])
                controls["restore"].assert_called_once()
                self.assertTrue(any(call.args[1] == evidence.fcntl.F_SETFL for call in controls["flags"].call_args_list))
        for stage in ("cleanup.json", "export-decision.json", "finished.json"):
            with self.subTest(capture_stage=stage), tempfile.TemporaryDirectory() as directory, \
                    mock.patch.object(evidence, "PENDING_CANCELLATION", None):
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                capture = export_fixture(root)
                write = evidence.write_json
                def writing(path, *args, **kwargs):
                    write(path, *args, **kwargs)
                    if path.name == stage:
                        evidence.interrupted(evidence.signal.SIGTERM, None)
                with mock.patch.object(evidence, "snapshot", return_value="a" * 64), \
                        mock.patch.object(evidence, "write_json", side_effect=writing):
                    self.assertEqual(capture.run(), 1)
                self.assertFalse((root / "export-decision.json").exists())
                self.assertEqual(evidence.verify_failure(root, capture.context)["code"], "interrupted")

    def test_normal_checkpoints_do_not_interrupt_spawn_registration_or_change_child_masks(self):
        manager = evidence.Processes(100)
        proc = mock.Mock(pid=54321, returncode=None, stdout=None)
        captured = []
        def bind(child, deadline):
            evidence.interrupted(evidence.signal.SIGTERM, None)
            self.assertIn(child, manager.items)
            child.identity = {"pid": child.pid, "parent_pid": child.parent, "pgid": child.pid,
                              "session": child.pid, "start_ticks": 17}
            captured.append(child)
            return child
        with mock.patch.object(evidence.time, "monotonic", return_value=0), \
                mock.patch.object(evidence.subprocess, "Popen", return_value=proc) as popen, \
                mock.patch.object(evidence.OwnedChild, "bind", autospec=True, side_effect=bind), \
                mock.patch.object(evidence.signal, "pthread_sigmask") as mask:
            child = manager.spawn(["synthetic-not-executed"])
            self.assertIs(child, captured[0])
            self.assertTrue(child.identity)
            self.assertTrue(popen.call_args.kwargs["start_new_session"])
            self.assertNotIn("preexec_fn", popen.call_args.kwargs)
            mask.assert_not_called()
        with self.assertRaisesRegex(evidence.Refusal, "interrupted"):
            child.wait(1)
        with mock.patch.object(evidence.subprocess, "Popen") as popen:
            with self.assertRaisesRegex(evidence.Refusal, "interrupted"):
                manager.spawn(["synthetic-not-executed"])
            popen.assert_not_called()
        with mock.patch.object(evidence.select, "select") as select_call:
            with self.assertRaisesRegex(evidence.Refusal, "interrupted"):
                object.__new__(evidence.Bridge).pump(1)
            select_call.assert_not_called()
        with mock.patch.object(evidence, "PENDING_CANCELLATION", None), contextlib.ExitStack() as stack:
            listener = mock.Mock()
            listener.getsockname.return_value = ("127.0.0.1", 12345)
            def accepting():
                evidence.interrupted(evidence.signal.SIGHUP, None)
                raise evidence.socket.timeout()
            listener.accept.side_effect = accepting
            stack.enter_context(mock.patch.object(evidence.socket, "socket", return_value=listener))
            stack.enter_context(mock.patch.object(evidence, "read_json", return_value={"deadline": 10}))
            stack.enter_context(mock.patch.object(evidence, "bounded_read", return_value=b"synthetic archive"))
            stack.enter_context(mock.patch.object(evidence, "write_json"))
            stack.enter_context(mock.patch.object(evidence.time, "monotonic", return_value=0))
            with self.assertRaisesRegex(evidence.Refusal, "interrupted"):
                evidence.serve(Path("/unused-contract/spec.json"))
            listener.close.assert_called_once()

    def test_finalizer_preserves_only_verified_current_minimum_failure_bytes(self):
        for code in ("cleanup_incomplete", "interrupted", "capture_refused", "export_refused"):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                context = evidence.context_from_env(evidence_environment(root))
                evidence.write_json(root / "state.json", {"started": True})
                evidence.minimum_failure(root, context, code)
                before = [(root / name).read_bytes() for name in ("failure/receipt.json", "failure-decision.json")]
                with mock.patch.dict(os.environ, evidence_environment(root, "failure")), \
                        mock.patch.object(evidence, "export_outputs") as outputs:
                    self.assertEqual(evidence.finalize(root), 1)
                outputs.assert_called_once_with(False, True)
                self.assertEqual(before, [(root / name).read_bytes() for name in ("failure/receipt.json", "failure-decision.json")])
                self.assertEqual(evidence.verify_failure(root, context)["code"], code)
        for mutation in ("run_id", "attempt", "job", "auxiliary_sha", "source_sha", "source_tree", "digest", "extra", "code"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                context = evidence.context_from_env(evidence_environment(root))
                evidence.write_json(root / "state.json", {"started": True})
                evidence.minimum_failure(root, context, "cleanup_incomplete")
                decision = evidence.read_json(root / "failure-decision.json")
                if mutation in ("extra", "code"):
                    value = evidence.read_json(root / "failure/receipt.json")
                    if mutation == "extra": value["output"] = "synthetic-private-value"
                    else: value["code"] = "synthetic-private-value"
                    evidence.write_json(root / "failure/receipt.json", value, replace=True)
                    decision["receipt_sha256"] = evidence.sha((root / "failure/receipt.json").read_bytes())
                elif mutation == "digest": decision["receipt_sha256"] = "0" * 64
                else: decision["binding"][mutation] = "wrong"
                evidence.write_json(root / "failure-decision.json", decision, replace=True)
                with mock.patch.dict(os.environ, evidence_environment(root, "failure")), \
                        mock.patch.object(evidence, "export_outputs") as outputs:
                    self.assertEqual(evidence.finalize(root), 1)
                outputs.assert_called_once_with(False, True)
                self.assertEqual(evidence.verify_failure(root, context)["code"], "finalizer_refused")
                self.assertNotIn(b"synthetic-private-value", (root / "failure/receipt.json").read_bytes())

    def test_finalizer_fallback_recheck_and_write_faults_never_enable_unchecked_upload(self):
        for fault in ("missing", "recheck", "write", "revoke"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "quickstart-cli-evidence-123-1"
                root.mkdir(mode=0o700)
                context = evidence.context_from_env(evidence_environment(root))
                verify, calls = evidence.verify_failure, []
                def checking(*args):
                    calls.append(True)
                    if fault == "recheck" and len(calls) == 3:
                        raise OSError("controlled recheck fault")
                    return verify(*args)
                with mock.patch.dict(os.environ, evidence_environment(root, "failure")), \
                        mock.patch.object(evidence, "verify_failure", side_effect=checking), \
                        mock.patch.object(evidence, "export_outputs") as outputs, contextlib.ExitStack() as stack:
                    if fault == "write":
                        stack.enter_context(mock.patch.object(evidence, "minimum_failure", side_effect=OSError))
                    if fault == "revoke":
                        stack.enter_context(mock.patch.object(evidence, "revoke_success", side_effect=OSError))
                    self.assertEqual(evidence.finalize(root), 1)
                if fault == "missing":
                    outputs.assert_called_once_with(False, True)
                    self.assertEqual(verify(root, context)["code"], "finalizer_refused")
                else:
                    outputs.assert_not_called()
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(evidence, "PENDING_CANCELLATION", None):
            root = Path(directory) / "quickstart-cli-evidence-123-1"
            root.mkdir(mode=0o700)
            capture = export_fixture(root)
            with mock.patch.object(evidence, "snapshot", return_value="a" * 64):
                self.assertEqual(capture.run(), 0)
            def output(success, failure):
                if success:
                    evidence.interrupted(evidence.signal.SIGINT, None)
            with mock.patch.dict(os.environ, evidence_environment(root)), \
                    mock.patch.object(evidence, "export_outputs", side_effect=output) as outputs:
                self.assertEqual(evidence.finalize(root), 1)
            self.assertEqual(outputs.call_args, mock.call(False, True))
            self.assertFalse((root / "export-decision.json").exists())
            self.assertFalse(evidence.verify_failure(root, capture.context)["execution_proof"])

    def test_snapshot_git_oid_compatibility_keeps_independent_sha256_and_overlay_refusal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = b"hello\n"
            oid = "ce013625030ba8dba906f756967f9e9ca394464a"
            path = root / "known.txt"
            evidence.private_write(path, raw)
            index = ("100644 " + oid + " 0\tknown.txt\0").encode()
            with mock.patch.object(evidence, "git", return_value=index):
                actual = evidence.snapshot(None, root)
                wanted = evidence.sha(evidence.json.dumps(
                    [("known.txt", "100644", oid, evidence.sha(raw))], separators=(",", ":")).encode())
                self.assertEqual(actual, wanted)
                path.write_bytes(b"private overlay\n")
                with self.assertRaisesRegex(evidence.Refusal, "source_overlay"):
                    evidence.snapshot(None, root)


if __name__ == "__main__":
    unittest.main()
