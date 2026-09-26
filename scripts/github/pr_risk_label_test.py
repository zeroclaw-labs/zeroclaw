#!/usr/bin/env python3
"""Focused tests for the report-only pull-request risk classifier."""

from __future__ import annotations

import base64
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

try:
    from scripts.github import pr_risk_label as classifier
except ModuleNotFoundError:
    import pr_risk_label as classifier


ROOT = Path(__file__).resolve().parents[2]
POLICY_PATH = ROOT / ".github/risk-labeler.yml"
WORKFLOW_PATH = ROOT / ".github/workflows/pr-risk-labeler.yml"
HEAD = "a" * 40
BASE = "b" * 40
EXPECTED_HIGH_GLOBS = (
    "crates/zeroclaw-runtime/src/security/**",
    "crates/zeroclaw-runtime/src/approval/**",
    "crates/zeroclaw-runtime/src/trust/**",
    "crates/zeroclaw-providers/src/auth/**",
    "wit/**",
    ".cargo/**",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".github/CODEOWNERS",
    ".github/risk-labeler.yml",
    ".github/workflows/pr-risk-labeler.yml",
    ".github/workflows/*release*.yml",
    ".github/workflows/*publish*.yml",
    ".github/workflows/pub-*.yml",
    "scripts/github/pr_risk_label.py",
    "scripts/release/**",
)
EXPECTED_CONTENT_RULES = (
    "workflow permission expansion",
    "secret access",
    "OIDC token access",
    "artifact publication",
    "release behavior",
    "elevated pull_request_target",
    "toolchain install or container baseline",
    "release floor",
    "WIT contract",
)


def pull(labels: list[str] | None = None, **extra: object) -> dict[str, object]:
    value: dict[str, object] = {
        "head": {"sha": HEAD},
        "base": {"sha": BASE},
        "labels": [{"name": label} for label in labels or []],
        "changed_files": 1,
    }
    value.update(extra)
    return value


def changed_file(
    path: str,
    additions: int = 1,
    deletions: int = 0,
    patch: str | None = None,
    **extra: object,
) -> dict[str, object]:
    value: dict[str, object] = {
        "filename": path,
        "status": "modified",
        "additions": additions,
        "deletions": deletions,
        "changes": additions + deletions,
        "patch": patch,
        "contents_url": f"https://api.github.com/repos/zeroclaw-labs/zeroclaw/contents/{path}?ref={HEAD}",
    }
    value.update(extra)
    return value


def workflow_step_run(step_name: str) -> str:
    lines = WORKFLOW_PATH.read_text(encoding="utf-8").splitlines()
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


class FakeAPI:
    repository = "zeroclaw-labs/zeroclaw"

    def __init__(self, pr: dict[str, object], files: list[dict[str, object]]) -> None:
        self.pr = pr
        self.files = files
        self.pr["changed_files"] = len(files)
        self.sources: dict[tuple[str, str], dict[str, str]] = {}
        self.paginated_paths: list[str] = []

    def get_pull(self, number: int) -> dict[str, object]:
        return self.pr

    def paginate(self, path: str, expected_count: int | None = None) -> list[dict[str, object]]:
        self.paginated_paths.append(path)
        if expected_count is not None and len(self.files) > expected_count:
            raise classifier.RiskReportError("GitHub paginated response exceeds expected PR file count")
        if path.endswith("/files"):
            return self.files
        raise AssertionError(path)

    def get_source(self, path: str, revision: str) -> dict[str, str]:
        return self.sources[(path, revision)]

    def add_source(self, path: str, revision: str, content: str) -> None:
        self.sources[(path, revision)] = {
            "encoding": "base64",
            "content": base64.b64encode(content.encode()).decode(),
        }


class PaginatedAPI(classifier.GitHubAPI):
    def __init__(self, pages: list[list[dict[str, object]]]) -> None:
        super().__init__("zeroclaw-labs/zeroclaw", "token")
        self.pages = pages
        self.calls: list[str] = []

    def request(self, method: str, path: str) -> list[dict[str, object]]:
        self.calls.append(path)
        if not self.pages:
            raise AssertionError(path)
        return self.pages.pop(0)


def evaluate(api: FakeAPI) -> dict[str, object]:
    return classifier.evaluate(api, 1, POLICY_PATH)


TEST_SOURCE = """fn production() {
    println!("production");
}

#[cfg(test)]
mod tests {
    #[test]
    fn existing() {
        assert_eq!(1, 1);
    }
}
"""
TEST_PATCH = """@@ -9,1 +9,1 @@
-        assert_eq!(1, 1);
+        assert_eq!(1, 2);
"""


