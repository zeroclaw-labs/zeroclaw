#!/usr/bin/env python3
"""Behavioral tests for CI helper scripts under scripts/ci."""

from __future__ import annotations

import contextlib
import hashlib
import http.server
import json
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


if __name__ == "__main__":  # pragma: no cover
    unittest.main(verbosity=2)
