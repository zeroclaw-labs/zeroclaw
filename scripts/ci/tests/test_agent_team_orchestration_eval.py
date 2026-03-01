#!/usr/bin/env python3
"""Tests for scripts/ci/agent_team_orchestration_eval.py."""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "ci" / "agent_team_orchestration_eval.py"


def run_cmd(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(ROOT),
        text=True,
        capture_output=True,
        check=False,
    )


class AgentTeamOrchestrationEvalTest(unittest.TestCase):
    maxDiff = None

    def test_json_output_contains_expected_fields(self) -> None:
        with tempfile.NamedTemporaryFile(suffix=".json") as out:
            proc = run_cmd(
                [
                    "python3",
                    str(SCRIPT),
                    "--budget",
                    "medium",
                    "--json-output",
                    out.name,
                ]
            )
            self.assertEqual(proc.returncode, 0, msg=proc.stderr)

            payload = json.loads(Path(out.name).read_text(encoding="utf-8"))
            self.assertEqual(payload["schema_version"], "zeroclaw.agent-team-eval.v1")
            self.assertEqual(payload["budget_profile"], "medium")
            self.assertIn("results", payload)
            self.assertEqual(len(payload["results"]), 4)
            self.assertIn("recommendation", payload)

            sample = payload["results"][0]
            required_keys = {
                "topology",
                "participants",
                "model_tier",
                "tasks",
                "execution_tokens",
                "coordination_tokens",
                "cache_savings_tokens",
                "total_tokens",
                "coordination_ratio",
                "estimated_pass_rate",
                "estimated_defect_escape",
                "estimated_p95_latency_s",
                "estimated_throughput_tpd",
                "budget_limit_tokens",
                "budget_ok",
                "gates",
                "gate_pass",
            }
            self.assertTrue(required_keys.issubset(sample.keys()))

    def test_coordination_ratio_increases_with_topology_complexity(self) -> None:
        proc = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        payload = json.loads(proc.stdout)

        by_topology = {row["topology"]: row for row in payload["results"]}
        self.assertLess(
            by_topology["single"]["coordination_ratio"],
            by_topology["lead_subagent"]["coordination_ratio"],
        )
        self.assertLess(
            by_topology["lead_subagent"]["coordination_ratio"],
            by_topology["star_team"]["coordination_ratio"],
        )
        self.assertLess(
            by_topology["star_team"]["coordination_ratio"],
            by_topology["mesh_team"]["coordination_ratio"],
        )

    def test_protocol_transcript_costs_more_coordination_tokens(self) -> None:
        base = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--topologies",
                "star_team",
                "--protocol-mode",
                "a2a_lite",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(base.returncode, 0, msg=base.stderr)
        base_payload = json.loads(base.stdout)

        transcript = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--topologies",
                "star_team",
                "--protocol-mode",
                "transcript",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(transcript.returncode, 0, msg=transcript.stderr)
        transcript_payload = json.loads(transcript.stdout)

        base_tokens = base_payload["results"][0]["coordination_tokens"]
        transcript_tokens = transcript_payload["results"][0]["coordination_tokens"]
        self.assertGreater(transcript_tokens, base_tokens)

    def test_auto_degradation_applies_under_pressure(self) -> None:
        no_degrade = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--topologies",
                "mesh_team",
                "--degradation-policy",
                "none",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(no_degrade.returncode, 0, msg=no_degrade.stderr)
        no_degrade_payload = json.loads(no_degrade.stdout)
        no_degrade_row = no_degrade_payload["results"][0]

        auto_degrade = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--topologies",
                "mesh_team",
                "--degradation-policy",
                "auto",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(auto_degrade.returncode, 0, msg=auto_degrade.stderr)
        auto_payload = json.loads(auto_degrade.stdout)
        auto_row = auto_payload["results"][0]

        self.assertTrue(auto_row["degradation_applied"])
        self.assertLess(auto_row["participants"], no_degrade_row["participants"])
        self.assertLess(auto_row["coordination_tokens"], no_degrade_row["coordination_tokens"])

    def test_all_budgets_emits_budget_sweep(self) -> None:
        proc = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--all-budgets",
                "--topologies",
                "single,star_team",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        payload = json.loads(proc.stdout)
        self.assertIn("budget_sweep", payload)
        self.assertEqual(len(payload["budget_sweep"]), 3)
        budgets = [x["budget_profile"] for x in payload["budget_sweep"]]
        self.assertEqual(budgets, ["low", "medium", "high"])

    def test_gate_fails_for_mesh_under_default_threshold(self) -> None:
        proc = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--topologies",
                "mesh_team",
                "--enforce-gates",
                "--max-coordination-ratio",
                "0.20",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("gate violations detected", proc.stderr)
        self.assertIn("mesh_team", proc.stderr)

    def test_gate_passes_for_star_under_default_threshold(self) -> None:
        proc = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--topologies",
                "star_team",
                "--enforce-gates",
                "--max-coordination-ratio",
                "0.20",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)

    def test_recommendation_prefers_star_for_medium_defaults(self) -> None:
        proc = run_cmd(
            [
                "python3",
                str(SCRIPT),
                "--budget",
                "medium",
                "--json-output",
                "-",
            ]
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        payload = json.loads(proc.stdout)
        self.assertEqual(payload["recommendation"]["recommended_topology"], "star_team")


if __name__ == "__main__":
    unittest.main()