class RiskClassifierTest(unittest.TestCase):
    def test_policy_uses_high_risk_globs(self) -> None:
        policy = classifier.load_policy(POLICY_PATH)
        self.assertEqual(policy.high_globs, EXPECTED_HIGH_GLOBS)
        self.assertEqual(tuple(rule.name for rule in policy.content_rules), EXPECTED_CONTENT_RULES)
        self.assertTrue(
            classifier.glob_matches(
                ".github/workflows/release-stable.yml",
                ".github/workflows/*release*.yml",
            )
        )
        self.assertFalse(
            classifier.glob_matches(
                ".github/workflows/nested/release-stable.yml",
                ".github/workflows/*release*.yml",
            )
        )

    def test_repository_paths_are_encoded_segment_by_segment(self) -> None:
        self.assertEqual(
            classifier.repository_path("owner/repo", "contents/crates/foo bar/baz#qux.rs"),
            "/repos/owner/repo/contents/crates/foo%20bar/baz%23qux.rs",
        )
        with self.assertRaises(classifier.RiskReportError):
            classifier.GitHubAPI("owner/repo/extra", "token")

        api = FakeAPI(pull(), [changed_file("docs/book/src/guide.md")])
        evaluate(api)
        self.assertEqual(api.paginated_paths, ["/repos/zeroclaw-labs/zeroclaw/pulls/1/files"])

    def test_github_api_refuses_non_https_or_ambiguous_origins(self) -> None:
        invalid_origins = (
            "file:///tmp/github-api",
            "http://api.github.com",
            "https://api.github.com?debug=1",
            "https://api.github.com#fragment",
            "https://token@api.github.com",
        )
        for origin in invalid_origins:
            with self.subTest(origin=origin), self.assertRaises(classifier.RiskReportError):
                classifier.GitHubAPI("owner/repo", "token", origin)

    def test_github_api_request_targets_stay_on_configured_origin(self) -> None:
        api = classifier.GitHubAPI("owner/repo", "token", "https://github.example/api/v3")
        self.assertEqual(
            api.request_target("/repos/owner/repo/pulls/1/files?per_page=100&page=2"),
            "/api/v3/repos/owner/repo/pulls/1/files?per_page=100&page=2",
        )

        invalid_paths = (
            "repos/owner/repo",
            "file:///etc/passwd",
            "https://api.github.com/repos/owner/repo",
            "//evil.example/repos/owner/repo",
            "/repos/owner/repo#fragment",
            "/repos/owner/repo;params",
        )
        for path in invalid_paths:
            with self.subTest(path=path), self.assertRaises(classifier.RiskReportError):
                api.request_target(path)

    def test_github_api_request_uses_validated_gh_api_target(self) -> None:
        calls: list[dict[str, object]] = []

        def fake_run(*args: object, **kwargs: object) -> subprocess.CompletedProcess[bytes]:
            calls.append({"args": args, "kwargs": kwargs})
            return subprocess.CompletedProcess(args=args[0], returncode=0, stdout=b'{"ok": true}', stderr=b"")

        with mock.patch.object(classifier.subprocess, "run", fake_run):
            api = classifier.GitHubAPI("owner/repo", "token", "https://github.example/api/v3")
            self.assertEqual(api.request("GET", "/repos/owner/repo"), {"ok": True})

        self.assertEqual(len(calls), 1)
        kwargs = calls[0]["kwargs"]
        self.assertEqual(
            calls[0]["args"],
            (
                [
                    "gh",
                    "api",
                    "--method",
                    "GET",
                    "--hostname",
                    "github.example",
                    "--header",
                    "Accept: application/vnd.github+json",
                    "--header",
                    "X-GitHub-Api-Version: 2022-11-28",
                    "/api/v3/repos/owner/repo",
                ],
            ),
        )
        self.assertEqual(kwargs["check"], False)
        self.assertEqual(kwargs["capture_output"], True)
        self.assertEqual(kwargs["timeout"], 30)
        self.assertEqual(kwargs["env"]["GH_TOKEN"], "token")
        self.assertEqual(kwargs["env"]["GH_ENTERPRISE_TOKEN"], "token")

    def test_github_api_request_fails_closed_on_gh_api_errors(self) -> None:
        failures = (
            subprocess.CompletedProcess(args=["gh"], returncode=1, stdout=b"", stderr=b"error"),
            subprocess.CompletedProcess(args=["gh"], returncode=0, stdout=b"not json", stderr=b""),
        )
        for result in failures:
            with (
                self.subTest(returncode=result.returncode, stdout=result.stdout),
                mock.patch.object(classifier.subprocess, "run", return_value=result),
                self.assertRaises(classifier.RiskReportError),
            ):
                classifier.GitHubAPI("owner/repo", "token").request("GET", "/repos/owner/repo")

    def test_high_glob_proposes_high_with_matching_evidence(self) -> None:
        report = evaluate(FakeAPI(pull(), [changed_file("wit/plugin.wit")]))
        self.assertEqual(report["proposed_risk"], "risk:high")
        self.assertEqual(report["matching_evidence"][0]["path"], "wit/plugin.wit")

    def test_changed_line_policy_escalates_high_risk_workflow_content(self) -> None:
        cases = [
            (
                "workflow permission expansion",
                ".github/workflows/docs-check.yml",
                "@@ -7,1 +7,1 @@\n-  contents: read\n+  contents: write\n",
            ),
            (
                "workflow permission expansion",
                ".github/workflows/other.yml",
                "@@ -1,1 +1,1 @@\n-name: old\n+permissions: {contents: write}\n",
            ),
            (
                "workflow permission expansion",
                ".github/workflows/other.yml",
                '@@ -1,1 +1,1 @@\n-name: old\n+permissions: {"contents": "write"}\n',
            ),
            (
                "secret access",
                ".github/workflows/docs-check.yml",
                "@@ -12,1 +12,1 @@\n-          TOKEN: ${{ github.token }}\n+          TOKEN: ${{ secrets.RELEASE_TOKEN }}\n",
            ),
            (
                "OIDC token access",
                ".github/workflows/docs-check.yml",
                "@@ -8,1 +8,1 @@\n-  id-token: none\n+  id-token: write\n",
            ),
            (
                "OIDC token access",
                ".github/workflows/other.yml",
                "@@ -1,1 +1,1 @@\n-name: old\n+permissions: {id-token: write}\n",
            ),
            (
                "OIDC token access",
                ".github/workflows/other.yml",
                "@@ -1,1 +1,1 @@\n-name: old\n+permissions: {'id-token': 'write'}\n",
            ),
            (
                "artifact publication",
                ".github/workflows/docs-check.yml",
                "@@ -20,1 +20,1 @@\n-      - run: echo ok\n+      - uses: actions/upload-artifact@0123456789abcdef0123456789abcdef01234567\n",
            ),
            (
                "release behavior",
                ".github/workflows/docs-check.yml",
                "@@ -20,1 +20,1 @@\n-      - run: echo ok\n+      - run: gh release create \"$TAG\"\n",
            ),
            (
                "elevated pull_request_target",
                ".github/workflows/docs-check.yml",
                "@@ -2,1 +2,1 @@\n-  pull_request:\n+  pull_request_target:\n",
            ),
            (
                "elevated pull_request_target",
                ".github/workflows/other.yml",
                "@@ -1,1 +1,1 @@\n-on: [pull_request]\n+on: [pull_request_target]\n",
            ),
            (
                "elevated pull_request_target",
                ".github/workflows/other.yml",
                "@@ -1,1 +1,1 @@\n-on: pull_request\n+on: pull_request_target\n",
            ),
            (
                "elevated pull_request_target",
                ".github/workflows/other.yml",
                '@@ -1,1 +1,1 @@\n-on: "pull_request"\n+on: "pull_request_target"\n',
            ),
            (
                "elevated pull_request_target",
                ".github/workflows/other.yml",
                "@@ -1,1 +1,1 @@\n-on: {pull_request: {}}\n+on: {pull_request_target: {}}\n",
            ),
            (
                "toolchain install or container baseline",
                "dev/Containerfile",
                "@@ -1,1 +1,1 @@\n-FROM ubuntu:22.04\n+FROM ubuntu:24.04\n",
            ),
            (
                "release floor",
                "crates/zeroclaw-runtime/Cargo.toml",
                '@@ -5,1 +5,1 @@\n-rust-version = "1.86"\n+rust-version = "1.90"\n',
            ),
            (
                "WIT contract",
                "crates/plugin-contracts/component.wit",
                "@@ -1,1 +1,1 @@\n-package zeroclaw:old;\n+interface plugin { export run: func(); }\n",
            ),
        ]
        for rule_name, path, patch in cases:
            with self.subTest(rule_name=rule_name):
                report = evaluate(FakeAPI(pull(), [changed_file(path, 1, 1, patch)]))
                self.assertEqual(report["proposed_risk"], "risk:high")
                self.assertIn(rule_name, report["matching_evidence"][0]["content_rules"])

    def test_changed_line_policy_does_not_escalate_read_only_workflow_or_docs_mentions(self) -> None:
        workflow_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/docs-check.yml",
                        1,
                        1,
                        "@@ -7,1 +7,1 @@\n-  contents: none\n+  contents: read\n",
                    )
                ],
            )
        )
        self.assertEqual(workflow_report["proposed_risk"], "risk:medium")
        self.assertEqual(workflow_report["matching_evidence"], [])

        workflow_name_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/docs-check.yml",
                        1,
                        1,
                        "@@ -7,1 +7,1 @@\n-name: read\n+name: write\n",
                    )
                ],
            )
        )
        self.assertEqual(workflow_name_report["proposed_risk"], "risk:medium")
        self.assertEqual(workflow_name_report["matching_evidence"], [])

        workflow_release_name_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/docs-check.yml",
                        1,
                        1,
                        "@@ -7,1 +7,1 @@\n-name: docs\n+name: release docs\n",
                    )
                ],
            )
        )
        self.assertEqual(workflow_release_name_report["proposed_risk"], "risk:medium")
        self.assertEqual(workflow_release_name_report["matching_evidence"], [])

        workflow_comment_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/docs-check.yml",
                        1,
                        1,
                        "@@ -7,1 +7,1 @@\n-name: docs\n+# secrets: none\n",
                    )
                ],
            )
        )
        self.assertEqual(workflow_comment_report["proposed_risk"], "risk:medium")
        self.assertEqual(workflow_comment_report["matching_evidence"], [])

        workflow_inert_command_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/docs-check.yml",
                        1,
                        1,
                        "@@ -20,1 +20,1 @@\n-      - run: echo ok\n+      - run: echo release secrets.RELEASE_TOKEN\n",
                    )
                ],
            )
        )
        self.assertEqual(workflow_inert_command_report["proposed_risk"], "risk:medium")
        self.assertEqual(workflow_inert_command_report["matching_evidence"], [])

        docs_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        "docs/book/src/guide.md",
                        1,
                        1,
                        "@@ -1,1 +1,1 @@\n-old\n+Mention ${{ secrets.EXAMPLE }} in prose.\n",
                    )
                ],
            )
        )
        self.assertEqual(docs_report["proposed_risk"], "risk:low")
        self.assertEqual(docs_report["matching_evidence"], [])

        workflow_docs_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/master-branch-flow.md",
                        1,
                        1,
                        "@@ -51,1 +51,1 @@\n-old\n+| Tag push `vX.Y.Z` | `release-stable-manual.yml` (full release pipeline) |\n",
                    )
                ],
            )
        )
        self.assertEqual(workflow_docs_report["proposed_risk"], "risk:low")
        self.assertEqual(workflow_docs_report["matching_evidence"], [])

        fixture_report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        "scripts/github/pr_risk_label_test.py",
                        1,
                        1,
                        "@@ -1,1 +1,1 @@\n-old\n+case = '${{ secrets.RELEASE_TOKEN }} && FROM ubuntu:24.04'\n",
                    )
                ],
            )
        )
        self.assertEqual(fixture_report["proposed_risk"], "risk:medium")
        self.assertEqual(fixture_report["matching_evidence"], [])

    def test_changed_line_policy_escalates_removed_workflow_permission_restrictions(self) -> None:
        cases = [
            "@@ -7,1 +7,0 @@\n-    contents: read\n",
            "@@ -7,1 +7,0 @@\n-    permissions: read-all\n",
            "@@ -7,1 +7,0 @@\n-    permissions: {contents: none}\n",
        ]
        for patch in cases:
            with self.subTest(patch=patch):
                report = evaluate(FakeAPI(pull(), [changed_file(".github/workflows/docs-check.yml", 0, 1, patch)]))
                self.assertEqual(report["proposed_risk"], "risk:high")
                self.assertEqual(
                    report["matching_evidence"][0]["content_rules"],
                    ["workflow permission expansion"],
                )

    def test_changed_line_policy_fails_closed_when_content_sensitive_patch_is_missing(self) -> None:
        report = evaluate(FakeAPI(pull(), [changed_file(".github/workflows/docs-check.yml", patch=None)]))
        self.assertEqual(report["proposed_risk"], "risk:high")
        self.assertEqual(
            report["matching_evidence"][0]["content_rules"],
            ["content-sensitive diff unavailable"],
        )

    def test_changed_line_policy_fails_closed_when_content_sensitive_patch_is_truncated(self) -> None:
        report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file(
                        ".github/workflows/docs-check.yml",
                        2,
                        1,
                        "@@ -7,1 +7,1 @@\n-  contents: read\n+  contents: write\n",
                    )
                ],
            )
        )
        self.assertEqual(report["proposed_risk"], "risk:high")
        self.assertEqual(
            report["matching_evidence"][0]["content_rules"],
            ["content-sensitive diff unavailable"],
        )

    def test_file_evidence_must_match_captured_head(self) -> None:
        with self.assertRaisesRegex(classifier.RiskReportError, "captured head SHA"):
            evaluate(
                FakeAPI(
                    pull(),
                    [
                        changed_file(
                            "wit/plugin.wit",
                            contents_url="https://api.github.com/repos/zeroclaw-labs/zeroclaw/contents/wit/plugin.wit?ref="
                            + "c" * 40,
                        )
                    ],
                )
            )

    def test_pagination_stops_at_expected_count_and_fails_on_overflow(self) -> None:
        api = PaginatedAPI([[{}] * 100, [{}] * 50, [{}]])
        self.assertEqual(len(api.paginate("/repos/zeroclaw-labs/zeroclaw/pulls/1/files", 150)), 150)
        self.assertEqual(len(api.calls), 2)

        with self.assertRaisesRegex(classifier.RiskReportError, "exceeds expected PR file count"):
            PaginatedAPI([[{}] * 100, [{}] * 51]).paginate(
                "/repos/zeroclaw-labs/zeroclaw/pulls/1/files",
                150,
            )

    def test_docs_and_fixtures_propose_low(self) -> None:
        report = evaluate(
            FakeAPI(
                pull(),
                [
                    changed_file("docs/book/src/guide.md"),
                    changed_file("tests/fixtures/input.json"),
                ],
            )
        )
        self.assertEqual(report["proposed_risk"], "risk:low")

    def test_ordinary_source_proposes_medium(self) -> None:
        report = evaluate(FakeAPI(pull(), [changed_file("crates/zeroclaw-providers/src/openai.rs")]))
        self.assertEqual(report["proposed_risk"], "risk:medium")

    def test_manual_freeze_and_security_are_reported_without_mutation(self) -> None:
        report = evaluate(
            FakeAPI(
                pull(["risk:medium", "risk:manual", "domain:security"]),
                [changed_file("src/providers/openai.rs")],
            )
        )
        self.assertTrue(report["risk_manual"])
        self.assertTrue(report["mutation_freeze"])
        self.assertTrue(report["domain_security"])
        self.assertFalse(report["mutations_attempted"])
        self.assertIn("risk:manual freezes future automatic risk replacement", report["mismatches"])

    def test_summary_escapes_untrusted_markdown_text(self) -> None:
        report = evaluate(
            FakeAPI(
                pull(["risk:`manual`"]),
                [
                    changed_file(
                        "wit/![x](https:attacker.invalid_pixel.png)```escape.wit",
                    )
                ],
            )
        )
        summary = classifier.human_summary(report)
        self.assertIn(
            r"wit/\!\[x\]\(https:attacker\.invalid\_pixel\.png\)\`\`\`escape\.wit",
            summary,
        )
        self.assertEqual(classifier.summary_text("risk:`manual`\n"), r"risk:\`manual\`\\n")
        self.assertEqual(classifier.summary_text("~~untrusted~~"), r"\~\~untrusted\~\~")

    def test_summary_json_is_indented_without_markdown_fences(self) -> None:
        report = evaluate(FakeAPI(pull(["risk:high"]), [changed_file("wit/```escape.wit")]))
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary.md"
            classifier.write_summary(summary, report)
            text = summary.read_text(encoding="utf-8")
            self.assertNotIn("\n```", text)
            self.assertNotIn("```json", text)
            self.assertIn("JSON report:\n\n    {", text)
            self.assertIn('"path": "wit/```escape.wit"', text)

    def test_9530_positive_test_only_rust_high_path_proposes_medium(self) -> None:
        path = "crates/zeroclaw-runtime/src/security/policy.rs"
        api = FakeAPI(pull(), [changed_file(path, 1, 1, TEST_PATCH)])
        api.add_source(path, BASE, TEST_SOURCE)
        api.add_source(path, HEAD, TEST_SOURCE.replace("assert_eq!(1, 1)", "assert_eq!(1, 2)"))
        report = evaluate(api)
        self.assertEqual(report["proposed_risk"], "risk:medium")
        self.assertTrue(report["exception_9530"]["applied"])

    def test_9530_negative_cases_propose_high(self) -> None:
        path = "crates/zeroclaw-runtime/src/security/policy.rs"
        cases = [
            ("missing patch", changed_file(path, 1, 1, None), TEST_SOURCE, TEST_SOURCE),
            (
                "truncated patch",
                changed_file(path, 2, 1, TEST_PATCH),
                TEST_SOURCE,
                TEST_SOURCE.replace("assert_eq!(1, 1)", "assert_eq!(1, 2)"),
            ),
            (
                "cfg boundary",
                changed_file(
                    path,
                    1,
                    1,
                    "@@ -9,1 +9,1 @@\n-        assert_eq!(1, 1);\n+        #[cfg(test)]\n",
                ),
                TEST_SOURCE,
                TEST_SOURCE,
            ),
            (
                "production line",
                changed_file(
                    path,
                    1,
                    1,
                    '@@ -1,1 +1,1 @@\n-fn production() {\n+fn changed() {\n',
                ),
                TEST_SOURCE,
                TEST_SOURCE.replace("fn production()", "fn changed()"),
            ),
            (
                "brace move changes cfg membership",
                changed_file(
                    path,
                    1,
                    1,
                    "@@ -6,7 +6,7 @@\n mod tests {\n     #[test]\n     fn existing() {\n         assert_eq!(1, 1);\n     }\n-}\n \n fn production() {\n     println!(\"production\");\n }\n+}\n",
                ),
                """#[cfg(test)]
mod tests {
    #[test]
    fn existing() {
        assert_eq!(1, 1);
    }
}

fn production() {
    println!("production");
}
""",
                """#[cfg(test)]
mod tests {
    #[test]
    fn existing() {
        assert_eq!(1, 1);
    }

fn production() {
    println!("production");
}
                }
                """,
            ),
            (
                "commented cfg attribute does not create test range",
                changed_file(
                    path,
                    1,
                    1,
                    "@@ -5,1 +5,1 @@\n-    return false;\n+    return true;\n",
                ),
                """/*
#[cfg(test)]
*/
pub fn allowed() -> bool {
    return false;
}
""",
                """/*
#[cfg(test)]
*/
pub fn allowed() -> bool {
    return true;
}
""",
            ),
            (
                "raw string cfg attribute does not create test range",
                changed_file(
                    path,
                    1,
                    1,
                    "@@ -6,1 +6,1 @@\n-    return false;\n+    return true;\n",
                ),
                '''const NOTE: &str = r#"
#[cfg(test)]
"#;
pub fn allowed() -> bool {
    return false;
}
''',
                '''const NOTE: &str = r#"
#[cfg(test)]
"#;
pub fn allowed() -> bool {
    return true;
}
''',
            ),
        ]
        for name, file, base_source, head_source in cases:
            with self.subTest(name=name):
                api = FakeAPI(pull(), [file])
                api.add_source(path, BASE, base_source)
                api.add_source(path, HEAD, head_source)
                report = evaluate(api)
                self.assertEqual(report["proposed_risk"], "risk:high")
                self.assertFalse(report["exception_9530"]["applied"])

    def test_9530_negative_statuses_and_mixed_files_propose_high(self) -> None:
        path = "crates/zeroclaw-runtime/src/security/policy.rs"
        for status in ("added", "removed", "renamed", "copied"):
            with self.subTest(status=status):
                file = changed_file(path, 1, 1, TEST_PATCH, status=status)
                if status in {"renamed", "copied"}:
                    file["previous_filename"] = path
                api = FakeAPI(pull(), [file])
                api.add_source(path, BASE, TEST_SOURCE)
                api.add_source(path, HEAD, TEST_SOURCE.replace("assert_eq!(1, 1)", "assert_eq!(1, 2)"))
                self.assertEqual(evaluate(api)["proposed_risk"], "risk:high")

        api = FakeAPI(
            pull(),
            [
                changed_file(path, 1, 1, TEST_PATCH),
                changed_file("crates/zeroclaw-runtime/src/policy.rs"),
            ],
        )
        api.add_source(path, BASE, TEST_SOURCE)
        api.add_source(path, HEAD, TEST_SOURCE.replace("assert_eq!(1, 1)", "assert_eq!(1, 2)"))
        self.assertEqual(evaluate(api)["proposed_risk"], "risk:high")

    def test_9530_source_proof_has_bounded_file_count(self) -> None:
        files = [
            changed_file(f"crates/zeroclaw-runtime/src/security/policy_{index}.rs", 1, 1, TEST_PATCH)
            for index in range(classifier.MAX_TEST_ONLY_SOURCE_FILES + 1)
        ]
        report = evaluate(FakeAPI(pull(), files))
        self.assertEqual(report["proposed_risk"], "risk:high")
        self.assertFalse(report["exception_9530"]["applied"])
        self.assertIn("too many high-risk Rust files", report["exception_9530"]["detail"])

    def test_9530_malformed_source_and_patch_mismatch_propose_high(self) -> None:
        path = "crates/zeroclaw-runtime/src/security/policy.rs"
        report = evaluate(FakeAPI(pull(), [changed_file(path, 1, 1, TEST_PATCH)]))
        self.assertEqual(report["proposed_risk"], "risk:high")

        api = FakeAPI(pull(), [changed_file(path, 1, 1, TEST_PATCH)])
        api.add_source(path, BASE, "fn broken() {\n")
        api.add_source(path, HEAD, "fn broken() {\n")
        report = evaluate(api)
        self.assertEqual(report["proposed_risk"], "risk:high")

    def test_9530_lexer_does_not_extend_test_scope_through_literals_or_comments(self) -> None:
        path = "crates/zeroclaw-runtime/src/security/policy.rs"
        base_source = '''mod outer {
    #[cfg(test)]
    mod tests {
        const OPEN: &str = "{";
        const RAW: &str = r#"}"#;
        const BRACE: char = '{';
        /* } nested /* { */ comment */
    }

    fn production() {
        run_old();
        let _ = "}";
    }
}
'''
        head_source = base_source.replace("run_old();", "run_new();")
        patch = "@@ -11,1 +11,1 @@\n-        run_old();\n+        run_new();\n"
        api = FakeAPI(pull(), [changed_file(path, 1, 1, patch)])
        api.add_source(path, BASE, base_source)
        api.add_source(path, HEAD, head_source)
        report = evaluate(api)
        self.assertEqual(report["proposed_risk"], "risk:high")
        self.assertFalse(report["exception_9530"]["applied"])

        api = FakeAPI(pull(), [changed_file(path, 1, 1, TEST_PATCH)])
        api.add_source(path, BASE, TEST_SOURCE.replace("assert_eq!(1, 1)", "assert_eq!(9, 9)"))
        api.add_source(path, HEAD, TEST_SOURCE.replace("assert_eq!(1, 1)", "assert_eq!(1, 2)"))
        report = evaluate(api)
        self.assertEqual(report["proposed_risk"], "risk:high")

    def test_malformed_metadata_and_incomplete_files_fail_closed(self) -> None:
        with self.assertRaises(classifier.RiskReportError):
            classifier.parse_pr_metadata(
                {
                    "head": {"sha": HEAD},
                    "base": {"sha": BASE},
                    "labels": [{"bad": "label"}],
                    "changed_files": 1,
                }
            )
        with self.assertRaises(classifier.RiskReportError):
            classifier.validate_files([{"filename": "src/lib.rs", "status": "modified"}], 1)
        with self.assertRaises(classifier.RiskReportError):
            classifier.validate_files([], 1)
        with self.assertRaises(classifier.RiskReportError):
            classifier.validate_files([changed_file("src/lib.rs")], 1, "c" * 40)

    def test_main_emits_human_summary_and_json_without_mutation(self) -> None:
        output = io.StringIO()
        api = FakeAPI(pull(["risk:high"]), [changed_file("wit/plugin.wit")])
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary.md"
            with contextlib.redirect_stdout(output):
                result = classifier.main(
                    [
                        "--pr-number",
                        "1",
                        "--policy",
                        str(POLICY_PATH),
                        "--summary",
                        str(summary),
                    ],
                    api,
                )
            self.assertEqual(result, 0)
            payload = json.loads(output.getvalue().splitlines()[-1])
            self.assertTrue(payload["report_only"])
            self.assertFalse(payload["mutations_attempted"])
            self.assertIn("PR risk report (report-only)", output.getvalue())
            self.assertIn("Proposed risk: risk:high", summary.read_text(encoding="utf-8"))

    def test_invalid_pr_number_emits_structured_fail_closed_report(self) -> None:
        output = io.StringIO()
        errors = io.StringIO()
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary.md"
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                result = classifier.main(
                    [
                        "--pr-number",
                        "0",
                        "--policy",
                        str(POLICY_PATH),
                        "--summary",
                        str(summary),
                    ],
                    FakeAPI(pull(), [changed_file("docs/book/src/guide.md")]),
                )
            self.assertEqual(result, 1)
            self.assertIn("PR risk report failed closed: PR number is invalid", errors.getvalue())
            self.assertIn('"mutations_attempted": false', output.getvalue())
            self.assertIn("Risk report failed closed: PR number is invalid", summary.read_text(encoding="utf-8"))


