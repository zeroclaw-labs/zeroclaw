#!/usr/bin/env python3
"""Behavioral tests for CI helper scripts under scripts/ci."""

from __future__ import annotations

import contextlib
import hashlib
import http.server
import json
import os
import shutil
import socket
import socketserver
import subprocess
import tempfile
import textwrap
import threading
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPTS_DIR = ROOT / "scripts" / "ci"
ANDROID_SCRIPTS_DIR = ROOT / "scripts" / "android"


def run_cmd(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


class _LocalProbeHandler(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self) -> None:  # noqa: N802
        if self.path == "/head-fallback":
            self.send_response(405)
            self.end_headers()
            return
        self.send_response(200)
        self.end_headers()

    def do_GET(self) -> None:  # noqa: N802
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def do_POST(self) -> None:  # noqa: N802
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, fmt: str, *args: object) -> None:  # pragma: no cover
        # Keep unit test output deterministic and quiet.
        return


@contextlib.contextmanager
def local_http_server() -> tuple[str, int]:
    class _ThreadedServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
        allow_reuse_address = True

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        host, port = sock.getsockname()

    server = _ThreadedServer((host, port), _LocalProbeHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield (host, port)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


class CiScriptsBehaviorTest(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="zc-ci-tests-"))
        self.addCleanup(lambda: shutil.rmtree(self.tmp, ignore_errors=True))

    def _script(self, name: str) -> str:
        return str(SCRIPTS_DIR / name)

    def _android_script(self, name: str) -> str:
        return str(ANDROID_SCRIPTS_DIR / name)

    def test_android_selfcheck_help_mentions_modes(self) -> None:
        proc = run_cmd(["bash", self._android_script("termux_source_build_check.sh"), "--help"])
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        self.assertIn("--mode <auto|termux-native|ndk-cross>", proc.stdout)
        self.assertIn("--diagnose-log <p>", proc.stdout)
        self.assertIn("--json-output <p|-]", proc.stdout)
        self.assertIn("--quiet", proc.stdout)
        self.assertIn("--strict", proc.stdout)

    def test_android_selfcheck_diagnose_log_ndk_cross(self) -> None:
        log_path = self.tmp / "android-failure.log"
        log_path.write_text(
            textwrap.dedent(
                """
                error occurred in cc-rs: failed to find tool "aarch64-linux-android-clang": No such file or directory (os error 2)
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "ndk-cross",
                "--diagnose-log",
                str(log_path),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        combined = f"{proc.stdout}\n{proc.stderr}"
        self.assertIn("detected cc-rs compiler lookup failure", combined)
        self.assertIn("export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", combined)
        self.assertIn("export CC_aarch64_linux_android", combined)

    def test_android_selfcheck_diagnose_log_termux_native(self) -> None:
        log_path = self.tmp / "android-failure-termux.log"
        log_path.write_text(
            textwrap.dedent(
                """
                error occurred in cc-rs: failed to find tool "aarch64-linux-android-clang": No such file or directory (os error 2)
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "termux-native",
                "--diagnose-log",
                str(log_path),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        combined = f"{proc.stdout}\n{proc.stderr}"
        self.assertIn("suggested recovery (termux-native)", combined)
        self.assertIn("unset CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", combined)

    def test_android_selfcheck_json_output_on_diagnose_success(self) -> None:
        log_path = self.tmp / "android-failure-json.log"
        json_path = self.tmp / "android-selfcheck.json"
        log_path.write_text(
            textwrap.dedent(
                """
                error occurred in cc-rs: failed to find tool "aarch64-linux-android-clang": No such file or directory (os error 2)
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "ndk-cross",
                "--diagnose-log",
                str(log_path),
                "--json-output",
                str(json_path),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(json_path.read_text(encoding="utf-8"))
        self.assertEqual(report["schema_version"], "zeroclaw.android-selfcheck.v1")
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["error_code"], "NONE")
        self.assertFalse(report["strict_mode"])
        self.assertEqual(report["target"], "aarch64-linux-android")
        self.assertEqual(report["mode_effective"], "ndk-cross")
        self.assertTrue(any("cc-rs compiler lookup failure" in x for x in report["detections"]))
        self.assertIn("CC_RS_TOOL_NOT_FOUND", report["detection_codes"])
        self.assertTrue(any("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER" in x for x in report["suggestions"]))

    def test_android_selfcheck_json_output_on_missing_diagnose_log(self) -> None:
        missing_log = self.tmp / "missing.log"
        json_path = self.tmp / "android-selfcheck-error.json"
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "ndk-cross",
                "--diagnose-log",
                str(missing_log),
                "--json-output",
                str(json_path),
            ]
        )
        self.assertEqual(proc.returncode, 1)
        report = json.loads(json_path.read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "error")
        self.assertEqual(report["exit_code"], 1)
        self.assertEqual(report["error_code"], "MISSING_DIAGNOSE_LOG")
        self.assertIn("does not exist", report["error_message"])

    def test_android_selfcheck_json_stdout_mode(self) -> None:
        log_path = self.tmp / "android-failure-stdout.log"
        log_path.write_text(
            textwrap.dedent(
                """
                error occurred in cc-rs: failed to find tool "aarch64-linux-android-clang": No such file or directory (os error 2)
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "ndk-cross",
                "--diagnose-log",
                str(log_path),
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(proc.stdout)
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["mode_effective"], "ndk-cross")

    def test_android_selfcheck_strict_fails_when_warnings_present(self) -> None:
        log_path = self.tmp / "android-failure-strict.log"
        json_path = self.tmp / "android-selfcheck-strict-error.json"
        log_path.write_text(
            textwrap.dedent(
                """
                error occurred in cc-rs: failed to find tool "aarch64-linux-android-clang": No such file or directory (os error 2)
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "ndk-cross",
                "--diagnose-log",
                str(log_path),
                "--json-output",
                str(json_path),
                "--strict",
            ]
        )
        self.assertEqual(proc.returncode, 1)
        report = json.loads(json_path.read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "error")
        self.assertEqual(report["error_code"], "STRICT_WARNINGS")
        self.assertTrue(report["strict_mode"])
        self.assertGreater(report["warning_count"], 0)

    def test_android_selfcheck_strict_passes_without_warnings(self) -> None:
        log_path = self.tmp / "android-clean-strict.log"
        json_path = self.tmp / "android-selfcheck-strict-ok.json"
        log_path.write_text("build completed cleanly\n", encoding="utf-8")
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--target",
                "aarch64-linux-android",
                "--mode",
                "ndk-cross",
                "--diagnose-log",
                str(log_path),
                "--json-output",
                str(json_path),
                "--strict",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(json_path.read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "ok")
        self.assertEqual(report["error_code"], "NONE")
        self.assertEqual(report["warning_count"], 0)
        self.assertTrue(report["strict_mode"])

    def test_android_selfcheck_bad_argument_reports_error_code(self) -> None:
        json_path = self.tmp / "android-selfcheck-bad-arg.json"
        proc = run_cmd(
            [
                "bash",
                self._android_script("termux_source_build_check.sh"),
                "--mode",
                "invalid-mode",
                "--json-output",
                str(json_path),
            ]
        )
        self.assertEqual(proc.returncode, 1)
        report = json.loads(json_path.read_text(encoding="utf-8"))
        self.assertEqual(report["status"], "error")
        self.assertEqual(report["error_code"], "BAD_ARGUMENT")

    def test_emit_audit_event_envelope(self) -> None:
        payload_path = self.tmp / "payload.json"
        output_path = self.tmp / "event.json"
        payload_path.write_text('{"status":"ok","value":42}\n', encoding="utf-8")

        proc = run_cmd(
            [
                "python3",
                self._script("emit_audit_event.py"),
                "--event-type",
                "unit_test_event",
                "--input-json",
                str(payload_path),
                "--output-json",
                str(output_path),
                "--artifact-name",
                "unit-test-artifact",
                "--retention-days",
                "14",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        event = json.loads(output_path.read_text(encoding="utf-8"))
        self.assertEqual(event["schema_version"], "zeroclaw.audit.v1")
        self.assertEqual(event["event_type"], "unit_test_event")
        self.assertIn("run_context", event)
        self.assertEqual(event["payload"]["status"], "ok")
        self.assertEqual(event["payload"]["value"], 42)
        self.assertEqual(event["artifact"]["name"], "unit-test-artifact")
        self.assertEqual(event["artifact"]["retention_days"], 14)

    def test_flake_retry_probe_blocks_when_flake_suspected(self) -> None:
        out_json = self.tmp / "flake.json"
        out_md = self.tmp / "flake.md"
        proc = run_cmd(
            [
                "python3",
                self._script("flake_retry_probe.py"),
                "--initial-result",
                "failure",
                "--retry-command",
                "python3 -c 'import sys; sys.exit(0)'",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--block-on-flake",
                "true",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["classification"], "flake_suspected")
        self.assertTrue(report["retry_attempted"])

    def test_flake_retry_probe_persistent_failure_non_blocking(self) -> None:
        out_json = self.tmp / "flake.json"
        out_md = self.tmp / "flake.md"
        proc = run_cmd(
            [
                "python3",
                self._script("flake_retry_probe.py"),
                "--initial-result",
                "failure",
                "--retry-command",
                "python3 -c 'import sys; sys.exit(2)'",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--block-on-flake",
                "false",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["classification"], "persistent_failure")

    def test_smoke_build_retry_retries_transient_143_once(self) -> None:
        fake_bin = self.tmp / "fake-bin"
        fake_bin.mkdir(parents=True, exist_ok=True)
        counter = self.tmp / "cargo-counter.txt"

        fake_cargo = fake_bin / "cargo"
        fake_cargo.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                counter="${FAKE_CARGO_COUNTER:?}"
                attempts=0
                if [ -f "$counter" ]; then
                  attempts="$(cat "$counter")"
                fi
                attempts="$((attempts + 1))"
                printf '%s' "$attempts" > "$counter"
                if [ "$attempts" -eq 1 ]; then
                  exit 143
                fi
                exit 0
                """
            ),
            encoding="utf-8",
        )
        fake_cargo.chmod(0o755)

        env = dict(os.environ)
        env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
        env["FAKE_CARGO_COUNTER"] = str(counter)
        env["CI_SMOKE_BUILD_ATTEMPTS"] = "2"

        proc = run_cmd(["bash", self._script("smoke_build_retry.sh")], env=env, cwd=ROOT)
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        self.assertEqual(counter.read_text(encoding="utf-8"), "2")
        self.assertIn("Retrying", proc.stdout)

    def test_smoke_build_retry_fails_immediately_on_non_retryable_code(self) -> None:
        fake_bin = self.tmp / "fake-bin"
        fake_bin.mkdir(parents=True, exist_ok=True)
        counter = self.tmp / "cargo-counter.txt"

        fake_cargo = fake_bin / "cargo"
        fake_cargo.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                counter="${FAKE_CARGO_COUNTER:?}"
                attempts=0
                if [ -f "$counter" ]; then
                  attempts="$(cat "$counter")"
                fi
                attempts="$((attempts + 1))"
                printf '%s' "$attempts" > "$counter"
                exit 101
                """
            ),
            encoding="utf-8",
        )
        fake_cargo.chmod(0o755)

        env = dict(os.environ)
        env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
        env["FAKE_CARGO_COUNTER"] = str(counter)
        env["CI_SMOKE_BUILD_ATTEMPTS"] = "3"

        proc = run_cmd(["bash", self._script("smoke_build_retry.sh")], env=env, cwd=ROOT)
        self.assertEqual(proc.returncode, 101)
        self.assertEqual(counter.read_text(encoding="utf-8"), "1")
        self.assertIn("failed with exit code 101", proc.stdout)

    def test_deny_policy_guard_detects_invalid_entries(self) -> None:
        deny_path = self.tmp / "deny.toml"
        deny_path.write_text(
            textwrap.dedent(
                """
                [advisories]
                ignore = [
                    { id = "RUSTSEC-2025-9999", reason = "short" },
                    "RUSTSEC-2024-0001",
                ]
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        out_json = self.tmp / "deny.json"
        out_md = self.tmp / "deny.md"
        proc = run_cmd(
            [
                "python3",
                self._script("deny_policy_guard.py"),
                "--deny-file",
                str(deny_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(len(report["violations"]), 2)

    def test_deny_policy_guard_passes_with_valid_governance(self) -> None:
        deny_path = self.tmp / "deny.toml"
        deny_path.write_text(
            textwrap.dedent(
                """
                [advisories]
                ignore = [
                    { id = "RUSTSEC-2025-0001", reason = "Tracked with mitigation plan while waiting upstream patch." },
                    { id = "RUSTSEC-2025-0002", reason = "Accepted transiently due to transitive dependency under migration." },
                ]
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        governance_path = self.tmp / "deny-governance.json"
        governance_path.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.deny-governance.v1",
                    "advisories": [
                        {
                            "id": "RUSTSEC-2025-0001",
                            "owner": "repo-maintainers",
                            "reason": "Tracked with mitigation plan while waiting upstream patch.",
                            "ticket": "RMN-21",
                            "expires_on": "2027-01-01",
                        },
                        {
                            "id": "RUSTSEC-2025-0002",
                            "owner": "repo-maintainers",
                            "reason": "Accepted transiently due to transitive dependency under migration.",
                            "ticket": "RMN-21",
                            "expires_on": "2027-01-01",
                        },
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "deny-governed.json"
        out_md = self.tmp / "deny-governed.md"
        proc = run_cmd(
            [
                "python3",
                self._script("deny_policy_guard.py"),
                "--deny-file",
                str(deny_path),
                "--governance-file",
                str(governance_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["warnings"], [])

    def test_deny_policy_guard_detects_unmanaged_or_expired_governance(self) -> None:
        deny_path = self.tmp / "deny.toml"
        deny_path.write_text(
            textwrap.dedent(
                """
                [advisories]
                ignore = [
                    { id = "RUSTSEC-2025-1111", reason = "Temporary ignore while upstream patch is under review." },
                    { id = "RUSTSEC-2025-2222", reason = "Temporary ignore while migration work is active." },
                ]
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        governance_path = self.tmp / "deny-governance.json"
        governance_path.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.deny-governance.v1",
                    "advisories": [
                        {
                            "id": "RUSTSEC-2025-1111",
                            "owner": "repo-maintainers",
                            "reason": "Temporary ignore while upstream patch is under review.",
                            "ticket": "RMN-21",
                            "expires_on": "2020-01-01",
                        }
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "deny-governed-invalid.json"
        out_md = self.tmp / "deny-governed-invalid.md"
        proc = run_cmd(
            [
                "python3",
                self._script("deny_policy_guard.py"),
                "--deny-file",
                str(deny_path),
                "--governance-file",
                str(governance_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("expired", joined)
        self.assertIn("has no governance metadata", joined)

    def test_secrets_governance_guard_passes_for_valid_metadata(self) -> None:
        gitleaks_path = self.tmp / ".gitleaks.toml"
        gitleaks_path.write_text(
            textwrap.dedent(
                r"""
                title = "test"
                [allowlist]
                paths = ['''src/security/leak_detector\.rs''']
                regexes = ['''Authorization: Bearer \$\{[^}]+\}''']
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        governance_path = self.tmp / "governance.json"
        governance_path.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.secrets-governance.v1",
                    "paths": [
                        {
                            "pattern": r"src/security/leak_detector\.rs",
                            "owner": "repo-maintainers",
                            "reason": "Fixture pattern used in secret scanning regression tests.",
                            "ticket": "RMN-13",
                            "expires_on": "2027-01-01",
                        }
                    ],
                    "regexes": [
                        {
                            "pattern": r"Authorization: Bearer \$\{[^}]+\}",
                            "owner": "repo-maintainers",
                            "reason": "Placeholder token pattern used in docs and snippets.",
                            "ticket": "RMN-13",
                            "expires_on": "2027-01-01",
                        }
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        out_json = self.tmp / "secrets.json"
        out_md = self.tmp / "secrets.md"
        proc = run_cmd(
            [
                "python3",
                self._script("secrets_governance_guard.py"),
                "--gitleaks-file",
                str(gitleaks_path),
                "--governance-file",
                str(governance_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["violations"], [])

    def test_secrets_governance_guard_detects_expired_or_unmanaged_entries(self) -> None:
        gitleaks_path = self.tmp / ".gitleaks.toml"
        gitleaks_path.write_text(
            textwrap.dedent(
                r"""
                title = "test"
                [allowlist]
                paths = ['''src/security/leak_detector\.rs''', '''docs/example\.md''']
                regexes = ['''Authorization: Bearer \$\{[^}]+\}''']
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        governance_path = self.tmp / "governance.json"
        governance_path.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.secrets-governance.v1",
                    "paths": [
                        {
                            "pattern": r"src/security/leak_detector\.rs",
                            "owner": "repo-maintainers",
                            "reason": "Fixture pattern used in secret scanning regression tests.",
                            "ticket": "RMN-13",
                            "expires_on": "2020-01-01",
                        }
                    ],
                    "regexes": [
                        {
                            "pattern": r"Authorization: Bearer \$\{[^}]+\}",
                            "owner": "repo-maintainers",
                            "reason": "Placeholder token pattern used in docs and snippets.",
                            "ticket": "RMN-13",
                            "expires_on": "2027-01-01",
                        }
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        out_json = self.tmp / "secrets.json"
        out_md = self.tmp / "secrets.md"
        proc = run_cmd(
            [
                "python3",
                self._script("secrets_governance_guard.py"),
                "--gitleaks-file",
                str(gitleaks_path),
                "--governance-file",
                str(governance_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        violation_text = "\n".join(report["violations"])
        self.assertIn("expired", violation_text)
        self.assertIn("no governance metadata", violation_text)

    def test_provider_connectivity_matrix_fail_on_critical_unreachable(self) -> None:
        with local_http_server() as (host, port):
            cfg = self.tmp / "providers.json"
            cfg.write_text(
                json.dumps(
                    {
                        "global_timeout_seconds": 2,
                        "providers": [
                            {
                                "id": "ok",
                                "url": f"http://{host}:{port}/ok",
                                "method": "GET",
                                "critical": True,
                            },
                            {
                                "id": "head-fallback",
                                "url": f"http://{host}:{port}/head-fallback",
                                "method": "HEAD",
                                "critical": False,
                            },
                            {
                                "id": "down",
                                "url": f"http://{host}:{port + 1}/down",
                                "method": "GET",
                                "critical": True,
                            },
                        ],
                    },
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )

            out_json = self.tmp / "matrix.json"
            out_md = self.tmp / "matrix.md"
            proc = run_cmd(
                [
                    "python3",
                    self._script("provider_connectivity_matrix.py"),
                    "--config",
                    str(cfg),
                    "--output-json",
                    str(out_json),
                    "--output-md",
                    str(out_md),
                    "--fail-on-critical",
                ]
            )
            self.assertEqual(proc.returncode, 3)
            report = json.loads(out_json.read_text(encoding="utf-8"))
            self.assertEqual(report["critical_failures"], 1)
            head_fallback = [r for r in report["rows"] if r["provider"] == "head-fallback"][0]
            self.assertTrue(head_fallback["reachable"])

    def test_generate_provenance_contains_subject_digest(self) -> None:
        artifact = self.tmp / "artifact.bin"
        artifact.write_bytes(b"zeroclaw-provenance-test")
        out = self.tmp / "provenance.json"
        proc = run_cmd(
            [
                "python3",
                self._script("generate_provenance.py"),
                "--artifact",
                str(artifact),
                "--subject-name",
                "artifact-test",
                "--output",
                str(out),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        statement = json.loads(out.read_text(encoding="utf-8"))
        digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
        self.assertEqual(statement["subject"][0]["digest"]["sha256"], digest)
        self.assertEqual(statement["subject"][0]["name"], "artifact-test")

    def test_rollback_guard_resolves_latest_tag(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        notes = repo / "notes.txt"
        notes.write_text("v1\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "v1"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v1.0.0", "-m", "v1.0.0"], cwd=repo)

        notes.write_text("v2\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "v2"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v1.1.0", "-m", "v1.1.0"], cwd=repo)

        notes.write_text("head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "head"], cwd=repo)

        out_json = self.tmp / "rollback.json"
        out_md = self.tmp / "rollback.md"
        proc = run_cmd(
            [
                "python3",
                self._script("rollback_guard.py"),
                "--repo-root",
                str(repo),
                "--branch",
                "dev",
                "--mode",
                "dry-run",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["target_ref"], "v1.1.0")
        self.assertEqual(report["ancestor_check"], "pass")
        self.assertFalse(report["ready_to_execute"])

    def test_rollback_guard_rejects_non_ancestor_target(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        notes = repo / "notes.txt"
        notes.write_text("base\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()
        main_branch = run_cmd(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=repo).stdout.strip()

        notes.write_text("main-head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "main"], cwd=repo)

        run_cmd(["git", "checkout", "-b", "side", base_sha], cwd=repo)
        notes.write_text("side-head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "side"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v9.9.9-side", "-m", "v9.9.9-side"], cwd=repo)
        run_cmd(["git", "checkout", main_branch], cwd=repo)

        out_json = self.tmp / "rollback-invalid.json"
        out_md = self.tmp / "rollback-invalid.md"
        proc = run_cmd(
            [
                "python3",
                self._script("rollback_guard.py"),
                "--repo-root",
                str(repo),
                "--branch",
                "dev",
                "--mode",
                "execute",
                "--target-ref",
                "v9.9.9-side",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["ancestor_check"], "fail")
        self.assertGreaterEqual(len(report["violations"]), 1)

    def test_rollback_guard_allow_non_ancestor_mode(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        notes = repo / "notes.txt"
        notes.write_text("base\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()
        main_branch = run_cmd(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=repo).stdout.strip()

        notes.write_text("main-head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "main"], cwd=repo)

        run_cmd(["git", "checkout", "-b", "side", base_sha], cwd=repo)
        notes.write_text("side-head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "side"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v9.9.9-side", "-m", "v9.9.9-side"], cwd=repo)
        run_cmd(["git", "checkout", main_branch], cwd=repo)

        out_json = self.tmp / "rollback-warning.json"
        out_md = self.tmp / "rollback-warning.md"
        proc = run_cmd(
            [
                "python3",
                self._script("rollback_guard.py"),
                "--repo-root",
                str(repo),
                "--branch",
                "dev",
                "--mode",
                "execute",
                "--target-ref",
                "v9.9.9-side",
                "--allow-non-ancestor",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["ancestor_check"], "fail")
        self.assertEqual(report["violations"], [])
        self.assertGreaterEqual(len(report["warnings"]), 1)
        self.assertTrue(report["ready_to_execute"])

    def test_rollback_guard_invalid_target_ref_reports_violation(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        notes = repo / "notes.txt"
        notes.write_text("base\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)

        out_json = self.tmp / "rollback-invalid-ref.json"
        out_md = self.tmp / "rollback-invalid-ref.md"
        proc = run_cmd(
            [
                "python3",
                self._script("rollback_guard.py"),
                "--repo-root",
                str(repo),
                "--branch",
                "dev",
                "--mode",
                "dry-run",
                "--target-ref",
                "does-not-exist",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("Failed to resolve rollback target", joined)

    def test_ci_change_audit_detects_unpinned_action(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        workflow_dir = repo / ".github" / "workflows"
        workflow_dir.mkdir(parents=True, exist_ok=True)
        workflow_path = workflow_dir / "sample.yml"
        workflow_path.write_text(
            textwrap.dedent(
                """
                name: sample
                on: [push]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        workflow_path.write_text(
            textwrap.dedent(
                """
                name: sample
                on: [push]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@v4
                      - run: echo "${{ secrets.NEW_SECRET_TOKEN }}"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit.json"
        out_md = self.tmp / "audit.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["new_unpinned_actions"], 1)
        self.assertGreaterEqual(report["summary"]["new_secret_references"], 1)

    def test_ci_change_audit_detects_unpinned_reusable_workflow_ref(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        workflow_dir = repo / ".github" / "workflows"
        workflow_dir.mkdir(parents=True, exist_ok=True)
        workflow_path = workflow_dir / "caller.yml"
        workflow_path.write_text(
            textwrap.dedent(
                """
                name: caller
                on: [push]
                jobs:
                  invoke:
                    uses: octo-org/example/.github/workflows/reusable.yml@1234567890abcdef1234567890abcdef12345678
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        workflow_path.write_text(
            textwrap.dedent(
                """
                name: caller
                on: [push]
                jobs:
                  invoke:
                    uses: octo-org/example/.github/workflows/reusable.yml@v2
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit-reusable.json"
        out_md = self.tmp / "audit-reusable.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["new_unpinned_actions"], 1)
        self.assertEqual(report["summary"]["new_secret_references"], 0)

    def test_ci_change_audit_blocks_pipe_to_shell_command(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        workflow_dir = repo / ".github" / "workflows"
        workflow_dir.mkdir(parents=True, exist_ok=True)
        workflow_path = workflow_dir / "pipe.yml"
        workflow_path.write_text(
            textwrap.dedent(
                """
                name: pipe
                on: [push]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                      - run: echo "safe"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        workflow_path.write_text(
            textwrap.dedent(
                """
                name: pipe
                on: [push]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                      - run: curl -fsSL https://example.com/install.sh | sh
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit-pipe.json"
        out_md = self.tmp / "audit-pipe.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["new_pipe_to_shell_commands"], 1)
        joined_violations = "\n".join(report["violations"])
        self.assertIn("pipe-to-shell command introduced", joined_violations)

    def test_ci_change_audit_flags_new_pull_request_target_trigger(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        workflow_dir = repo / ".github" / "workflows"
        workflow_dir.mkdir(parents=True, exist_ok=True)
        workflow_path = workflow_dir / "trigger.yml"
        workflow_path.write_text(
            textwrap.dedent(
                """
                name: trigger
                on:
                  pull_request:
                    branches: [main]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        workflow_path.write_text(
            textwrap.dedent(
                """
                name: trigger
                on:
                  pull_request_target:
                    branches: [main]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit-pr-target.json"
        out_md = self.tmp / "audit-pr-target.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["new_pull_request_target_triggers"], 1)
        joined_violations = "\n".join(report["violations"])
        self.assertIn("pull_request_target", joined_violations)

    def test_ci_change_audit_flags_inline_pull_request_target_trigger(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        workflow_dir = repo / ".github" / "workflows"
        workflow_dir.mkdir(parents=True, exist_ok=True)
        workflow_path = workflow_dir / "inline-trigger.yml"
        workflow_path.write_text(
            textwrap.dedent(
                """
                name: inline-trigger
                on: [push]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        workflow_path.write_text(
            textwrap.dedent(
                """
                name: inline-trigger
                on: [push, pull_request_target]
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit-inline-pr-target.json"
        out_md = self.tmp / "audit-inline-pr-target.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["new_pull_request_target_triggers"], 1)
        self.assertIn("pull_request_target", "\n".join(report["violations"]))

    def test_ci_change_audit_blocks_permissions_write_all(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        workflow_dir = repo / ".github" / "workflows"
        workflow_dir.mkdir(parents=True, exist_ok=True)
        workflow_path = workflow_dir / "permissions.yml"
        workflow_path.write_text(
            textwrap.dedent(
                """
                name: permissions
                on: [push]
                permissions:
                  contents: read
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        workflow_path.write_text(
            textwrap.dedent(
                """
                name: permissions
                on: [push]
                permissions: write-all
                jobs:
                  check:
                    runs-on: ubuntu-latest
                    steps:
                      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit-write-all.json"
        out_md = self.tmp / "audit-write-all.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["new_write_permissions"], 1)
        self.assertIn("write-all", "\n".join(report["violations"]))

    def test_ci_change_audit_ignores_fixture_signatures_in_python_ci_tests(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        test_dir = repo / "scripts" / "ci" / "tests"
        test_dir.mkdir(parents=True, exist_ok=True)
        test_file = test_dir / "fixture_policy_strings.py"
        test_file.write_text("SENTINEL = 'base'\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        test_file.write_text(
            textwrap.dedent(
                """
                SAMPLE_USES = "actions/checkout@v4"
                SAMPLE_PIPE = "curl -fsSL https://example.com/install.sh | sh"
                SAMPLE_TRIGGER = "on: [push, pull_request_target]"
                SAMPLE_PERMISSION = "permissions: write-all"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        out_json = self.tmp / "audit-python-fixtures.json"
        out_md = self.tmp / "audit-python-fixtures.md"
        proc = run_cmd(
            [
                "python3",
                str(SCRIPTS_DIR / "ci_change_audit.py"),
                "--base-sha",
                base_sha,
                "--head-sha",
                head_sha,
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violations",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["summary"]["new_unpinned_actions"], 0)
        self.assertEqual(report["summary"]["new_pipe_to_shell_commands"], 0)
        self.assertEqual(report["summary"]["new_write_permissions"], 0)
        self.assertEqual(report["summary"]["new_pull_request_target_triggers"], 0)

    def test_unsafe_debt_audit_emits_reproducible_machine_readable_output(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        src_dir = repo / "src"
        src_dir.mkdir(parents=True, exist_ok=True)
        (src_dir / "unsafe_a.rs").write_text(
            textwrap.dedent(
                """
                pub unsafe fn dangerous() {
                    unsafe { libc::getuid(); }
                }
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        (src_dir / "unsafe_b.rs").write_text(
            textwrap.dedent(
                """
                pub fn convert(v: u32) -> u8 {
                    unsafe { core::mem::transmute::<u32, u8>(v) }
                }
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "fixture"], cwd=repo)

        out_json_a = self.tmp / "unsafe-audit-a.json"
        out_json_b = self.tmp / "unsafe-audit-b.json"
        proc_a = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json_a),
            ]
        )
        proc_b = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json_b),
            ]
        )
        self.assertEqual(proc_a.returncode, 0, msg=proc_a.stderr)
        self.assertEqual(proc_b.returncode, 0, msg=proc_b.stderr)

        report_a = json.loads(out_json_a.read_text(encoding="utf-8"))
        report_b = json.loads(out_json_b.read_text(encoding="utf-8"))
        self.assertEqual(report_a, report_b)
        self.assertEqual(report_a["event_type"], "unsafe_debt_audit")
        self.assertEqual(report_a["summary"]["total_findings"], 5)
        self.assertEqual(report_a["summary"]["by_pattern"]["unsafe_block"], 2)
        self.assertEqual(report_a["summary"]["by_pattern"]["unsafe_fn"], 1)
        self.assertEqual(report_a["summary"]["by_pattern"]["ffi_libc_call"], 1)
        self.assertEqual(report_a["summary"]["by_pattern"]["mem_transmute"], 1)
        self.assertEqual(report_a["source"]["mode"], "git_ls_files")
        self.assertEqual(report_a["source"]["crate_roots_scanned"], 0)

    def test_unsafe_debt_audit_fail_on_findings(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        (repo / "src").mkdir(parents=True, exist_ok=True)
        (repo / "src" / "unsafe_one.rs").write_text(
            "pub fn whoami() -> bool { unsafe { libc::getuid() == 0 } }\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "unsafe-fail.json"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json),
                "--fail-on-findings",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertGreaterEqual(report["summary"]["total_findings"], 1)

    def test_unsafe_debt_audit_detects_missing_crate_unsafe_guard(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        (repo / "Cargo.toml").write_text(
            textwrap.dedent(
                """
                [package]
                name = "fixture-missing-guard"
                version = "0.1.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        (repo / "src").mkdir(parents=True, exist_ok=True)
        (repo / "src" / "lib.rs").write_text(
            "pub fn version() -> &'static str { \"v1\" }\n",
            encoding="utf-8",
        )

        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "fixture"], cwd=repo)

        out_json = self.tmp / "unsafe-missing-guard.json"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["source"]["crate_roots_scanned"], 1)
        self.assertEqual(report["summary"]["total_findings"], 1)
        self.assertEqual(report["summary"]["by_pattern"]["missing_crate_unsafe_guard"], 1)
        finding = report["findings"][0]
        self.assertEqual(finding["pattern_id"], "missing_crate_unsafe_guard")
        self.assertEqual(finding["path"], "src/lib.rs")

    def test_unsafe_debt_audit_accepts_crate_with_unsafe_guard(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        (repo / "Cargo.toml").write_text(
            textwrap.dedent(
                """
                [package]
                name = "fixture-with-guard"
                version = "0.1.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        (repo / "src").mkdir(parents=True, exist_ok=True)
        (repo / "src" / "lib.rs").write_text(
            textwrap.dedent(
                """
                #![forbid(unsafe_code)]
                pub fn version() -> &'static str { "v2" }
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )

        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "fixture"], cwd=repo)

        out_json = self.tmp / "unsafe-with-guard.json"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json),
                "--fail-on-findings",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["source"]["crate_roots_scanned"], 1)
        self.assertEqual(report["summary"]["total_findings"], 0)

    def test_unsafe_debt_audit_policy_file_ignores_pattern_findings(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        (repo / "src").mkdir(parents=True, exist_ok=True)
        (repo / "src" / "unsafe_one.rs").write_text(
            "pub fn whoami() -> bool { unsafe { libc::getuid() == 0 } }\n",
            encoding="utf-8",
        )

        policy_path = repo / "scripts" / "ci" / "config"
        policy_path.mkdir(parents=True, exist_ok=True)
        (policy_path / "unsafe_debt_policy.toml").write_text(
            textwrap.dedent(
                """
                [audit]
                include_paths = ["src"]
                ignore_pattern_ids = ["unsafe_block", "ffi_libc_call"]
                enforce_crate_unsafe_guard = false
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "unsafe-policy-ignore.json"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json),
                "--fail-on-findings",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["source"]["policy_file"], "scripts/ci/config/unsafe_debt_policy.toml")
        self.assertEqual(report["summary"]["total_findings"], 0)

    def test_unsafe_debt_audit_fails_on_excluded_crate_roots_policy(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        (repo / "Cargo.toml").write_text(
            textwrap.dedent(
                """
                [package]
                name = "top-crate"
                version = "0.1.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        (repo / "src").mkdir(parents=True, exist_ok=True)
        (repo / "src" / "lib.rs").write_text(
            "#![forbid(unsafe_code)]\npub fn top() {}\n",
            encoding="utf-8",
        )

        (repo / "firmware" / "sensor").mkdir(parents=True, exist_ok=True)
        (repo / "firmware" / "sensor" / "Cargo.toml").write_text(
            textwrap.dedent(
                """
                [package]
                name = "sensor-crate"
                version = "0.1.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        (repo / "firmware" / "sensor" / "src").mkdir(parents=True, exist_ok=True)
        (repo / "firmware" / "sensor" / "src" / "lib.rs").write_text(
            "#![forbid(unsafe_code)]\npub fn sensor() {}\n",
            encoding="utf-8",
        )

        policy_dir = repo / "scripts" / "ci" / "config"
        policy_dir.mkdir(parents=True, exist_ok=True)
        (policy_dir / "unsafe_debt_policy.toml").write_text(
            textwrap.dedent(
                """
                [audit]
                include_paths = ["src", "crates", "tests", "benches", "fuzz"]
                fail_on_excluded_crate_roots = true
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )

        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "fixture"], cwd=repo)

        out_json = self.tmp / "unsafe-excluded-roots.json"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_debt_audit.py"),
                "--repo-root",
                str(repo),
                "--output-json",
                str(out_json),
            ]
        )
        self.assertEqual(proc.returncode, 4)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["source"]["crate_roots_total"], 2)
        self.assertEqual(report["source"]["crate_roots_scanned"], 1)
        self.assertEqual(report["source"]["crate_roots_excluded"], 1)
        self.assertIn("firmware/sensor/src/lib.rs", report["source"]["excluded_crate_roots"])

    def test_unsafe_policy_guard_passes_for_valid_governance(self) -> None:
        policy_path = self.tmp / "unsafe_debt_policy.toml"
        policy_path.write_text(
            textwrap.dedent(
                """
                [audit]
                ignore_paths = ["legacy/vendor"]
                ignore_pattern_ids = ["ffi_libc_call"]
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        governance_path = self.tmp / "unsafe-governance.json"
        governance_path.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.unsafe-audit-governance.v1",
                    "ignore_paths": [
                        {
                            "path": "legacy/vendor",
                            "owner": "repo-maintainers",
                            "reason": "Temporary vendor mirror while upstream replaces unsafe bindings.",
                            "ticket": "RMN-32",
                            "expires_on": "2027-01-01",
                        }
                    ],
                    "ignore_pattern_ids": [
                        {
                            "pattern_id": "ffi_libc_call",
                            "owner": "repo-maintainers",
                            "reason": "Allowlisted for libc shim crate pending migration to safer wrappers.",
                            "ticket": "RMN-32",
                            "expires_on": "2027-01-01",
                        }
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "unsafe-policy-guard.json"
        out_md = self.tmp / "unsafe-policy-guard.md"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_policy_guard.py"),
                "--policy-file",
                str(policy_path),
                "--governance-file",
                str(governance_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["unmanaged_paths"], [])
        self.assertEqual(report["unmanaged_pattern_ids"], [])

    def test_unsafe_policy_guard_detects_expired_or_unmanaged_entries(self) -> None:
        policy_path = self.tmp / "unsafe_debt_policy.toml"
        policy_path.write_text(
            textwrap.dedent(
                """
                [audit]
                ignore_paths = ["legacy/vendor", "legacy/temp"]
                ignore_pattern_ids = ["ffi_libc_call", "unknown_pattern"]
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        governance_path = self.tmp / "unsafe-governance.json"
        governance_path.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.unsafe-audit-governance.v1",
                    "ignore_paths": [
                        {
                            "path": "legacy/vendor",
                            "owner": "repo-maintainers",
                            "reason": "Temporary vendor mirror while upstream replaces unsafe bindings.",
                            "ticket": "RMN-32",
                            "expires_on": "2020-01-01",
                        }
                    ],
                    "ignore_pattern_ids": [
                        {
                            "pattern_id": "ffi_libc_call",
                            "owner": "repo-maintainers",
                            "reason": "Allowlisted for libc shim crate pending migration to safer wrappers.",
                            "ticket": "RMN-32",
                            "expires_on": "2027-01-01",
                        }
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "unsafe-policy-guard-invalid.json"
        out_md = self.tmp / "unsafe-policy-guard-invalid.md"
        proc = run_cmd(
            [
                "python3",
                self._script("unsafe_policy_guard.py"),
                "--policy-file",
                str(policy_path),
                "--governance-file",
                str(governance_path),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        violation_text = "\n".join(report["violations"])
        self.assertIn("expired", violation_text)
        self.assertIn("unknown", violation_text)
        self.assertIn("no governance metadata", violation_text)

    def test_release_manifest_generates_checksums_and_report(self) -> None:
        artifacts = self.tmp / "artifacts"
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "zeroclaw-x86_64-unknown-linux-gnu.tar.gz").write_bytes(b"release-asset")
        (artifacts / "zeroclaw.cdx.json").write_text('{"sbom":"ok"}\n', encoding="utf-8")
        (artifacts / "LICENSE-APACHE").write_text("license\n", encoding="utf-8")

        out_json = self.tmp / "release-manifest.json"
        out_md = self.tmp / "release-manifest.md"
        checksums = self.tmp / "SHA256SUMS"
        proc = run_cmd(
            [
                "python3",
                self._script("release_manifest.py"),
                "--artifacts-dir",
                str(artifacts),
                "--release-tag",
                "v0.2.0-rc.1",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--checksums-path",
                str(checksums),
                "--fail-empty",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["release_tag"], "v0.2.0-rc.1")
        self.assertGreaterEqual(len(report["files"]), 3)
        self.assertIn("zeroclaw-x86_64-unknown-linux-gnu.tar.gz", checksums.read_text(encoding="utf-8"))

    def test_release_notes_supply_chain_refs_generates_release_preface(self) -> None:
        artifacts = self.tmp / "artifacts"
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "release-manifest.json").write_text('{"ok":true}\n', encoding="utf-8")
        (artifacts / "release-manifest.md").write_text("# manifest\n", encoding="utf-8")
        (artifacts / "SHA256SUMS").write_text("abc  file\n", encoding="utf-8")
        (artifacts / "zeroclaw.cdx.json").write_text('{"sbom":"cdx"}\n', encoding="utf-8")
        (artifacts / "zeroclaw.spdx.json").write_text('{"sbom":"spdx"}\n', encoding="utf-8")
        (artifacts / "zeroclaw.sha256sums.intoto.json").write_text('{"_type":"statement"}\n', encoding="utf-8")
        (artifacts / "audit-event-release-sha256sums-provenance.json").write_text(
            '{"schema_version":"zeroclaw.audit.v1"}\n',
            encoding="utf-8",
        )
        (artifacts / "release-artifact-guard.publish.json").write_text('{"ready":true}\n', encoding="utf-8")
        (artifacts / "audit-event-release-artifact-guard-publish.json").write_text(
            '{"schema_version":"zeroclaw.audit.v1"}\n',
            encoding="utf-8",
        )
        (artifacts / "SHA256SUMS.sig").write_text("sig\n", encoding="utf-8")
        (artifacts / "SHA256SUMS.pem").write_text("pem\n", encoding="utf-8")
        (artifacts / "SHA256SUMS.sigstore.json").write_text('{"bundle":"ok"}\n', encoding="utf-8")
        trigger_dir = artifacts / "release-trigger-guard"
        trigger_dir.mkdir(parents=True, exist_ok=True)
        (trigger_dir / "release-trigger-guard.json").write_text('{"ready":true}\n', encoding="utf-8")
        (trigger_dir / "audit-event-release-trigger-guard.json").write_text(
            '{"schema_version":"zeroclaw.audit.v1"}\n',
            encoding="utf-8",
        )

        out_json = self.tmp / "release-notes-supply-chain.json"
        out_md = self.tmp / "release-notes-supply-chain.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_notes_with_supply_chain_refs.py"),
                "--artifacts-dir",
                str(artifacts),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--release-tag",
                "v1.2.3",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-missing",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertTrue(report["ready"])
        self.assertEqual(report["violations"], [])
        sbom_url = report["references"]["sbom_cyclonedx"]["url"]
        self.assertIn("/releases/download/v1.2.3/zeroclaw.cdx.json", sbom_url)
        body = out_md.read_text(encoding="utf-8")
        self.assertIn("Supply-Chain Evidence", body)
        self.assertIn("Automated Commit Notes", body)

    def test_release_notes_supply_chain_refs_fails_on_missing_required_file(self) -> None:
        artifacts = self.tmp / "artifacts"
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "release-manifest.json").write_text('{"ok":true}\n', encoding="utf-8")
        (artifacts / "release-manifest.md").write_text("# manifest\n", encoding="utf-8")
        (artifacts / "SHA256SUMS").write_text("abc  file\n", encoding="utf-8")
        (artifacts / "zeroclaw.cdx.json").write_text('{"sbom":"cdx"}\n', encoding="utf-8")
        (artifacts / "zeroclaw.spdx.json").write_text('{"sbom":"spdx"}\n', encoding="utf-8")
        (artifacts / "zeroclaw.sha256sums.intoto.json").write_text('{"_type":"statement"}\n', encoding="utf-8")
        (artifacts / "release-trigger-guard.json").write_text('{"ready":true}\n', encoding="utf-8")
        (artifacts / "audit-event-release-trigger-guard.json").write_text(
            '{"schema_version":"zeroclaw.audit.v1"}\n',
            encoding="utf-8",
        )
        (artifacts / "release-artifact-guard.publish.json").write_text('{"ready":true}\n', encoding="utf-8")
        (artifacts / "audit-event-release-artifact-guard-publish.json").write_text(
            '{"schema_version":"zeroclaw.audit.v1"}\n',
            encoding="utf-8",
        )

        out_json = self.tmp / "release-notes-supply-chain.missing.json"
        out_md = self.tmp / "release-notes-supply-chain.missing.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_notes_with_supply_chain_refs.py"),
                "--artifacts-dir",
                str(artifacts),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--release-tag",
                "v1.2.3",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-missing",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertFalse(report["ready"])
        self.assertIn(
            "audit-event-release-sha256sums-provenance.json",
            "\n".join(report["violations"]),
        )

    def test_ghcr_publish_contract_guard_passes_with_matching_digests(self) -> None:
        policy = self.tmp / "ghcr-tag-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.ghcr-tag-policy.v1",
                    "release_tag_regex": "^v[0-9]+\\.[0-9]+\\.[0-9]+$",
                    "sha_tag_prefix": "sha-",
                    "sha_tag_length": 12,
                    "latest_tag": "latest",
                    "require_latest_on_release": True,
                    "immutable_tag_classes": ["release", "sha"],
                    "rollback_priority": ["sha", "release"],
                    "contract_artifact_retention_days": 21,
                    "scan_artifact_retention_days": 14,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        snapshot = self.tmp / "ghcr-snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "tags": {
                        "v1.2.3": {"status_code": 200, "digest": "sha256:abc123"},
                        "sha-abcdef123456": {"status_code": 200, "digest": "sha256:abc123"},
                        "latest": {"status_code": 200, "digest": "sha256:abc123"},
                    }
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "ghcr-publish-contract.json"
        out_md = self.tmp / "ghcr-publish-contract.md"
        proc = run_cmd(
            [
                "python3",
                self._script("ghcr_publish_contract_guard.py"),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--release-tag",
                "v1.2.3",
                "--sha",
                "abcdef1234567890abcdef1234567890abcdef12",
                "--policy-file",
                str(policy),
                "--manifest-snapshot-file",
                str(snapshot),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertTrue(report["ready"])
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["rollback_candidates"], ["sha-abcdef123456", "v1.2.3"])

    def test_ghcr_publish_contract_guard_detects_digest_parity_violation(self) -> None:
        policy = self.tmp / "ghcr-tag-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.ghcr-tag-policy.v1",
                    "release_tag_regex": "^v[0-9]+\\.[0-9]+\\.[0-9]+$",
                    "sha_tag_prefix": "sha-",
                    "sha_tag_length": 12,
                    "latest_tag": "latest",
                    "require_latest_on_release": True,
                    "immutable_tag_classes": ["release", "sha"],
                    "rollback_priority": ["sha", "release"],
                    "contract_artifact_retention_days": 21,
                    "scan_artifact_retention_days": 14,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        snapshot = self.tmp / "ghcr-snapshot.mismatch.json"
        snapshot.write_text(
            json.dumps(
                {
                    "tags": {
                        "v1.2.3": {"status_code": 200, "digest": "sha256:111"},
                        "sha-abcdef123456": {"status_code": 200, "digest": "sha256:222"},
                        "latest": {"status_code": 200, "digest": "sha256:333"},
                    }
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "ghcr-publish-contract.mismatch.json"
        out_md = self.tmp / "ghcr-publish-contract.mismatch.md"
        proc = run_cmd(
            [
                "python3",
                self._script("ghcr_publish_contract_guard.py"),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--release-tag",
                "v1.2.3",
                "--sha",
                "abcdef1234567890abcdef1234567890abcdef12",
                "--policy-file",
                str(policy),
                "--manifest-snapshot-file",
                str(snapshot),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertFalse(report["ready"])
        violations = "\n".join(report["violations"])
        self.assertIn("release tag digest does not match immutable sha tag digest", violations)
        self.assertIn("latest tag digest does not match release tag digest", violations)

    def test_ghcr_vulnerability_gate_passes_when_blocking_findings_are_zero(self) -> None:
        policy = self.tmp / "ghcr-vulnerability-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.ghcr-vulnerability-policy.v1",
                    "required_tag_classes": ["release", "sha", "latest"],
                    "blocking_severities": ["HIGH", "CRITICAL"],
                    "max_blocking_findings_per_tag": 0,
                    "require_blocking_count_parity": True,
                    "require_artifact_id_parity": True,
                    "scan_artifact_retention_days": 14,
                    "audit_artifact_retention_days": 21,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        release_report = self.tmp / "trivy-v1.2.3.json"
        sha_report = self.tmp / "trivy-sha-abcdef123456.json"
        latest_report = self.tmp / "trivy-latest.json"
        shared_report = {
            "ArtifactID": "sha256:deadbeef",
            "Results": [
                {
                    "Target": "alpine:3.20",
                    "Type": "os",
                    "Vulnerabilities": [
                        {
                            "VulnerabilityID": "CVE-2026-0001",
                            "Severity": "MEDIUM",
                        }
                    ],
                }
            ],
        }
        release_report.write_text(json.dumps(shared_report, indent=2) + "\n", encoding="utf-8")
        sha_report.write_text(json.dumps(shared_report, indent=2) + "\n", encoding="utf-8")
        latest_report.write_text(json.dumps(shared_report, indent=2) + "\n", encoding="utf-8")

        out_json = self.tmp / "ghcr-vulnerability-gate.json"
        out_md = self.tmp / "ghcr-vulnerability-gate.md"
        proc = run_cmd(
            [
                "python3",
                self._script("ghcr_vulnerability_gate.py"),
                "--release-tag",
                "v1.2.3",
                "--sha-tag",
                "sha-abcdef123456",
                "--latest-tag",
                "latest",
                "--release-report-json",
                str(release_report),
                "--sha-report-json",
                str(sha_report),
                "--latest-report-json",
                str(latest_report),
                "--policy-file",
                str(policy),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertTrue(report["ready"])
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["reports"]["release"]["blocking_vulnerabilities"], 0)
        self.assertEqual(report["reports"]["sha"]["blocking_vulnerabilities"], 0)
        self.assertEqual(report["reports"]["latest"]["blocking_vulnerabilities"], 0)

    def test_ghcr_vulnerability_gate_fails_on_blocking_and_parity_violations(self) -> None:
        policy = self.tmp / "ghcr-vulnerability-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.ghcr-vulnerability-policy.v1",
                    "required_tag_classes": ["release", "sha", "latest"],
                    "blocking_severities": ["HIGH", "CRITICAL"],
                    "max_blocking_findings_per_tag": 0,
                    "require_blocking_count_parity": True,
                    "require_artifact_id_parity": True,
                    "scan_artifact_retention_days": 14,
                    "audit_artifact_retention_days": 21,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        release_report = self.tmp / "trivy-v1.2.3.json"
        sha_report = self.tmp / "trivy-sha-abcdef123456.json"
        latest_report = self.tmp / "trivy-latest.json"
        release_report.write_text(
            json.dumps(
                {
                    "ArtifactID": "sha256:image-a",
                    "Results": [
                        {
                            "Target": "alpine:3.20",
                            "Type": "os",
                            "Vulnerabilities": [
                                {
                                    "VulnerabilityID": "CVE-2026-9999",
                                    "Severity": "CRITICAL",
                                }
                            ],
                        }
                    ],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        sha_report.write_text(
            json.dumps(
                {
                    "ArtifactID": "sha256:image-b",
                    "Results": [{"Target": "alpine:3.20", "Type": "os", "Vulnerabilities": []}],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        latest_report.write_text(
            json.dumps(
                {
                    "ArtifactID": "sha256:image-a",
                    "Results": [{"Target": "alpine:3.20", "Type": "os", "Vulnerabilities": []}],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "ghcr-vulnerability-gate.fail.json"
        out_md = self.tmp / "ghcr-vulnerability-gate.fail.md"
        proc = run_cmd(
            [
                "python3",
                self._script("ghcr_vulnerability_gate.py"),
                "--release-tag",
                "v1.2.3",
                "--sha-tag",
                "sha-abcdef123456",
                "--latest-tag",
                "latest",
                "--release-report-json",
                str(release_report),
                "--sha-report-json",
                str(sha_report),
                "--latest-report-json",
                str(latest_report),
                "--policy-file",
                str(policy),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertFalse(report["ready"])
        violations = "\n".join(report["violations"])
        self.assertIn("Blocking vulnerabilities for `release`", violations)
        self.assertIn("Blocking vulnerability count parity violation across tags", violations)
        self.assertIn("Artifact ID parity violation across tags", violations)

    def test_docs_deploy_guard_allows_manual_production_rollback_with_preview_evidence(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)
        run_cmd(["git", "branch", "-m", "main"], cwd=repo)

        notes = repo / "docs.md"
        notes.write_text("base\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        rollback_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        notes.write_text("head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        policy = self.tmp / "docs-deploy-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.docs-deploy-policy.v1",
                    "production_branch": "main",
                    "allow_manual_production_dispatch": True,
                    "require_preview_evidence_on_manual_production": True,
                    "allow_manual_rollback_dispatch": True,
                    "rollback_ref_must_be_ancestor_of_production_branch": True,
                    "docs_preview_retention_days": 14,
                    "docs_guard_artifact_retention_days": 21,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "docs-deploy-guard.json"
        out_md = self.tmp / "docs-deploy-guard.md"
        proc = run_cmd(
            [
                "python3",
                self._script("docs_deploy_guard.py"),
                "--repo-root",
                str(repo),
                "--event-name",
                "workflow_dispatch",
                "--git-ref",
                "refs/heads/main",
                "--git-sha",
                head_sha,
                "--input-deploy-target",
                "production",
                "--input-preview-evidence-run-url",
                "https://github.com/zeroclaw-labs/zeroclaw/actions/runs/123",
                "--input-rollback-ref",
                rollback_sha,
                "--policy-file",
                str(policy),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertTrue(report["ready"])
        self.assertEqual(report["deploy_target"], "production")
        self.assertEqual(report["deploy_mode"], "rollback")
        self.assertEqual(report["source_ref"], rollback_sha)
        self.assertEqual(report["violations"], [])

    def test_docs_deploy_guard_requires_preview_evidence_for_manual_production(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)
        run_cmd(["git", "branch", "-m", "main"], cwd=repo)

        notes = repo / "docs.md"
        notes.write_text("head\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        policy = self.tmp / "docs-deploy-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.docs-deploy-policy.v1",
                    "production_branch": "main",
                    "allow_manual_production_dispatch": True,
                    "require_preview_evidence_on_manual_production": True,
                    "allow_manual_rollback_dispatch": True,
                    "rollback_ref_must_be_ancestor_of_production_branch": True,
                    "docs_preview_retention_days": 14,
                    "docs_guard_artifact_retention_days": 21,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "docs-deploy-guard.missing-preview.json"
        out_md = self.tmp / "docs-deploy-guard.missing-preview.md"
        proc = run_cmd(
            [
                "python3",
                self._script("docs_deploy_guard.py"),
                "--repo-root",
                str(repo),
                "--event-name",
                "workflow_dispatch",
                "--git-ref",
                "refs/heads/main",
                "--git-sha",
                head_sha,
                "--input-deploy-target",
                "production",
                "--policy-file",
                str(policy),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertFalse(report["ready"])
        self.assertIn("requires `preview_evidence_run_url`", "\n".join(report["violations"]))

    def test_docs_deploy_guard_rejects_non_ancestor_rollback_ref(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)
        run_cmd(["git", "branch", "-m", "main"], cwd=repo)

        notes = repo / "docs.md"
        notes.write_text("base\n", encoding="utf-8")
        run_cmd(["git", "add", "."], cwd=repo)
        run_cmd(["git", "commit", "-m", "base"], cwd=repo)
        base_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        notes.write_text("main-head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "main-head"], cwd=repo)
        head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()

        run_cmd(["git", "checkout", "-b", "side", base_sha], cwd=repo)
        notes.write_text("side-head\n", encoding="utf-8")
        run_cmd(["git", "commit", "-am", "side-head"], cwd=repo)
        side_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()
        run_cmd(["git", "checkout", "main"], cwd=repo)

        policy = self.tmp / "docs-deploy-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.docs-deploy-policy.v1",
                    "production_branch": "main",
                    "allow_manual_production_dispatch": True,
                    "require_preview_evidence_on_manual_production": True,
                    "allow_manual_rollback_dispatch": True,
                    "rollback_ref_must_be_ancestor_of_production_branch": True,
                    "docs_preview_retention_days": 14,
                    "docs_guard_artifact_retention_days": 21,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "docs-deploy-guard.non-ancestor.json"
        out_md = self.tmp / "docs-deploy-guard.non-ancestor.md"
        proc = run_cmd(
            [
                "python3",
                self._script("docs_deploy_guard.py"),
                "--repo-root",
                str(repo),
                "--event-name",
                "workflow_dispatch",
                "--git-ref",
                "refs/heads/main",
                "--git-sha",
                head_sha,
                "--input-deploy-target",
                "production",
                "--input-preview-evidence-run-url",
                "https://github.com/zeroclaw-labs/zeroclaw/actions/runs/123",
                "--input-rollback-ref",
                side_sha,
                "--policy-file",
                str(policy),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertFalse(report["ready"])
        self.assertIn("is not an ancestor", "\n".join(report["violations"]))

    def test_release_artifact_guard_detects_missing_archives_in_verify_stage(self) -> None:
        artifacts = self.tmp / "artifacts"
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "zeroclaw-x86_64-unknown-linux-gnu.tar.gz").write_bytes(b"linux-gnu")

        contract = self.tmp / "artifact-contract.json"
        contract.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.release-artifact-contract.v1",
                    "release_archive_patterns": [
                        "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
                        "zeroclaw-x86_64-unknown-linux-musl.tar.gz",
                    ],
                    "required_manifest_files": [
                        "release-manifest.json",
                        "release-manifest.md",
                        "SHA256SUMS",
                    ],
                    "required_sbom_files": ["zeroclaw.cdx.json", "zeroclaw.spdx.json"],
                    "required_notice_files": ["LICENSE-APACHE", "LICENSE-MIT", "NOTICE"],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "release-artifact-guard.verify.json"
        out_md = self.tmp / "release-artifact-guard.verify.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_artifact_guard.py"),
                "--artifacts-dir",
                str(artifacts),
                "--contract-file",
                str(contract),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--allow-extra-archives",
                "--skip-manifest-files",
                "--skip-sbom-files",
                "--skip-notice-files",
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("Missing release archives", joined)
        self.assertTrue(report["categories"]["manifest_files"]["skipped"])
        self.assertTrue(report["categories"]["sbom_files"]["skipped"])
        self.assertTrue(report["categories"]["notice_files"]["skipped"])

    def test_release_artifact_guard_passes_for_full_publish_contract(self) -> None:
        artifacts = self.tmp / "artifacts"
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "zeroclaw-x86_64-unknown-linux-gnu.tar.gz").write_bytes(b"linux-gnu")
        (artifacts / "zeroclaw-x86_64-unknown-linux-musl.tar.gz").write_bytes(b"linux-musl")
        (artifacts / "release-manifest.json").write_text('{"ok":true}\n', encoding="utf-8")
        (artifacts / "release-manifest.md").write_text("# ok\n", encoding="utf-8")
        (artifacts / "SHA256SUMS").write_text("abc  file\n", encoding="utf-8")
        (artifacts / "zeroclaw.cdx.json").write_text('{"sbom":"cdx"}\n', encoding="utf-8")
        (artifacts / "zeroclaw.spdx.json").write_text('{"sbom":"spdx"}\n', encoding="utf-8")
        (artifacts / "LICENSE-APACHE").write_text("license\n", encoding="utf-8")
        (artifacts / "LICENSE-MIT").write_text("license\n", encoding="utf-8")
        (artifacts / "NOTICE").write_text("notice\n", encoding="utf-8")
        (artifacts / "zeroclaw-x86_64-unknown-linux-gnu.tar.gz.sig").write_text("sig\n", encoding="utf-8")

        contract = self.tmp / "artifact-contract.json"
        contract.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.release-artifact-contract.v1",
                    "release_archive_patterns": [
                        "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
                        "zeroclaw-x86_64-unknown-linux-musl.tar.gz",
                    ],
                    "required_manifest_files": [
                        "release-manifest.json",
                        "release-manifest.md",
                        "SHA256SUMS",
                    ],
                    "required_sbom_files": ["zeroclaw.cdx.json", "zeroclaw.spdx.json"],
                    "required_notice_files": ["LICENSE-APACHE", "LICENSE-MIT", "NOTICE"],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "release-artifact-guard.publish.json"
        out_md = self.tmp / "release-artifact-guard.publish.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_artifact_guard.py"),
                "--artifacts-dir",
                str(artifacts),
                "--contract-file",
                str(contract),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--allow-extra-archives",
                "--allow-extra-manifest-files",
                "--allow-extra-sbom-files",
                "--allow-extra-notice-files",
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertTrue(report["ready"])
        self.assertEqual(report["violations"], [])

    def test_release_artifact_guard_rejects_invalid_contract_schema(self) -> None:
        artifacts = self.tmp / "artifacts"
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "zeroclaw-x86_64-unknown-linux-gnu.tar.gz").write_bytes(b"linux-gnu")

        contract = self.tmp / "artifact-contract.json"
        contract.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.release-artifact-contract.v0",
                    "release_archive_patterns": ["zeroclaw-x86_64-unknown-linux-gnu.tar.gz"],
                    "required_manifest_files": ["release-manifest.json"],
                    "required_sbom_files": ["zeroclaw.cdx.json"],
                    "required_notice_files": ["NOTICE"],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "release-artifact-guard.invalid-schema.json"
        out_md = self.tmp / "release-artifact-guard.invalid-schema.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_artifact_guard.py"),
                "--artifacts-dir",
                str(artifacts),
                "--contract-file",
                str(contract),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--allow-extra-archives",
                "--skip-manifest-files",
                "--skip-sbom-files",
                "--skip-notice-files",
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertIn("schema_version", "\n".join(report["violations"]))

    def test_release_trigger_guard_allows_authorized_actor_and_tagger(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0", "-m", "v0.2.0"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)

        out_json = self.tmp / "release-trigger-guard.json"
        out_md = self.tmp / "release-trigger-guard.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_trigger_guard.py"),
                "--repo-root",
                str(repo),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--origin-url",
                str(repo),
                "--event-name",
                "push",
                "--actor",
                "chumyin",
                "--release-ref",
                "v0.2.0",
                "--release-tag",
                "v0.2.0",
                "--publish-release",
                "true",
                "--authorized-actors",
                "willsarg,theonlyhennygod,chumyin",
                "--authorized-tagger-emails",
                "test@example.com",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertTrue(report["ready_to_publish"])
        self.assertTrue(report["authorization"]["actor_authorized"])
        self.assertTrue(report["authorization"]["tagger_authorized"])
        self.assertTrue(report["tag_metadata"]["annotated_tag"])
        self.assertEqual(report["tag_metadata"]["cargo_version"], "0.2.0")

    def test_release_trigger_guard_blocks_unauthorized_actor(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0", "-m", "v0.2.0"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)

        out_json = self.tmp / "release-trigger-guard-unauthorized.json"
        out_md = self.tmp / "release-trigger-guard-unauthorized.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_trigger_guard.py"),
                "--repo-root",
                str(repo),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--origin-url",
                str(repo),
                "--event-name",
                "workflow_dispatch",
                "--actor",
                "intruder",
                "--release-ref",
                "v0.2.0",
                "--release-tag",
                "v0.2.0",
                "--publish-release",
                "true",
                "--authorized-actors",
                "willsarg,theonlyhennygod,chumyin",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertFalse(report["ready_to_publish"])
        self.assertFalse(report["authorization"]["actor_authorized"])
        joined = "\n".join(report["violations"])
        self.assertIn("not authorized", joined)

    def test_release_trigger_guard_rejects_lightweight_tag(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        tag_proc = run_cmd(["git", "update-ref", "refs/tags/v0.2.0", "HEAD"], cwd=repo)
        self.assertEqual(tag_proc.returncode, 0, msg=tag_proc.stderr)
        tag_list = run_cmd(["git", "tag", "--list", "v0.2.0"], cwd=repo)
        self.assertEqual(tag_list.stdout.strip(), "v0.2.0")
        remote_proc = run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)
        self.assertEqual(remote_proc.returncode, 0, msg=remote_proc.stderr)

        out_json = self.tmp / "release-trigger-guard-lightweight.json"
        out_md = self.tmp / "release-trigger-guard-lightweight.md"
        proc = run_cmd(
            [
                "python3",
                self._script("release_trigger_guard.py"),
                "--repo-root",
                str(repo),
                "--repository",
                "zeroclaw-labs/zeroclaw",
                "--origin-url",
                str(repo),
                "--event-name",
                "push",
                "--actor",
                "chumyin",
                "--release-ref",
                "v0.2.0",
                "--release-tag",
                "v0.2.0",
                "--publish-release",
                "true",
                "--authorized-actors",
                "willsarg,theonlyhennygod,chumyin",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("annotated tag", joined)

    def test_nightly_matrix_report_fails_on_failed_lane(self) -> None:
        lane_root = self.tmp / "lane-artifacts"
        lane_root.mkdir(parents=True, exist_ok=True)
        (lane_root / "nightly-result-default.json").write_text(
            json.dumps(
                {
                    "lane": "default",
                    "status": "success",
                    "exit_code": 0,
                    "duration_seconds": 12,
                    "command": "cargo check --locked",
                }
            )
            + "\n",
            encoding="utf-8",
        )
        (lane_root / "nightly-result-nightly-all-features.json").write_text(
            json.dumps(
                {
                    "lane": "nightly-all-features",
                    "status": "failure",
                    "exit_code": 101,
                    "duration_seconds": 47,
                    "command": "cargo test --all-features",
                }
            )
            + "\n",
            encoding="utf-8",
        )
        owners = self.tmp / "owners.json"
        owners.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.nightly-owner-routing.v1",
                    "owners": {
                        "default": "@ops",
                        "nightly-all-features": "@release",
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "nightly-summary.json"
        out_md = self.tmp / "nightly-summary.md"
        proc = run_cmd(
            [
                "python3",
                self._script("nightly_matrix_report.py"),
                "--input-dir",
                str(lane_root),
                "--owners-file",
                str(owners),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-failure",
            ]
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["failed"], 1)
        self.assertEqual(report["passed"], 1)

    def test_nightly_matrix_report_includes_history_trend_snapshot(self) -> None:
        lane_root = self.tmp / "lane-artifacts-trend"
        lane_root.mkdir(parents=True, exist_ok=True)
        (lane_root / "nightly-result-default.json").write_text(
            json.dumps(
                {
                    "lane": "default",
                    "status": "success",
                    "exit_code": 0,
                    "duration_seconds": 11,
                    "command": "cargo test --locked --test agent_e2e --verbose",
                }
            )
            + "\n",
            encoding="utf-8",
        )

        owners = self.tmp / "owners-trend.json"
        owners.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.nightly-owner-routing.v1",
                    "owners": {"default": "@ops"},
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        history = self.tmp / "nightly-history.json"
        history.write_text(
            json.dumps(
                [
                    {
                        "run_id": 101,
                        "url": "https://example.test/runs/101",
                        "event": "workflow_dispatch",
                        "conclusion": "success",
                        "created_at": "2026-02-25T01:00:00Z",
                    },
                    {
                        "run_id": 100,
                        "url": "https://example.test/runs/100",
                        "event": "schedule",
                        "conclusion": "failure",
                        "created_at": "2026-02-24T01:00:00Z",
                    },
                    {
                        "run_id": 99,
                        "url": "https://example.test/runs/99",
                        "event": "schedule",
                        "conclusion": "success",
                        "created_at": "2026-02-23T01:00:00Z",
                    },
                ],
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "nightly-summary-trend.json"
        out_md = self.tmp / "nightly-summary-trend.md"
        proc = run_cmd(
            [
                "python3",
                self._script("nightly_matrix_report.py"),
                "--input-dir",
                str(lane_root),
                "--owners-file",
                str(owners),
                "--history-file",
                str(history),
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-failure",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(out_json.read_text(encoding="utf-8"))
        trend = report["trend_snapshot"]
        self.assertEqual(trend["history_total"], 3)
        self.assertEqual(trend["history_passed"], 2)
        self.assertEqual(trend["history_failed"], 1)
        self.assertEqual(trend["history_pass_rate"], 0.6667)
        self.assertEqual(trend["history_runs"][0]["run_id"], 101)

        markdown = out_md.read_text(encoding="utf-8")
        self.assertIn("## Recent Nightly Runs", markdown)
        self.assertIn("example.test/runs/101", markdown)

    def test_canary_guard_promote_when_metrics_within_threshold(self) -> None:
        policy = self.tmp / "canary-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.canary-policy.v1",
                    "minimum_sample_size": 300,
                    "observation_window_minutes": 60,
                    "cohorts": [
                        {"name": "canary-5pct", "traffic_percent": 5, "duration_minutes": 20},
                        {"name": "canary-20pct", "traffic_percent": 20, "duration_minutes": 20},
                    ],
                    "observability_signals": [
                        "error_rate",
                        "crash_rate",
                        "p95_latency_ms",
                        "sample_size",
                    ],
                    "thresholds": {
                        "max_error_rate": 0.02,
                        "max_crash_rate": 0.01,
                        "max_p95_latency_ms": 1200,
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        out_json = self.tmp / "canary.json"
        out_md = self.tmp / "canary.md"
        proc = run_cmd(
            [
                "python3",
                self._script("canary_guard.py"),
                "--policy-file",
                str(policy),
                "--candidate-tag",
                "v0.2.0-rc.1",
                "--mode",
                "execute",
                "--error-rate",
                "0.01",
                "--crash-rate",
                "0.005",
                "--p95-latency-ms",
                "900",
                "--sample-size",
                "500",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["decision"], "promote")
        self.assertTrue(report["ready_to_execute"])
        self.assertEqual(report["cohorts"][0]["name"], "canary-5pct")
        self.assertEqual(report["cohorts"][1]["traffic_percent"], 20)
        self.assertEqual(
            report["observability_signals"],
            ["error_rate", "crash_rate", "p95_latency_ms", "sample_size"],
        )

    def test_prerelease_guard_requires_previous_stage(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-rc.1", "-m", "v0.2.0-rc.1"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)
        run_cmd(["git", "fetch", "origin", "main:refs/remotes/origin/main"], cwd=repo)

        stage_cfg = self.tmp / "stage-gates.json"
        stage_cfg.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.prerelease-stage-gates.v1",
                    "required_previous_stage": {
                        "beta": "alpha",
                        "rc": "beta",
                        "stable": "rc",
                    },
                    "required_checks": {
                        "rc": ["CI Required Gate", "Nightly All-Features"],
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "prerelease-guard.json"
        out_md = self.tmp / "prerelease-guard.md"
        proc = run_cmd(
            [
                "python3",
                self._script("prerelease_guard.py"),
                "--repo-root",
                str(repo),
                "--tag",
                "v0.2.0-rc.1",
                "--stage-config-file",
                str(stage_cfg),
                "--mode",
                "publish",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("requires at least one `beta` tag", joined)

    def test_prerelease_guard_reports_promotion_transition_and_stage_history(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-alpha.1", "-m", "v0.2.0-alpha.1"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-beta.1", "-m", "v0.2.0-beta.1"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)
        run_cmd(["git", "fetch", "origin", "main:refs/remotes/origin/main"], cwd=repo)

        stage_cfg = self.tmp / "stage-gates.json"
        stage_cfg.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.prerelease-stage-gates.v1",
                    "stage_order": ["alpha", "beta", "rc", "stable"],
                    "required_previous_stage": {
                        "beta": "alpha",
                        "rc": "beta",
                        "stable": "rc",
                    },
                    "required_checks": {
                        "alpha": ["CI Required Gate", "Security Audit"],
                        "beta": ["CI Required Gate", "Security Audit", "Feature Matrix Summary"],
                        "rc": [
                            "CI Required Gate",
                            "Security Audit",
                            "Feature Matrix Summary",
                            "Nightly Summary & Routing",
                        ],
                        "stable": [
                            "CI Required Gate",
                            "Security Audit",
                            "Feature Matrix Summary",
                            "Verify Artifact Set",
                            "Nightly Summary & Routing",
                        ],
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "prerelease-guard-promotion.json"
        out_md = self.tmp / "prerelease-guard-promotion.md"
        proc = run_cmd(
            [
                "python3",
                self._script("prerelease_guard.py"),
                "--repo-root",
                str(repo),
                "--tag",
                "v0.2.0-beta.1",
                "--stage-config-file",
                str(stage_cfg),
                "--mode",
                "publish",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["schema_version"], "zeroclaw.prerelease-guard.v2")
        self.assertEqual(report["transition"]["type"], "promotion")
        self.assertEqual(report["transition"]["outcome"], "promotion")
        self.assertEqual(report["transition"]["required_previous_tag"], "v0.2.0-alpha.1")
        self.assertEqual(report["stage_history"]["latest_stage"], "beta")
        self.assertEqual(report["stage_history"]["latest_tag"], "v0.2.0-beta.1")
        self.assertIn("v0.2.0-alpha.1", report["stage_history"]["per_stage"]["alpha"])
        self.assertIn("v0.2.0-beta.1", report["stage_history"]["per_stage"]["beta"])

    def test_prerelease_guard_blocks_demotion_and_records_transition(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-alpha.1", "-m", "v0.2.0-alpha.1"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-beta.1", "-m", "v0.2.0-beta.1"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-alpha.2", "-m", "v0.2.0-alpha.2"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)
        run_cmd(["git", "fetch", "origin", "main:refs/remotes/origin/main"], cwd=repo)

        stage_cfg = self.tmp / "stage-gates.json"
        stage_cfg.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.prerelease-stage-gates.v1",
                    "stage_order": ["alpha", "beta", "rc", "stable"],
                    "required_previous_stage": {
                        "beta": "alpha",
                        "rc": "beta",
                        "stable": "rc",
                    },
                    "required_checks": {
                        "alpha": ["CI Required Gate", "Security Audit"],
                        "beta": ["CI Required Gate", "Security Audit", "Feature Matrix Summary"],
                        "rc": [
                            "CI Required Gate",
                            "Security Audit",
                            "Feature Matrix Summary",
                            "Nightly Summary & Routing",
                        ],
                        "stable": [
                            "CI Required Gate",
                            "Security Audit",
                            "Feature Matrix Summary",
                            "Verify Artifact Set",
                            "Nightly Summary & Routing",
                        ],
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "prerelease-guard-demotion.json"
        out_md = self.tmp / "prerelease-guard-demotion.md"
        proc = run_cmd(
            [
                "python3",
                self._script("prerelease_guard.py"),
                "--repo-root",
                str(repo),
                "--tag",
                "v0.2.0-alpha.2",
                "--stage-config-file",
                str(stage_cfg),
                "--mode",
                "publish",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        self.assertEqual(report["transition"]["type"], "demotion_blocked")
        self.assertEqual(report["transition"]["outcome"], "demotion_blocked")
        self.assertEqual(report["transition"]["previous_highest_stage"], "beta")
        self.assertEqual(report["transition"]["previous_highest_tag"], "v0.2.0-beta.1")
        joined = "\n".join(report["violations"])
        self.assertIn("Refusing stage regression", joined)

    def test_prerelease_guard_requires_monotonic_same_stage_number(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-alpha.2", "-m", "v0.2.0-alpha.2"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-alpha.1", "-m", "v0.2.0-alpha.1"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)
        run_cmd(["git", "fetch", "origin", "main:refs/remotes/origin/main"], cwd=repo)

        stage_cfg = self.tmp / "stage-gates.json"
        stage_cfg.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.prerelease-stage-gates.v1",
                    "stage_order": ["alpha", "beta", "rc", "stable"],
                    "required_previous_stage": {
                        "beta": "alpha",
                        "rc": "beta",
                        "stable": "rc",
                    },
                    "required_checks": {
                        "alpha": ["CI Required Gate", "Security Audit"],
                        "beta": ["CI Required Gate", "Security Audit", "Feature Matrix Summary"],
                        "rc": [
                            "CI Required Gate",
                            "Security Audit",
                            "Feature Matrix Summary",
                            "Nightly Summary & Routing",
                        ],
                        "stable": [
                            "CI Required Gate",
                            "Security Audit",
                            "Feature Matrix Summary",
                            "Verify Artifact Set",
                            "Nightly Summary & Routing",
                        ],
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "prerelease-guard-monotonic.json"
        out_md = self.tmp / "prerelease-guard-monotonic.md"
        proc = run_cmd(
            [
                "python3",
                self._script("prerelease_guard.py"),
                "--repo-root",
                str(repo),
                "--tag",
                "v0.2.0-alpha.1",
                "--stage-config-file",
                str(stage_cfg),
                "--mode",
                "publish",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("must increase monotonically", joined)

    def test_prerelease_guard_detects_incomplete_stage_matrix_config(self) -> None:
        repo = self.tmp / "repo"
        repo.mkdir(parents=True, exist_ok=True)
        run_cmd(["git", "init"], cwd=repo)
        run_cmd(["git", "config", "user.name", "Test User"], cwd=repo)
        run_cmd(["git", "config", "user.email", "test@example.com"], cwd=repo)

        cargo = repo / "Cargo.toml"
        cargo.write_text(
            textwrap.dedent(
                """
                [package]
                name = "sample"
                version = "0.2.0"
                edition = "2021"
                """
            ).strip()
            + "\n",
            encoding="utf-8",
        )
        run_cmd(["git", "add", "Cargo.toml"], cwd=repo)
        run_cmd(["git", "commit", "-m", "init"], cwd=repo)
        run_cmd(["git", "branch", "-M", "main"], cwd=repo)
        run_cmd(["git", "tag", "-a", "v0.2.0-alpha.1", "-m", "v0.2.0-alpha.1"], cwd=repo)
        run_cmd(["git", "remote", "add", "origin", str(repo)], cwd=repo)
        run_cmd(["git", "fetch", "origin", "main:refs/remotes/origin/main"], cwd=repo)

        stage_cfg = self.tmp / "invalid-stage-gates.json"
        stage_cfg.write_text(
            json.dumps(
                {
                    "schema_version": "zeroclaw.prerelease-stage-gates.v1",
                    "stage_order": ["alpha", "beta", "stable"],
                    "required_previous_stage": {
                        "beta": "alpha",
                        "stable": "rc",
                    },
                    "required_checks": {
                        "alpha": ["CI Required Gate", "Security Audit"],
                        "beta": ["CI Required Gate", "Security Audit", "Feature Matrix Summary"],
                    },
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )

        out_json = self.tmp / "prerelease-guard-policy.json"
        out_md = self.tmp / "prerelease-guard-policy.md"
        proc = run_cmd(
            [
                "python3",
                self._script("prerelease_guard.py"),
                "--repo-root",
                str(repo),
                "--tag",
                "v0.2.0-alpha.1",
                "--stage-config-file",
                str(stage_cfg),
                "--mode",
                "dry-run",
                "--output-json",
                str(out_json),
                "--output-md",
                str(out_md),
                "--fail-on-violation",
            ],
            cwd=repo,
        )
        self.assertEqual(proc.returncode, 3)
        report = json.loads(out_json.read_text(encoding="utf-8"))
        joined = "\n".join(report["violations"])
        self.assertIn("stage_order", joined)
        self.assertIn("required_checks.rc", joined)
        self.assertIn("required_checks.stable", joined)

    def test_queue_hygiene_dry_run_selects_obsolete_and_superseded_runs(self) -> None:
        runs_json = self.tmp / "runs.json"
        output_json = self.tmp / "queue-hygiene.json"
        runs_json.write_text(
            json.dumps(
                {
                    "workflow_runs": [
                        {
                            "id": 11,
                            "name": "CI Build (Fast)",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-11",
                            "created_at": "2026-02-27T20:00:00Z",
                        },
                        {
                            "id": 12,
                            "name": "CI Build (Fast)",
                            "event": "pull_request",
                            "head_branch": "feature-a",
                            "head_sha": "sha-12",
                            "created_at": "2026-02-27T20:01:00Z",
                            "pull_requests": [{"number": 1001}],
                        },
                        {
                            "id": 21,
                            "name": "CI Run",
                            "event": "pull_request",
                            "head_branch": "feature-a",
                            "head_sha": "sha-21",
                            "created_at": "2026-02-27T20:02:00Z",
                            "pull_requests": [{"number": 1001}],
                        },
                        {
                            "id": 22,
                            "name": "CI Run",
                            "event": "pull_request",
                            "head_branch": "feature-a",
                            "head_sha": "sha-22",
                            "created_at": "2026-02-27T20:03:00Z",
                            "pull_requests": [{"number": 1001}],
                        },
                        {
                            "id": 23,
                            "name": "CI Run",
                            "event": "pull_request",
                            "head_branch": "feature-a",
                            "head_sha": "sha-23",
                            "created_at": "2026-02-27T20:04:00Z",
                            "pull_requests": [{"number": 1002}],
                        },
                        {
                            "id": 24,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-24",
                            "created_at": "2026-02-27T20:05:00Z",
                        },
                        {
                            "id": 25,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-25",
                            "created_at": "2026-02-27T20:06:00Z",
                        },
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

        proc = run_cmd(
            [
                "python3",
                self._script("queue_hygiene.py"),
                "--runs-json",
                str(runs_json),
                "--obsolete-workflow",
                "CI Build (Fast)",
                "--dedupe-workflow",
                "CI Run",
                "--output-json",
                str(output_json),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(output_json.read_text(encoding="utf-8"))
        self.assertEqual(report["counts"]["runs_in_scope"], 7)
        self.assertEqual(report["counts"]["candidate_runs_before_cap"], 3)
        planned_ids = [item["id"] for item in report["planned_actions"]]
        self.assertEqual(planned_ids, [11, 12, 21])
        reasons_by_id = {item["id"]: item["reasons"] for item in report["planned_actions"]}
        self.assertIn("obsolete-workflow", reasons_by_id[11])
        self.assertIn("obsolete-workflow", reasons_by_id[12])
        self.assertTrue(any(reason.startswith("dedupe-superseded-by:22") for reason in reasons_by_id[21]))

    def test_queue_hygiene_respects_max_cancel_cap(self) -> None:
        runs_json = self.tmp / "runs-cap.json"
        output_json = self.tmp / "queue-hygiene-cap.json"
        runs_json.write_text(
            json.dumps(
                {
                    "workflow_runs": [
                        {
                            "id": 101,
                            "name": "CI Build (Fast)",
                            "event": "push",
                            "head_branch": "main",
                            "created_at": "2026-02-27T20:00:00Z",
                        },
                        {
                            "id": 102,
                            "name": "CI Build (Fast)",
                            "event": "push",
                            "head_branch": "main",
                            "created_at": "2026-02-27T20:01:00Z",
                        },
                        {
                            "id": 103,
                            "name": "CI Build (Fast)",
                            "event": "push",
                            "head_branch": "main",
                            "created_at": "2026-02-27T20:02:00Z",
                        },
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

        proc = run_cmd(
            [
                "python3",
                self._script("queue_hygiene.py"),
                "--runs-json",
                str(runs_json),
                "--obsolete-workflow",
                "CI Build (Fast)",
                "--max-cancel",
                "2",
                "--output-json",
                str(output_json),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(output_json.read_text(encoding="utf-8"))
        self.assertEqual(report["counts"]["candidate_runs_before_cap"], 3)
        self.assertEqual(report["counts"]["candidate_runs_after_cap"], 2)
        self.assertEqual(report["counts"]["skipped_by_cap"], 1)
        planned_ids = [item["id"] for item in report["planned_actions"]]
        self.assertEqual(planned_ids, [101, 102])

    def test_queue_hygiene_non_pr_branch_mode_dedupes_push_runs(self) -> None:
        runs_json = self.tmp / "runs-non-pr-branch.json"
        output_json = self.tmp / "queue-hygiene-non-pr-branch.json"
        runs_json.write_text(
            json.dumps(
                {
                    "workflow_runs": [
                        {
                            "id": 201,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-201",
                            "created_at": "2026-02-27T20:00:00Z",
                        },
                        {
                            "id": 202,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-202",
                            "created_at": "2026-02-27T20:01:00Z",
                        },
                        {
                            "id": 203,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "dev",
                            "head_sha": "sha-203",
                            "created_at": "2026-02-27T20:02:00Z",
                        },
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

        proc = run_cmd(
            [
                "python3",
                self._script("queue_hygiene.py"),
                "--runs-json",
                str(runs_json),
                "--dedupe-workflow",
                "CI Run",
                "--dedupe-include-non-pr",
                "--non-pr-key",
                "branch",
                "--output-json",
                str(output_json),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(output_json.read_text(encoding="utf-8"))
        self.assertEqual(report["counts"]["candidate_runs_before_cap"], 1)
        planned_ids = [item["id"] for item in report["planned_actions"]]
        self.assertEqual(planned_ids, [201])
        reasons = report["planned_actions"][0]["reasons"]
        self.assertTrue(any(reason.startswith("dedupe-superseded-by:202") for reason in reasons))
        self.assertEqual(report["policies"]["non_pr_key"], "branch")

    def test_queue_hygiene_non_pr_sha_mode_keeps_distinct_push_commits(self) -> None:
        runs_json = self.tmp / "runs-non-pr-sha.json"
        output_json = self.tmp / "queue-hygiene-non-pr-sha.json"
        runs_json.write_text(
            json.dumps(
                {
                    "workflow_runs": [
                        {
                            "id": 301,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-301",
                            "created_at": "2026-02-27T20:00:00Z",
                        },
                        {
                            "id": 302,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-302",
                            "created_at": "2026-02-27T20:01:00Z",
                        },
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

        proc = run_cmd(
            [
                "python3",
                self._script("queue_hygiene.py"),
                "--runs-json",
                str(runs_json),
                "--dedupe-workflow",
                "CI Run",
                "--dedupe-include-non-pr",
                "--output-json",
                str(output_json),
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

        report = json.loads(output_json.read_text(encoding="utf-8"))
        self.assertEqual(report["counts"]["candidate_runs_before_cap"], 0)
        self.assertEqual(report["planned_actions"], [])
        self.assertEqual(report["policies"]["non_pr_key"], "sha")

    def test_queue_hygiene_apply_requires_authentication_token(self) -> None:
        runs_json = self.tmp / "runs-apply-auth.json"
        output_json = self.tmp / "queue-hygiene-apply-auth.json"
        runs_json.write_text(
            json.dumps(
                {
                    "workflow_runs": [
                        {
                            "id": 401,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-401",
                            "created_at": "2026-02-27T20:00:00Z",
                        },
                        {
                            "id": 402,
                            "name": "CI Run",
                            "event": "push",
                            "head_branch": "main",
                            "head_sha": "sha-402",
                            "created_at": "2026-02-27T20:01:00Z",
                        },
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

        isolated_home = self.tmp / "isolated-home"
        isolated_home.mkdir(parents=True, exist_ok=True)
        isolated_xdg = self.tmp / "isolated-xdg"
        isolated_xdg.mkdir(parents=True, exist_ok=True)

        env = dict(os.environ)
        env["GH_TOKEN"] = ""
        env["GITHUB_TOKEN"] = ""
        env["HOME"] = str(isolated_home)
        env["XDG_CONFIG_HOME"] = str(isolated_xdg)

        proc = run_cmd(
            [
                "python3",
                self._script("queue_hygiene.py"),
                "--runs-json",
                str(runs_json),
                "--dedupe-workflow",
                "CI Run",
                "--apply",
                "--output-json",
                str(output_json),
            ],
            env=env,
        )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("requires authentication token", proc.stderr.lower())


if __name__ == "__main__":  # pragma: no cover
    unittest.main(verbosity=2)
