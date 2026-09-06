#!/usr/bin/env python3
"""Tests for PR size-label classification."""

from __future__ import annotations

import base64
import contextlib
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import pr_size_label as size_labeler


REPO_ROOT = Path(__file__).resolve().parents[2]


def change(filename: str, additions: int, deletions: int = 0) -> size_labeler.FileChange:
    return size_labeler.FileChange(filename=filename, additions=additions, deletions=deletions)


def workflow_step_run(step_name: str) -> str:
    workflow = (REPO_ROOT / ".github/workflows/pr-size-labeler.yml").read_text(encoding="utf-8")
    lines = workflow.splitlines()
    step_index = lines.index(f"      - name: {step_name}")
    run_index = next(
        index for index in range(step_index + 1, len(lines)) if lines[index] == "        run: |"
    )
    body: list[str] = []
    for line in lines[run_index + 1 :]:
        if line and not line.startswith("          "):
            break
        body.append(line[10:] if line else "")
    return "\n".join(body).rstrip() + "\n"


class PrSizeLabelTest(unittest.TestCase):
    def test_threshold_boundaries(self) -> None:
        cases = [
            (0, "size:XS"),
            (80, "size:XS"),
            (81, "size:S"),
            (250, "size:S"),
            (251, "size:M"),
            (500, "size:M"),
            (501, "size:L"),
            (1000, "size:L"),
            (1001, "size:XL"),
        ]
        for changed_lines, expected in cases:
            with self.subTest(changed_lines=changed_lines):
                self.assertEqual(size_labeler.select_size_label(changed_lines), expected)

    def test_docs_like_files_do_not_count_toward_effective_size(self) -> None:
        files = [
            change("docs/book/src/maintainers/labels.md", 1000),
            change(".github/ISSUE_TEMPLATE/feature.yml", 1000),
            change(".github/pull_request_template.md", 1000),
            change("README.md", 1000),
            change("crates/zeroclaw-config/src/policy.rs", 10, 5),
        ]
        self.assertEqual(size_labeler.effective_changed_lines(files), 15)

    def test_cargo_lock_does_not_count_toward_effective_size(self) -> None:
        files = [
            change("Cargo.lock", 5000, 2000),
            change("Cargo.toml", 20, 5),
        ]
        self.assertEqual(size_labeler.effective_changed_lines(files), 25)

    def test_plan_adds_first_size_label(self) -> None:
        plan = size_labeler.plan_size_label([change("src/main.rs", 81)], {"bug"})
        self.assertEqual(plan.selected_label, "size:S")
        self.assertEqual(plan.labels_to_add, ("size:S",))
        self.assertEqual(plan.labels_to_remove, ())

    def test_plan_replaces_stale_canonical_size_label(self) -> None:
        plan = size_labeler.plan_size_label(
            [change("src/main.rs", 251)],
            {"size:XS", "risk:low"},
        )
        self.assertEqual(plan.selected_label, "size:M")
        self.assertEqual(plan.labels_to_add, ("size:M",))
        self.assertEqual(plan.labels_to_remove, ("size:XS",))

    def test_plan_removes_extra_canonical_size_labels_without_touching_legacy_spelling(self) -> None:
        plan = size_labeler.plan_size_label(
            [change("src/main.rs", 10)],
            {"size:XS", "size:S", "size: M"},
        )
        self.assertEqual(plan.labels_to_add, ())
        self.assertEqual(plan.labels_to_remove, ("size:S",))

    def test_file_change_rejects_malformed_api_payload(self) -> None:
        with self.assertRaisesRegex(ValueError, "must be an object"):
            size_labeler.file_change_from_api("not-an-object")  # type: ignore[arg-type]
        with self.assertRaisesRegex(ValueError, "invalid additions"):
            size_labeler.file_change_from_api(
                {"filename": "src/main.rs", "additions": -1, "deletions": 0}
            )
        with self.assertRaisesRegex(ValueError, "invalid additions"):
            size_labeler.file_change_from_api(
                {"filename": "src/main.rs", "additions": True, "deletions": 0}
            )
        with self.assertRaisesRegex(ValueError, "invalid deletions"):
            size_labeler.file_change_from_api(
                {"filename": "src/main.rs", "additions": 0, "deletions": False}
            )

    def test_client_rejects_non_api_urls(self) -> None:
        client = size_labeler.GitHubClient("token")
        self.assertEqual(client._parse_url("/repos/zeroclaw-labs/zeroclaw").scheme, "https")
        with self.assertRaisesRegex(ValueError, "non-GitHub-API URL"):
            client._parse_url("file:///tmp/token")
        with self.assertRaisesRegex(ValueError, "non-GitHub-API URL"):
            client._parse_url("https://example.com/repos/zeroclaw-labs/zeroclaw")

    def test_client_sends_required_user_agent_header(self) -> None:
        captured: dict[str, object] = {}

        class FakeResponse:
            status = 200

            def read(self) -> bytes:
                return b"{}"

            def getheaders(self) -> list[tuple[str, str]]:
                return []

        class FakeConnection:
            def __init__(self, netloc: str, timeout: int) -> None:
                captured["netloc"] = netloc
                captured["timeout"] = timeout

            def request(
                self,
                method: str,
                path: str,
                body: bytes | None = None,
                headers: dict[str, str] | None = None,
            ) -> None:
                captured["method"] = method
                captured["path"] = path
                captured["body"] = body
                captured["headers"] = headers or {}

            def getresponse(self) -> FakeResponse:
                return FakeResponse()

            def close(self) -> None:
                captured["closed"] = True

        client = size_labeler.GitHubClient("token")
        with mock.patch.object(size_labeler.http.client, "HTTPSConnection", FakeConnection):
            client._send("GET", "/repos/zeroclaw-labs/zeroclaw/pulls/1")

        headers = captured["headers"]
        self.assertIsInstance(headers, dict)
        self.assertEqual(headers["User-Agent"], size_labeler.USER_AGENT)
        self.assertEqual(headers["Accept"], "application/vnd.github+json")

    def test_load_pr_changed_file_count_reads_trusted_metadata(self) -> None:
        class FakeClient:
            def request(self, method: str, path: str) -> dict[str, int]:
                self.method = method
                self.path = path
                return {"changed_files": 3}

        client = FakeClient()
        self.assertEqual(size_labeler.load_pr_changed_file_count(client, "zeroclaw-labs/zeroclaw", 9), 3)
        self.assertEqual(client.method, "GET")
        self.assertEqual(client.path, "/repos/zeroclaw-labs/zeroclaw/pulls/9")

    def test_load_pr_changed_file_count_rejects_malformed_metadata(self) -> None:
        class FakeClient:
            def __init__(self, payload: object) -> None:
                self.payload = payload

            def request(self, method: str, path: str) -> object:
                return self.payload

        with self.assertRaisesRegex(ValueError, "must be an object"):
            size_labeler.load_pr_changed_file_count(FakeClient([]), "zeroclaw-labs/zeroclaw", 9)
        with self.assertRaisesRegex(ValueError, "invalid changed_files"):
            size_labeler.load_pr_changed_file_count(
                FakeClient({"changed_files": True}),
                "zeroclaw-labs/zeroclaw",
                9,
            )

    def test_load_pr_files_rejects_incomplete_capped_file_list(self) -> None:
        class FakeClient:
            def paginate(self, path: str) -> list[dict[str, object]]:
                self.path = path
                return [
                    {"filename": f"docs/generated-{index}.md", "additions": 1, "deletions": 0}
                    for index in range(3000)
                ]

        client = FakeClient()
        with self.assertRaisesRegex(ValueError, "incomplete PR file list"):
            size_labeler.load_pr_files(client, "zeroclaw-labs/zeroclaw", 9, expected_file_count=3001)
        self.assertEqual(client.path, "/repos/zeroclaw-labs/zeroclaw/pulls/9/files?per_page=100")

    def test_load_pr_files_accepts_complete_file_list(self) -> None:
        class FakeClient:
            def paginate(self, path: str) -> list[dict[str, object]]:
                return [
                    {"filename": "docs/guide.md", "additions": 5, "deletions": 0},
                    {"filename": "src/main.rs", "additions": 7, "deletions": 1},
                ]

        files = size_labeler.load_pr_files(
            FakeClient(),
            "zeroclaw-labs/zeroclaw",
            9,
            expected_file_count=2,
        )
        self.assertEqual(files, [change("docs/guide.md", 5), change("src/main.rs", 7, 1)])

    def test_main_refuses_incomplete_file_list_before_label_read_or_mutation(self) -> None:
        calls: list[tuple[str, str, str]] = []

        class FakeClient:
            def __init__(self, token: str, api_url: str) -> None:
                calls.append(("init", token, api_url))

            def request(
                self,
                method: str,
                path: str,
                payload: dict[str, object] | None = None,
            ) -> object:
                calls.append(("request", method, path))
                if path == "/repos/zeroclaw-labs/zeroclaw/pulls/9":
                    return {"changed_files": 3001}
                raise AssertionError(f"unexpected label request or mutation: {method} {path}")

            def paginate(self, path: str) -> list[dict[str, object]]:
                calls.append(("paginate", "GET", path))
                return [
                    {"filename": f"docs/generated-{index}.md", "additions": 1, "deletions": 0}
                    for index in range(3000)
                ]

        with mock.patch.object(size_labeler, "GitHubClient", FakeClient):
            with self.assertRaisesRegex(ValueError, "incomplete PR file list"):
                size_labeler.main(
                    [
                        "--repo",
                        "zeroclaw-labs/zeroclaw",
                        "--pr",
                        "9",
                        "--token",
                        "token",
                        "--dry-run",
                    ]
                )

        self.assertNotIn(("request", "GET", "/repos/zeroclaw-labs/zeroclaw/issues/9/labels?per_page=100"), calls)
        self.assertFalse(any("/issues/9/labels" in call[2] for call in calls if len(call) == 3))

    def test_main_dry_run_prints_plan_without_label_mutation(self) -> None:
        calls: list[tuple[str, str, str]] = []

        class FakeClient:
            def __init__(self, token: str, api_url: str) -> None:
                calls.append(("init", token, api_url))

            def request(
                self,
                method: str,
                path: str,
                payload: dict[str, object] | None = None,
            ) -> object:
                calls.append(("request", method, path))
                if method == "GET" and path == "/repos/zeroclaw-labs/zeroclaw/pulls/9":
                    return {"changed_files": 1}
                if method in {"POST", "DELETE"}:
                    raise AssertionError(f"dry run must not mutate labels: {method} {path}")
                raise AssertionError(f"unexpected request: {method} {path}")

            def paginate(self, path: str) -> list[dict[str, object]]:
                calls.append(("paginate", "GET", path))
                if path == "/repos/zeroclaw-labs/zeroclaw/pulls/9/files?per_page=100":
                    return [{"filename": "src/main.rs", "additions": 501, "deletions": 0}]
                if path == "/repos/zeroclaw-labs/zeroclaw/issues/9/labels?per_page=100":
                    return [{"name": "size:M"}]
                raise AssertionError(f"unexpected pagination: {path}")

        stdout = io.StringIO()
        with mock.patch.object(size_labeler, "GitHubClient", FakeClient):
            with contextlib.redirect_stdout(stdout):
                size_labeler.main(
                    [
                        "--repo",
                        "zeroclaw-labs/zeroclaw",
                        "--pr",
                        "9",
                        "--token",
                        "token",
                        "--dry-run",
                    ]
                )

        payload = json.loads(stdout.getvalue())
        self.assertTrue(payload["dry_run"])
        self.assertEqual(payload["effective_changed_lines"], 501)
        self.assertEqual(payload["selected_label"], "size:L")
        self.assertEqual(payload["labels_to_add"], ["size:L"])
        self.assertEqual(payload["labels_to_remove"], ["size:M"])
        self.assertNotIn(("request", "POST", "/repos/zeroclaw-labs/zeroclaw/issues/9/labels"), calls)
        self.assertFalse(any(call[0] == "request" and call[1] == "DELETE" for call in calls))

    def test_apply_size_plan_posts_and_deletes_only_planned_canonical_size_labels(self) -> None:
        calls: list[tuple[str, str, dict[str, object] | None]] = []

        class FakeClient:
            def request(
                self,
                method: str,
                path: str,
                payload: dict[str, object] | None = None,
            ) -> object:
                calls.append((method, path, payload))
                return None

        plan = size_labeler.SizePlan(
            effective_changed_lines=514,
            selected_label="size:L",
            labels_to_add=("size:L",),
            labels_to_remove=("size:M", "size:XS"),
        )

        size_labeler.apply_size_plan(FakeClient(), "zeroclaw-labs/zeroclaw", 9, plan)

        self.assertEqual(
            calls,
            [
                ("POST", "/repos/zeroclaw-labs/zeroclaw/issues/9/labels", {"labels": ["size:L"]}),
                ("DELETE", "/repos/zeroclaw-labs/zeroclaw/issues/9/labels/size%3AM", None),
                ("DELETE", "/repos/zeroclaw-labs/zeroclaw/issues/9/labels/size%3AXS", None),
            ],
        )

    def test_apply_size_plan_treats_missing_stale_label_as_already_removed(self) -> None:
        class FakeClient:
            def request(
                self,
                method: str,
                path: str,
                payload: dict[str, object] | None = None,
            ) -> object:
                raise size_labeler.GitHubHTTPError(404, "not found")

        plan = size_labeler.SizePlan(
            effective_changed_lines=514,
            selected_label="size:L",
            labels_to_add=(),
            labels_to_remove=("size:M",),
        )

        size_labeler.apply_size_plan(FakeClient(), "zeroclaw-labs/zeroclaw", 9, plan)

    def test_workflow_fetches_workflow_classifier_without_checking_out_pr_code(self) -> None:
        workflow = (REPO_ROOT / ".github/workflows/pr-size-labeler.yml").read_text(encoding="utf-8")

        self.assertIn("pull_request_target:", workflow)
        self.assertNotIn("actions/checkout", workflow)
        self.assertIn("WORKFLOW_SHA: ${{ github.sha }}", workflow)
        self.assertIn("?ref=$WORKFLOW_SHA", workflow)
        self.assertNotIn("github.event.pull_request.base.sha", workflow)
        self.assertIn("set -euo pipefail", workflow)
        self.assertIn('test -s "$RUNNER_TEMP/pr_size_label.py"', workflow)
        self.assertIn('"$RUNNER_TEMP/pr_size_label.py"', workflow)
        self.assertLess(
            workflow.index("- name: Fetch trusted workflow classifier"),
            workflow.index("- name: Apply size label from PR metadata"),
        )
        self.assertIn("issues: write", workflow)
        self.assertIn("pull-requests: read", workflow)

    def test_workflow_fetch_step_fails_closed(self) -> None:
        fetch_step = workflow_step_run("Fetch trusted workflow classifier")

        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            gh_stub = bin_path / "gh"
            gh_stub.write_text(
                "#!/bin/sh\n"
                'if [ "$FETCH_CASE" = empty ]; then exit 0; fi\n'
                "printf '%s' \"$ENCODED_CLASSIFIER\"\n"
                'if [ "$FETCH_CASE" = api-failure ]; then exit 42; fi\n',
                encoding="utf-8",
            )
            base64_stub = bin_path / "base64"
            base64_stub.write_text(
                "#!/bin/sh\n"
                '[ "$1" = --decode ] || exit 44\n'
                '[ "$FETCH_CASE" = decode-failure ] && exit 43\n'
                "python3 -c 'import base64, sys; "
                "sys.stdout.buffer.write(base64.b64decode(sys.stdin.buffer.read(), validate=True))'\n",
                encoding="utf-8",
            )
            gh_stub.chmod(0o755)
            base64_stub.chmod(0o755)

            for fetch_case in ("api-failure", "decode-failure", "empty", "success"):
                with self.subTest(fetch_case=fetch_case):
                    classifier_path = temp_path / "pr_size_label.py"
                    classifier_path.unlink(missing_ok=True)
                    env = {
                        **os.environ,
                        "ENCODED_CLASSIFIER": base64.b64encode(b'print("ok")\n').decode("ascii"),
                        "FETCH_CASE": fetch_case,
                        "GH_TOKEN": "test-token",
                        "GITHUB_REPOSITORY": "zeroclaw-labs/zeroclaw",
                        "RUNNER_TEMP": str(temp_path),
                        "WORKFLOW_SHA": "trusted-workflow-sha",
                        "PATH": f"{bin_path}{os.pathsep}{os.environ['PATH']}",
                    }
                    result = subprocess.run(
                        ["bash", "--noprofile", "--norc", "-c", fetch_step],
                        check=False,
                        capture_output=True,
                        env=env,
                        text=True,
                    )

                    if fetch_case == "success":
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(classifier_path.read_text(encoding="utf-8"), 'print("ok")\n')
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        if fetch_case == "api-failure":
                            self.assertEqual(
                                classifier_path.read_text(encoding="utf-8"), 'print("ok")\n'
                            )
                        else:
                            self.assertFalse(classifier_path.exists() and classifier_path.stat().st_size)
                    if fetch_case == "empty":
                        self.assertIn("trusted classifier fetch produced an empty file", result.stderr)

    def test_default_docs_contract_matches_repository_docs(self) -> None:
        size_labeler.validate_docs_contract()

    def test_docs_threshold_parser_matches_expected_table(self) -> None:
        docs = "\n".join(
            [
                size_labeler.DOCS_LIKE_CONTRACT_SENTENCE,
                "| Label | Threshold |",
                "|---|---|",
                "| `size:XS` | <= 80 lines |",
                "| `size:S` | <= 250 lines |",
                "| `size:M` | <= 500 lines |",
                "| `size:L` | <= 1000 lines |",
                "| `size:XL` | > 1000 lines |",
            ]
        )
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "labels.md"
            path.write_text(docs, encoding="utf-8")
            self.assertEqual(size_labeler.docs_thresholds(path), dict(size_labeler.SIZE_THRESHOLDS))
            size_labeler.validate_docs_contract(path)

    def test_docs_contract_rejects_missing_exclusion_sentence(self) -> None:
        docs = "\n".join(
            [
                "| Label | Threshold |",
                "|---|---|",
                "| `size:XS` | <= 80 lines |",
                "| `size:S` | <= 250 lines |",
                "| `size:M` | <= 500 lines |",
                "| `size:L` | <= 1000 lines |",
                "| `size:XL` | > 1000 lines |",
            ]
        )
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "labels.md"
            path.write_text(docs, encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "exclusion contract"):
                size_labeler.validate_docs_contract(path)

    def test_plan_json_is_stable(self) -> None:
        plan = size_labeler.SizePlan(81, "size:S", ("size:S",), ("size:XS",))
        payload = json.loads(size_labeler.plan_as_json(plan, dry_run=True))
        self.assertEqual(payload["selected_label"], "size:S")
        self.assertTrue(payload["dry_run"])
        self.assertTrue(payload["changed"])


if __name__ == "__main__":
    unittest.main()