class WorkflowContractTest(unittest.TestCase):
    def test_workflow_is_read_only_and_trusted(self) -> None:
        workflow = WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertIn("pull_request_target:", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("contents: read", workflow)
        self.assertIn("pull-requests: read", workflow)
        self.assertNotIn("write", workflow)
        self.assertNotIn("actions/checkout", workflow)
        self.assertNotIn("statuses", workflow)
        self.assertNotIn("POST", workflow)
        self.assertNotIn("DELETE", workflow)
        self.assertIn("github.event.action != 'labeled'", workflow)
        self.assertIn("github.event.action != 'unlabeled'", workflow)
        self.assertIn("startsWith(github.event.label.name, 'risk:')", workflow)
        self.assertIn("github.event.label.name == 'domain:security'", workflow)
        self.assertIn(
            "TRUSTED_REF: ${{ github.event_name == 'pull_request_target' && github.sha || github.event.repository.default_branch }}",
            workflow,
        )
        self.assertIn("?ref=$TRUSTED_REF", workflow)
        self.assertIn('test -s "$output"', workflow)
        self.assertIn('"$RUNNER_TEMP/pr_risk_label.py"', workflow)

    def test_workflow_runs_classifier_after_trusted_fetch(self) -> None:
        workflow = WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertLess(
            workflow.index("- name: Fetch trusted classifier and policy"),
            workflow.index("- name: Generate report-only risk evidence"),
        )
        self.assertIn('fetch_trusted "scripts/github/pr_risk_label.py"', workflow)
        self.assertIn('fetch_trusted ".github/risk-labeler.yml"', workflow)
        self.assertIn('--summary "$GITHUB_STEP_SUMMARY"', workflow)

    def test_workflow_fetch_step_fails_closed(self) -> None:
        fetch_step = workflow_step_run("Fetch trusted classifier and policy")

        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            gh_stub = bin_path / "gh"
            gh_stub.write_text(
                "#!/bin/sh\n"
                'case "$1" in api) ;; *) exit 45 ;; esac\n'
                'case "$FETCH_CASE" in api-failure) exit 42 ;; esac\n'
                'case "$*" in *pr_risk_label.py*) printf "%s" "$ENCODED_CLASSIFIER" ;; *risk-labeler.yml*) printf "%s" "$ENCODED_POLICY" ;; *) exit 46 ;; esac\n',
                encoding="utf-8",
            )
            base64_stub = bin_path / "base64"
            base64_stub.write_text(
                "#!/bin/sh\n"
                '[ "$1" = --decode ] || exit 44\n'
                '[ "$FETCH_CASE" = decode-failure ] && exit 43\n'
                "python3 -c 'import base64, sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.buffer.read(), validate=True))'\n",
                encoding="utf-8",
            )
            gh_stub.chmod(0o755)
            base64_stub.chmod(0o755)

            for fetch_case in ("api-failure", "decode-failure", "empty-classifier", "empty-policy", "success"):
                with self.subTest(fetch_case=fetch_case):
                    classifier_path = temp_path / "pr_risk_label.py"
                    policy_path = temp_path / "risk-labeler.yml"
                    classifier_path.unlink(missing_ok=True)
                    policy_path.unlink(missing_ok=True)
                    encoded_classifier = "" if fetch_case == "empty-classifier" else base64.b64encode(b"print('ok')\n").decode("ascii")
                    encoded_policy = "" if fetch_case == "empty-policy" else base64.b64encode(b'{"risk:high":[]}\n').decode("ascii")
                    env = {
                        **os.environ,
                        "ENCODED_CLASSIFIER": encoded_classifier,
                        "ENCODED_POLICY": encoded_policy,
                        "FETCH_CASE": fetch_case,
                        "GH_TOKEN": "test-token",
                        "REPOSITORY": "zeroclaw-labs/zeroclaw",
                        "RUNNER_TEMP": str(temp_path),
                        "TRUSTED_REF": "trusted-workflow-sha",
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
                        self.assertEqual(classifier_path.read_text(encoding="utf-8"), "print('ok')\n")
                        self.assertEqual(policy_path.read_text(encoding="utf-8"), '{"risk:high":[]}\n')
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        if fetch_case == "empty-classifier":
                            self.assertIn(
                                "trusted file fetch produced an empty file: scripts/github/pr_risk_label.py",
                                result.stderr,
                            )
                        if fetch_case == "empty-policy":
                            self.assertIn(
                                "trusted file fetch produced an empty file: .github/risk-labeler.yml",
                                result.stderr,
                            )


if __name__ == "__main__":
    unittest.main()
