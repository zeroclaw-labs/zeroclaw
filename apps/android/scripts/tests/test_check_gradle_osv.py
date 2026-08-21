import importlib.util
import unittest
from pathlib import Path

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
                "com.squareup.okhttp3:okhttp": "4.12.0",
                "com.squareup.okio:okio": "3.6.0",
                "org.apache.sshd:sshd-core": "2.19.0",
            },
        )

    def test_parse_components_ignores_non_dependency_lines(self):
        self.assertEqual(
            MODULE.parse_components("BUILD SUCCESSFUL\nNo dependencies"),
            {},
        )


if __name__ == "__main__":
    unittest.main()
