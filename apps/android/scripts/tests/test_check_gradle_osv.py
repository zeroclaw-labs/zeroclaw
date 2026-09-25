import importlib.util
import json
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).parents[1] / "check-gradle-osv.py"
SPEC = importlib.util.spec_from_file_location("check_gradle_osv", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)


class GradleOsvCheckTest(unittest.TestCase):
    def test_parse_components_uses_resolved_version(self):
        report = """
        +--- com.squareup.okhttp3:okhttp:3.12.1 -> 4.12.0
        |    \\--- com.squareup.okio:okio:3.6.0
        \\--- org.apache.sshd:sshd-core:2.19.0
        """
        self.assertEqual(
            MODULE.parse_components(report),
            {
                ("com.squareup.okhttp3:okhttp", "4.12.0"),
                ("com.squareup.okio:okio", "3.6.0"),
                ("org.apache.sshd:sshd-core", "2.19.0"),
            },
        )

    def test_parse_components_ignores_non_dependency_lines(self):
        self.assertEqual(
            MODULE.parse_components("BUILD SUCCESSFUL\nNo dependencies"),
            set(),
        )

    def test_merging_reports_preserves_two_versions_of_same_coordinate(self):
        reports = (
            "\\--- org.example:shared-runtime:1.0.0",
            "\\--- org.example:shared-runtime:2.0.0",
        )

        self.assertEqual(
            MODULE.merge_components(list(reports)),
            {
                ("org.example:shared-runtime", "1.0.0"),
                ("org.example:shared-runtime", "2.0.0"),
            },
        )

    def test_query_osv_queries_each_coordinate_version_pair(self):
        response = mock.Mock(status=200)
        response.read.return_value = json.dumps({"results": [{}, {}]}).encode()
        connection = mock.Mock()
        connection.getresponse.return_value = response

        with (
            mock.patch.object(MODULE.ssl, "create_default_context"),
            mock.patch.object(
                MODULE.http.client,
                "HTTPSConnection",
                return_value=connection,
            ),
        ):
            self.assertEqual(
                MODULE.query_osv(
                    {
                        ("org.example:shared-runtime", "1.0.0"),
                        ("org.example:shared-runtime", "2.0.0"),
                    }
                ),
                [],
            )

        payload = json.loads(connection.request.call_args.kwargs["body"])
        self.assertEqual(
            payload["queries"],
            [
                {
                    "version": "1.0.0",
                    "package": {
                        "name": "org.example:shared-runtime",
                        "ecosystem": "Maven",
                    },
                },
                {
                    "version": "2.0.0",
                    "package": {
                        "name": "org.example:shared-runtime",
                        "ecosystem": "Maven",
                    },
                },
            ],
        )

    def test_runtime_configurations_cover_both_build_types(self):
        self.assertEqual(
            MODULE.RUNTIME_CONFIGURATIONS,
            (
                "debugRuntimeClasspath",
                "releaseRuntimeClasspath",
            ),
        )


if __name__ == "__main__":
    unittest.main()
