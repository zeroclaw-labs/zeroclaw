"""Offline contract tests for the container's trust-bundle filter."""

import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "dev/ci/filter-emudhra-ca-bundle.sh"
FIXTURES = ROOT / "crates/zeroclaw-tls/tests/fixtures/emudhra"


class BundleFilterTests(unittest.TestCase):
    def run_filter(self, bundle):
        with tempfile.TemporaryDirectory() as directory:
            source = pathlib.Path(directory) / "input.pem"
            target = pathlib.Path(directory) / "output.pem"
            source.write_bytes(bundle)
            result = subprocess.run(
                ["sh", str(SCRIPT), str(source), str(target)],
                capture_output=True,
            )
            output = target.read_bytes() if target.exists() else None
            return result, output

    def test_removes_direct_and_cross_sign_roots_preserving_other_certificates(self):
        # A certificate outside the root denylist must survive byte-for-byte.
        retained = (FIXTURES / "host-invalid-leaf.pem").read_bytes()
        blocked = b"".join(
            (FIXTURES / name).read_bytes()
            for name in (
                "emsign-ecc-g3-root.pem",
                "aaa-root.pem",
                "emsign-tls-g3-root.pem",
                "emsign-cs-g3-root.pem",
            )
        )
        result, output = self.run_filter(blocked + retained)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(output, retained)

    def test_empty_input_does_not_produce_a_bundle(self):
        result, output = self.run_filter(b"")
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(output)

    def test_unterminated_certificate_does_not_produce_a_bundle(self):
        retained = (FIXTURES / "host-invalid-leaf.pem").read_bytes()
        result, output = self.run_filter(
            retained + b"-----BEGIN CERTIFICATE-----\nYQ==\n"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(output)

    def test_invalid_base64_does_not_produce_a_bundle(self):
        result, output = self.run_filter(
            b"-----BEGIN CERTIFICATE-----\n!invalid!\n-----END CERTIFICATE-----\n"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(output)

    def test_removing_every_root_does_not_produce_an_empty_bundle(self):
        result, output = self.run_filter((FIXTURES / "aaa-root.pem").read_bytes())
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(output)


if __name__ == "__main__":
    unittest.main()
