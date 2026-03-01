#!/usr/bin/env python3
"""Estimate coordination efficiency across agent-team topologies.

This script remains intentionally lightweight so it can run in local and CI
contexts without external dependencies. It supports:

- topology comparison (`single`, `lead_subagent`, `star_team`, `mesh_team`)
- budget-aware simulation (`low`, `medium`, `high`)
- workload and protocol profiles
- optional degradation policies under budget pressure
- gate enforcement and recommendation output
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from typing import Iterable


TOPOLOGIES = ("single", "lead_subagent", "star_team", "mesh_team")
RECOMMENDATION_MODES = ("balanced", "cost", "quality")
DEGRADATION_POLICIES = ("none", "auto", "aggressive")


@dataclass(frozen=True)
class BudgetProfile:
    name: str
    summary_cap_tokens: int
    max_workers: int
    compaction_interval_rounds: int
    message_budget_per_task: int
    quality_modifier: float


@dataclass(frozen=True)
class WorkloadProfile:
    name: str
    execution_multiplier: float
    sync_multiplier: float
    summary_multiplier: float
    latency_multiplier: float
    quality_modifier: float


@dataclass(frozen=True)
class ProtocolProfile:
    name: str
    summary_multiplier: float
    artifact_discount: float
    latency_penalty_per_message_s: float
    cache_bonus: float
    quality_modifier: float


BUDGETS: dict[str, BudgetProfile] = {
    "low": BudgetProfile(
        name="low",
        summary_cap_tokens=80,
        max_workers=3,
        compaction_interval_rounds=3,
        message_budget_per_task=10,
        quality_modifier=-0.03,
    ),
    "medium": BudgetProfile(
        name="medium",
        summary_cap_tokens=120,
        max_workers=5,
        compaction_interval_rounds=5,
        message_budget_per_task=20,
        quality_modifier=0.0,
    ),
    "high": BudgetProfile(
        name="high",
        summary_cap_tokens=180,
        max_workers=8,
        compaction_interval_rounds=8,
        message_budget_per_task=32,
        quality_modifier=0.02,
    ),
}


WORKLOADS: dict[str, WorkloadProfile] = {
    "implementation": WorkloadProfile(
        name="implementation",
        execution_multiplier=1.00,
        sync_multiplier=1.00,
        summary_multiplier=1.00,
        latency_multiplier=1.00,
        quality_modifier=0.00,
    ),
    "debugging": WorkloadProfile(
        name="debugging",
        execution_multiplier=1.12,
        sync_multiplier=1.25,
        summary_multiplier=1.12,
        latency_multiplier=1.18,
        quality_modifier=-0.02,
    ),
    "research": WorkloadProfile(
        name="research",
        execution_multiplier=0.95,
        sync_multiplier=0.90,
        summary_multiplier=0.95,
        latency_multiplier=0.92,
        quality_modifier=0.01,
    ),
    "mixed": WorkloadProfile(
        name="mixed",
        execution_multiplier=1.03,
        sync_multiplier=1.08,
        summary_multiplier=1.05,
        latency_multiplier=1.06,
        quality_modifier=0.00,
    ),
}


PROTOCOLS: dict[str, ProtocolProfile] = {
    "a2a_lite": ProtocolProfile(
        name="a2a_lite",
        summary_multiplier=1.00,
        artifact_discount=0.18,
        latency_penalty_per_message_s=0.00,
        cache_bonus=0.02,
        quality_modifier=0.01,
    ),
    "transcript": ProtocolProfile(
        name="transcript",
        summary_multiplier=2.20,
        artifact_discount=0.00,
        latency_penalty_per_message_s=0.012,
        cache_bonus=-0.01,
        quality_modifier=-0.02,
    ),
}


def _participants(topology: str, budget: BudgetProfile) -> int:
    if topology == "single":
        return 1
    if topology == "lead_subagent":
        return 2
    if topology in ("star_team", "mesh_team"):
        return min(5, budget.max_workers)
    raise ValueError(f"unknown topology: {topology}")


def _execution_factor(topology: str) -> float:
    factors = {
        "single": 1.00,
        "lead_subagent": 0.95,
        "star_team": 0.92,
        "mesh_team": 0.97,
    }
    return factors[topology]


def _base_pass_rate(topology: str) -> float:
    rates = {
        "single": 0.78,
        "lead_subagent": 0.84,
        "star_team": 0.88,
        "mesh_team": 0.82,
    }
    return rates[topology]


def _cache_factor(topology: str) -> float:
    factors = {
        "single": 0.05,
        "lead_subagent": 0.08,
        "star_team": 0.10,
        "mesh_team": 0.10,
    }
    return factors[topology]


def _coordination_messages(
    *,
    topology: str,
    rounds: int,
    participants: int,
    workload: WorkloadProfile,
) -> int:
    if topology == "single":
        return 0

    workers = max(1, participants - 1)
    lead_messages = 2 * workers * rounds

    if topology == "lead_subagent":
        base_messages = lead_messages
    elif topology == "star_team":
        broadcast = workers * rounds
        base_messages = lead_messages + broadcast
    elif topology == "mesh_team":
        peer_messages = workers * max(0, workers - 1) * rounds
        base_messages = lead_messages + peer_messages
    else:
        raise ValueError(f"unknown topology: {topology}")

    return int(round(base_messages * workload.sync_multiplier))


def _compute_result(
    *,
    topology: str,
    tasks: int,
    avg_task_tokens: int,
    rounds: int,
    budget: BudgetProfile,
    workload: WorkloadProfile,
    protocol: ProtocolProfile,
    participants_override: int | None = None,
    summary_scale: float = 1.0,
    extra_quality_modifier: float = 0.0,
    model_tier: str = "primary",
    degradation_applied: bool = False,
    degradation_actions: list[str] | None = None,
) -> dict[str, object]:
    participants = participants_override or _participants(topology, budget)
    participants = max(1, participants)
    parallelism = 1 if topology == "single" else max(1, participants - 1)

    execution_tokens = int(
        tasks
        * avg_task_tokens
        * _execution_factor(topology)
        * workload.execution_multiplier
    )

    summary_tokens = min(
        budget.summary_cap_tokens,
        max(24, int(avg_task_tokens * 0.08)),
    )
    summary_tokens = int(summary_tokens * workload.summary_multiplier * protocol.summary_multiplier)
    summary_tokens = max(16, int(summary_tokens * summary_scale))

    messages = _coordination_messages(
        topology=topology,
        rounds=rounds,
        participants=participants,
        workload=workload,
    )
    raw_coordination_tokens = messages * summary_tokens

    compaction_events = rounds // budget.compaction_interval_rounds
    compaction_discount = min(0.35, compaction_events * 0.10)
    coordination_tokens = int(raw_coordination_tokens * (1.0 - compaction_discount))
    coordination_tokens = int(coordination_tokens * (1.0 - protocol.artifact_discount))

    cache_factor = _cache_factor(topology) + protocol.cache_bonus
    cache_factor = min(0.30, max(0.0, cache_factor))
    cache_savings_tokens = int(execution_tokens * cache_factor)

    total_tokens = max(1, execution_tokens + coordination_tokens - cache_savings_tokens)
    coordination_ratio = coordination_tokens / total_tokens

    pass_rate = (
        _base_pass_rate(topology)
        + budget.quality_modifier
        + workload.quality_modifier
        + protocol.quality_modifier
        + extra_quality_modifier
    )
    pass_rate = min(0.99, max(0.0, pass_rate))
    defect_escape = round(max(0.0, 1.0 - pass_rate), 4)

    base_latency_s = (tasks / parallelism) * 6.0 * workload.latency_multiplier
    sync_penalty_s = messages * (0.02 + protocol.latency_penalty_per_message_s)
    p95_latency_s = round(base_latency_s + sync_penalty_s, 2)

    throughput_tpd = round((tasks / max(1.0, p95_latency_s)) * 86400.0, 2)

    budget_limit_tokens = tasks * avg_task_tokens + tasks * budget.message_budget_per_task
    budget_ok = total_tokens <= budget_limit_tokens

    return {
        "topology": topology,
        "participants": participants,
        "model_tier": model_tier,
        "tasks": tasks,
        "tasks_per_worker": round(tasks / parallelism, 2),
        "workload_profile": workload.name,
        "protocol_mode": protocol.name,
        "degradation_applied": degradation_applied,
        "degradation_actions": degradation_actions or [],
        "execution_tokens": execution_tokens,
        "coordination_tokens": coordination_tokens,
        "cache_savings_tokens": cache_savings_tokens,
        "total_tokens": total_tokens,
        "coordination_ratio": round(coordination_ratio, 4),
        "estimated_pass_rate": round(pass_rate, 4),
        "estimated_defect_escape": defect_escape,
        "estimated_p95_latency_s": p95_latency_s,
        "estimated_throughput_tpd": throughput_tpd,
        "budget_limit_tokens": budget_limit_tokens,
        "budget_headroom_tokens": budget_limit_tokens - total_tokens,
        "budget_ok": budget_ok,
    }


def evaluate_topology(
    *,
    topology: str,
    tasks: int,
    avg_task_tokens: int,
    rounds: int,
    budget: BudgetProfile,
    workload: WorkloadProfile,
    protocol: ProtocolProfile,
    degradation_policy: str,
    coordination_ratio_hint: float,
) -> dict[str, object]:
    base = _compute_result(
        topology=topology,
        tasks=tasks,
        avg_task_tokens=avg_task_tokens,
        rounds=rounds,
        budget=budget,
        workload=workload,
        protocol=protocol,
    )

    if degradation_policy == "none" or topology == "single":
        return base

    pressure = (not bool(base["budget_ok"])) or (
        float(base["coordination_ratio"]) > coordination_ratio_hint
    )
    if not pressure:
        return base

    if degradation_policy == "auto":
        participant_delta = 1
        summary_scale = 0.82
        quality_penalty = -0.01
        model_tier = "economy"
    elif degradation_policy == "aggressive":
        participant_delta = 2
        summary_scale = 0.65
        quality_penalty = -0.03
        model_tier = "economy"
    else:
        raise ValueError(f"unknown degradation policy: {degradation_policy}")

    reduced = max(2, int(base["participants"]) - participant_delta)
    actions = [
        f"reduce_participants:{base['participants']}->{reduced}",
        f"tighten_summary_scale:{summary_scale}",
        f"switch_model_tier:{model_tier}",
    ]

    return _compute_result(
        topology=topology,
        tasks=tasks,
        avg_task_tokens=avg_task_tokens,
        rounds=rounds,
        budget=budget,
        workload=workload,
        protocol=protocol,
        participants_override=reduced,
        summary_scale=summary_scale,
        extra_quality_modifier=quality_penalty,
        model_tier=model_tier,
        degradation_applied=True,
        degradation_actions=actions,
    )


def parse_topologies(raw: str) -> list[str]:
    items = [x.strip() for x in raw.split(",") if x.strip()]
    invalid = sorted(set(items) - set(TOPOLOGIES))
    if invalid:
        raise ValueError(f"invalid topologies: {', '.join(invalid)}")
    if not items:
        raise ValueError("topology list is empty")
    return items


def _emit_json(path: str, payload: dict[str, object]) -> None:
    content = json.dumps(payload, indent=2, sort_keys=False)
    if path == "-":
        print(content)
        return

    with open(path, "w", encoding="utf-8") as f:
        f.write(content)
        f.write("\n")


def _rank(results: Iterable[dict[str, object]], key: str) -> list[str]:
    return [x["topology"] for x in sorted(results, key=lambda row: row[key])]  # type: ignore[index]


def _score_recommendation(
    *,
    results: list[dict[str, object]],
    mode: str,
) -> dict[str, object]:
    if not results:
        return {
            "mode": mode,
            "recommended_topology": None,
            "reason": "no_results",
            "scores": [],
        }

    max_tokens = max(int(row["total_tokens"]) for row in results)
    max_latency = max(float(row["estimated_p95_latency_s"]) for row in results)

    if mode == "balanced":
        w_quality, w_cost, w_latency = 0.45, 0.35, 0.20
    elif mode == "cost":
        w_quality, w_cost, w_latency = 0.25, 0.55, 0.20
    elif mode == "quality":
        w_quality, w_cost, w_latency = 0.65, 0.20, 0.15
    else:
        raise ValueError(f"unknown recommendation mode: {mode}")

    scored: list[dict[str, object]] = []
    for row in results:
        quality = float(row["estimated_pass_rate"])
        cost_norm = 1.0 - (int(row["total_tokens"]) / max(1, max_tokens))
        latency_norm = 1.0 - (float(row["estimated_p95_latency_s"]) / max(1.0, max_latency))
        score = (quality * w_quality) + (cost_norm * w_cost) + (latency_norm * w_latency)
        scored.append(
            {
                "topology": row["topology"],
                "score": round(score, 5),
                "gate_pass": row["gate_pass"],
            }
        )

    scored.sort(key=lambda x: float(x["score"]), reverse=True)
    return {
        "mode": mode,
        "recommended_topology": scored[0]["topology"],
        "reason": "weighted_score",
        "scores": scored,
    }


def _apply_gates(
    *,
    row: dict[str, object],
    max_coordination_ratio: float,
    min_pass_rate: float,
    max_p95_latency: float,
) -> dict[str, object]:
    coord_ok = float(row["coordination_ratio"]) <= max_coordination_ratio
    quality_ok = float(row["estimated_pass_rate"]) >= min_pass_rate
    latency_ok = float(row["estimated_p95_latency_s"]) <= max_p95_latency
    budget_ok = bool(row["budget_ok"])

    row["gates"] = {
        "coordination_ratio_ok": coord_ok,
        "quality_ok": quality_ok,
        "latency_ok": latency_ok,
        "budget_ok": budget_ok,
    }
    row["gate_pass"] = coord_ok and quality_ok and latency_ok and budget_ok
    return row


def _evaluate_budget(
    *,
    budget: BudgetProfile,
    args: argparse.Namespace,
    topologies: list[str],
    workload: WorkloadProfile,
    protocol: ProtocolProfile,
) -> dict[str, object]:
    rows = [
        evaluate_topology(
            topology=t,
            tasks=args.tasks,
            avg_task_tokens=args.avg_task_tokens,
            rounds=args.coordination_rounds,
            budget=budget,
            workload=workload,
            protocol=protocol,
            degradation_policy=args.degradation_policy,
            coordination_ratio_hint=args.max_coordination_ratio,
        )
        for t in topologies
    ]

    rows = [
        _apply_gates(
            row=r,
            max_coordination_ratio=args.max_coordination_ratio,
            min_pass_rate=args.min_pass_rate,
            max_p95_latency=args.max_p95_latency,
        )
        for r in rows
    ]

    gate_pass_rows = [r for r in rows if bool(r["gate_pass"])]

    recommendation_pool = gate_pass_rows if gate_pass_rows else rows
    recommendation = _score_recommendation(
        results=recommendation_pool,
        mode=args.recommendation_mode,
    )
    recommendation["used_gate_filtered_pool"] = bool(gate_pass_rows)

    return {
        "budget_profile": budget.name,
        "results": rows,
        "rankings": {
            "cost_asc": _rank(rows, "total_tokens"),
            "coordination_ratio_asc": _rank(rows, "coordination_ratio"),
            "latency_asc": _rank(rows, "estimated_p95_latency_s"),
            "pass_rate_desc": [
                x["topology"]
                for x in sorted(rows, key=lambda row: row["estimated_pass_rate"], reverse=True)
            ],
        },
        "recommendation": recommendation,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--budget", choices=sorted(BUDGETS.keys()), default="medium")
    parser.add_argument("--all-budgets", action="store_true")
    parser.add_argument("--tasks", type=int, default=24)
    parser.add_argument("--avg-task-tokens", type=int, default=1400)
    parser.add_argument("--coordination-rounds", type=int, default=4)
    parser.add_argument(
        "--topologies",
        default=",".join(TOPOLOGIES),
        help=f"comma-separated list: {','.join(TOPOLOGIES)}",
    )
    parser.add_argument("--workload-profile", choices=sorted(WORKLOADS.keys()), default="mixed")
    parser.add_argument("--protocol-mode", choices=sorted(PROTOCOLS.keys()), default="a2a_lite")
    parser.add_argument(
        "--degradation-policy",
        choices=DEGRADATION_POLICIES,
        default="none",
    )
    parser.add_argument(
        "--recommendation-mode",
        choices=RECOMMENDATION_MODES,
        default="balanced",
    )
    parser.add_argument("--max-coordination-ratio", type=float, default=0.20)
    parser.add_argument("--min-pass-rate", type=float, default=0.80)
    parser.add_argument("--max-p95-latency", type=float, default=180.0)
    parser.add_argument("--json-output", default="-")
    parser.add_argument("--enforce-gates", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.tasks <= 0:
        parser.error("--tasks must be > 0")
    if args.avg_task_tokens <= 0:
        parser.error("--avg-task-tokens must be > 0")
    if args.coordination_rounds < 0:
        parser.error("--coordination-rounds must be >= 0")
    if not (0.0 < args.max_coordination_ratio < 1.0):
        parser.error("--max-coordination-ratio must be in (0, 1)")
    if not (0.0 < args.min_pass_rate <= 1.0):
        parser.error("--min-pass-rate must be in (0, 1]")
    if args.max_p95_latency <= 0.0:
        parser.error("--max-p95-latency must be > 0")

    try:
        topologies = parse_topologies(args.topologies)
    except ValueError as exc:
        parser.error(str(exc))

    workload = WORKLOADS[args.workload_profile]
    protocol = PROTOCOLS[args.protocol_mode]

    budget_targets = list(BUDGETS.values()) if args.all_budgets else [BUDGETS[args.budget]]

    budget_reports = [
        _evaluate_budget(
            budget=budget,
            args=args,
            topologies=topologies,
            workload=workload,
            protocol=protocol,
        )
        for budget in budget_targets
    ]

    primary = budget_reports[0]
    payload: dict[str, object] = {
        "schema_version": "zeroclaw.agent-team-eval.v1",
        "budget_profile": primary["budget_profile"],
        "inputs": {
            "tasks": args.tasks,
            "avg_task_tokens": args.avg_task_tokens,
            "coordination_rounds": args.coordination_rounds,
            "topologies": topologies,
            "workload_profile": args.workload_profile,
            "protocol_mode": args.protocol_mode,
            "degradation_policy": args.degradation_policy,
            "recommendation_mode": args.recommendation_mode,
            "max_coordination_ratio": args.max_coordination_ratio,
            "min_pass_rate": args.min_pass_rate,
            "max_p95_latency": args.max_p95_latency,
        },
        "results": primary["results"],
        "rankings": primary["rankings"],
        "recommendation": primary["recommendation"],
    }

    if args.all_budgets:
        payload["budget_sweep"] = budget_reports

    _emit_json(args.json_output, payload)

    if not args.enforce_gates:
        return 0

    violations: list[str] = []
    for report in budget_reports:
        budget_name = report["budget_profile"]
        for row in report["results"]:  # type: ignore[index]
            if bool(row["gate_pass"]):
                continue
            gates = row["gates"]
            if not gates["coordination_ratio_ok"]:
                violations.append(
                    f"{budget_name}:{row['topology']}: coordination_ratio={row['coordination_ratio']}"
                )
            if not gates["quality_ok"]:
                violations.append(
                    f"{budget_name}:{row['topology']}: pass_rate={row['estimated_pass_rate']}"
                )
            if not gates["latency_ok"]:
                violations.append(
                    f"{budget_name}:{row['topology']}: p95_latency_s={row['estimated_p95_latency_s']}"
                )
            if not gates["budget_ok"]:
                violations.append(f"{budget_name}:{row['topology']}: exceeded budget_limit_tokens")

    if violations:
        print("gate violations detected:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
