#!/usr/bin/env python3
"""Guard the release attestation workflow's security-sensitive sequencing."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = ROOT / ".github/workflows/release-stable-manual.yml"
WORKFLOW = WORKFLOW_PATH.read_text(encoding="utf-8")


def job_block(job_id: str) -> str:
    match = re.search(
        rf"^  {re.escape(job_id)}:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\n|\Z)",
        WORKFLOW,
        flags=re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise AssertionError(f"job {job_id!r} not found in {WORKFLOW_PATH}")
    return match.group(0)


def step_block(parent: str, step_id: str) -> str:
    match = re.search(
        rf"^      - id: {re.escape(step_id)}\n(?P<body>.*?)(?=^      - |\Z)",
        parent,
        flags=re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise AssertionError(f"step {step_id!r} not found")
    return match.group(0)


class ReleaseAttestationContractTest(unittest.TestCase):
    def setUp(self) -> None:
        self.sbom = job_block("sbom")
        self.publish = job_block("publish")
        self.docker = job_block("docker")

    def test_redundant_release_signing_paths_are_absent(self) -> None:
        forbidden = (
            "slsa-framework/slsa-github-generator",
            "generator_generic_slsa3.yml",
            "cosign sign-blob",
            "hash-artifacts:",
            "sign-and-sbom:",
        )
        for value in forbidden:
            with self.subTest(value=value):
                self.assertNotIn(value, WORKFLOW)

    def test_publish_keeps_required_attestation_permissions(self) -> None:
        self.assertRegex(self.publish, r"(?m)^      id-token: write$")
        self.assertRegex(self.publish, r"(?m)^      attestations: write$")

    def test_sboms_are_read_only_inputs_to_publish(self) -> None:
        self.assertRegex(self.sbom, r"(?m)^      contents: read$")
        self.assertNotIn("id-token: write", self.sbom)
        self.assertNotIn("attestations: write", self.sbom)
        self.assertNotRegex(self.sbom, r"(?m)^      [a-z-]+: write$")
        self.assertEqual(self.sbom.count("format: spdx-json"), 1)
        self.assertEqual(self.sbom.count("format: cyclonedx-json"), 1)
        self.assertIn("name: release-sboms", self.sbom)
        self.assertIn("build-desktop-windows, sbom]", self.publish)
        self.assertIn("name: release-sboms", self.publish)
        sequence = ("Collect release assets", "id: attest_payloads")
        offsets = [self.publish.index(marker) for marker in sequence]
        self.assertEqual(offsets, sorted(offsets))
        self.assertIn("*-sbom.spdx.json", self.publish)
        self.assertIn("*-sbom.cdx.json", self.publish)

    def test_payload_attestation_is_pinned_and_best_effort(self) -> None:
        step = step_block(self.publish, "attest_payloads")
        self.assertRegex(
            step,
            r"actions/attest-build-provenance@[0-9a-f]{40} # v3\.2\.0",
        )
        self.assertIn("continue-on-error: true", step)
        self.assertIn("subject-path: release-assets/*", step)

    def test_archive_is_built_only_from_verified_offline_material(self) -> None:
        step = step_block(self.publish, "verification_archive")
        required = (
            "steps.attest_payloads.outcome == 'success'",
            "gh attestation trusted-root",
            "gh attestation download",
            "gh attestation verify",
            "--bundle",
            "--custom-trusted-root",
            "ATTESTATION-BUNDLES.md",
            "zeroclaw-${TAG}-verification.tar.gz",
            "tar -C",
        )
        for value in required:
            with self.subTest(value=value):
                self.assertIn(value, step)

    def test_final_checksums_follow_archive_and_are_never_rewritten(self) -> None:
        archive = self.publish.index("id: verification_archive")
        checksums = self.publish.index("Generate final checksums")
        checksum_attestation = self.publish.index("id: attest_checksums")
        archive_attestation = self.publish.index("id: attest_verification_archive")
        release = self.publish.index("Create tag and release")
        self.assertLess(archive, checksums)
        self.assertLess(checksums, checksum_attestation)
        self.assertLess(checksum_attestation, archive_attestation)
        self.assertLess(archive_attestation, release)
        self.assertEqual(self.publish.count("> SHA256SUMS"), 1)
        self.assertIn("! -name SHA256SUMS", self.publish)

    def test_final_metadata_has_dedicated_attestations(self) -> None:
        subjects = {
            "attest_checksums": "subject-path: release-assets/SHA256SUMS",
            "attest_verification_archive": (
                "subject-path: release-assets/zeroclaw-${{ needs.validate.outputs.tag }}-verification.tar.gz"
            ),
        }
        for step_id, subject in subjects.items():
            with self.subTest(step_id=step_id):
                step = step_block(self.publish, step_id)
                self.assertIn(subject, step)
                self.assertIn("continue-on-error: true", step)
                self.assertRegex(
                    step,
                    r"actions/attest-build-provenance@[0-9a-f]{40} # v3\.2\.0",
                )

    def test_release_uploads_only_the_consolidated_asset_directory(self) -> None:
        self.assertEqual(self.publish.count('gh release create "$TAG" release-assets/*'), 2)
        self.assertNotIn("gh release upload", WORKFLOW)

    def test_cosign_remains_for_ghcr_images_only(self) -> None:
        self.assertRegex(
            self.docker,
            r"sigstore/cosign-installer@[0-9a-f]{40} # v3\.8\.1",
        )
        self.assertEqual(self.docker.count("cosign sign --yes"), 2)
        self.assertNotIn("cosign", self.publish)


if __name__ == "__main__":
    unittest.main()
