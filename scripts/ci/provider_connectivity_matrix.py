#!/usr/bin/env python3
"""Build provider/model connectivity matrix for CI and local inspection.

The script runs `zeroclaw doctor models --provider <id>` against a contract-defined
provider set, classifies failures, applies noise-control policy, and emits:
- machine-readable JSON report
- markdown summary (also appended to GITHUB_STEP_SUMMARY when available)
- optional raw log for deep triage

Exit code is non-zero only when policy says the run should gate (unless --report-only).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Any


AUTH_HINTS = [
    "401",
    "403",
    "unauthorized",
    "forbidden",
    "invalid api key",
    "requires api key",
    "api key",
    "token",
    "insufficient quota",
    "insufficient balance",
    "permission denied",
]

RATE_LIMIT_HINTS = [
    "429",
    "rate limit",
    "too many requests",
]

NETWORK_HINTS = [
    "timed out",
    "timeout",
    "network",
    "connection refused",
    "connection reset",
    "dns",
    "temporary failure in name resolution",
    "failed to connect",
    "tls",
    "certificate",
    "could not resolve host",
    "operation timed out",
]

UNAVAILABLE_HINTS = [
    "404",
    "not found",
    "service unavailable",
    "provider returned an empty model list",
    "does not support live model discovery",
    "unsupported",
]

MODEL_COUNT_PATTERNS = [
    re.compile(r"Refreshed '\\S+' model cache with (\\d+) models", re.IGNORECASE),
    re.compile(r"with (\\d+) models", re.IGNORECASE),
]


@dataclass
class ProviderContract:
    name: str
    provider: str
    required: bool
    secret_env: str | None
    timeout_sec: int
    retries: int
    notes: str


@dataclass
class ProviderResult:
    name: str
    provider: str
    required: bool
    secret_env: str | None
    status: str
    category: str
    gate: bool
    attempts: int
    timeout_sec: int
    retries: int
    message: str
    model_count: int | None
    started_at: str
    ended_at: str
    duration_ms: int
    notes: str


def utc_now() -> str:
    return dt.datetime.now(tz=dt.timezone.utc).isoformat(timespec="seconds")


def clip(text: str, max_chars: int = 280) -> str:
    clean = " ".join(text.strip().split())
    if len(clean) <= max_chars:
        return clean
    return clean[: max_chars - 3] + "..."


def classify_failure(raw: str) -> str:
    lower = raw.lower()

    if any(hint in lower for hint in RATE_LIMIT_HINTS):
        return "rate_limit"
    if any(hint in lower for hint in AUTH_HINTS):
        return "auth"
    if any(hint in lower for hint in NETWORK_HINTS):
        return "network"
    if any(hint in lower for hint in UNAVAILABLE_HINTS):
        return "unavailable"
    return "other"


def parse_model_count(output: str) -> int | None:
    for pattern in MODEL_COUNT_PATTERNS:
        m = pattern.search(output)
        if m:
            try:
                return int(m.group(1))
            except (TypeError, ValueError):
                return None
    return None


def load_contract(path: pathlib.Path) -> tuple[int, int, list[ProviderContract]]:
    raw = json.loads(path.read_text(encoding="utf-8"))

    version = int(raw.get("version", 1))
    threshold = int(raw.get("consecutive_transient_failures_to_escalate", 2))
    providers_raw = raw.get("providers", [])
    if not isinstance(providers_raw, list) or not providers_raw:
        raise ValueError("contract.providers must be a non-empty list")

    providers: list[ProviderContract] = []
    for item in providers_raw:
        if not isinstance(item, dict):
            raise ValueError("contract.providers entries must be objects")

        name = str(item.get("name", "")).strip()
        provider = str(item.get("provider", "")).strip()
        if not name or not provider:
            raise ValueError("provider entry requires non-empty 'name' and 'provider'")

        timeout_sec = int(item.get("timeout_sec", 90))
        retries = int(item.get("retries", 2))
        providers.append(
            ProviderContract(
                name=name,
                provider=provider,
                required=bool(item.get("required", False)),
                secret_env=str(item["secret_env"]).strip() if item.get("secret_env") else None,
                timeout_sec=max(10, timeout_sec),
                retries=max(1, retries),
                notes=str(item.get("notes", "")).strip(),
            )
        )

    return version, max(1, threshold), providers


def load_state(path: pathlib.Path) -> dict[str, Any]:
    if not path.exists():
        return {"providers": {}}

    try:
        parsed = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {"providers": {}}

    providers = parsed.get("providers")
    if not isinstance(providers, dict):
        providers = {}
    return {"providers": providers}


def save_state(path: pathlib.Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")


def run_probe(binary: str, contract: ProviderContract) -> tuple[bool, str, int | None, int, str]:
    """Return (ok, category, model_count, attempts_used, message)."""
    env = os.environ.copy()

    if contract.secret_env:
        value = env.get(contract.secret_env, "").strip()
        if value:
            env[contract.secret_env] = value

    command = [binary, "doctor", "models", "--provider", contract.provider]

    last_message = ""
    last_category = "other"
    last_model_count: int | None = None

    for attempt in range(1, contract.retries + 1):
        try:
            proc = subprocess.run(
                command,
                env=env,
                capture_output=True,
                text=True,
                timeout=contract.timeout_sec,
                check=False,
            )
            combined = "\n".join([proc.stdout or "", proc.stderr or ""]).strip()
            if proc.returncode == 0:
                return (
                    True,
                    "ok",
                    parse_model_count(combined),
                    attempt,
                    clip(combined, 360),
                )

            last_message = clip(combined or f"command exited with code {proc.returncode}", 360)
            last_category = classify_failure(combined)
            last_model_count = parse_model_count(combined)

            # Only retry transient classes.
            if last_category not in {"network", "rate_limit"}:
                return False, last_category, last_model_count, attempt, last_message

            if attempt < contract.retries:
                time.sleep(min(5, attempt))

        except subprocess.TimeoutExpired:
            last_message = (
                f"probe timed out after {contract.timeout_sec}s for provider {contract.provider}"
            )
            last_category = "network"
            last_model_count = None

            if attempt < contract.retries:
                time.sleep(min(5, attempt))

    return False, last_category, last_model_count, contract.retries, last_message


def build_markdown(
    report: dict[str, Any],
    binary: str,
    contract_path: pathlib.Path,
    report_only: bool,
) -> str:
    lines: list[str] = []
    lines.append("## Provider Connectivity Matrix")
    lines.append("")
    lines.append(f"- Generated: `{report['generated_at']}`")
    lines.append(f"- Contract: `{contract_path}` (v{report['contract_version']})")
    lines.append(f"- Probe binary: `{binary}`")
    lines.append(f"- Mode: `{'report-only' if report_only else 'enforced'}`")
    lines.append(
        "- Summary: "
        f"{report['summary']['ok']} ok, "
        f"{report['summary']['failed']} failed, "
        f"{report['summary']['skipped']} skipped"
    )
    lines.append(
        "- Categories: "
        f"auth={report['summary']['categories']['auth']}, "
        f"network={report['summary']['categories']['network']}, "
        f"unavailable={report['summary']['categories']['unavailable']}, "
        f"rate_limit={report['summary']['categories']['rate_limit']}, "
        f"other={report['summary']['categories']['other']}"
    )
    lines.append("")
    lines.append("| Provider | Required | Status | Category | Gate | Models | Attempts | Detail |")
    lines.append("| --- | --- | --- | --- | --- | ---: | ---: | --- |")

    for item in report["providers"]:
        models = "-" if item["model_count"] is None else str(item["model_count"])
        lines.append(
            "| "
            f"{item['name']} (`{item['provider']}`)"
            " | "
            f"{'yes' if item['required'] else 'no'}"
            " | "
            f"{item['status']}"
            " | "
            f"{item['category']}"
            " | "
            f"{'yes' if item['gate'] else 'no'}"
            " | "
            f"{models}"
            " | "
            f"{item['attempts']}"
            " | "
            f"{item['message']}"
            " |"
        )

    lines.append("")
    lines.append("### Local Inspection")
    lines.append("")
    lines.append("```bash")
    lines.append(
        "python3 scripts/ci/provider_connectivity_matrix.py "
        "--binary target/release-fast/zeroclaw "
        "--contract .github/connectivity/probe-contract.json "
        "--output-json connectivity-report.json "
        "--output-markdown connectivity-summary.md"
    )
    lines.append("```")

    return "\n".join(lines).strip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--contract",
        default=".github/connectivity/probe-contract.json",
        help="Path to connectivity probe contract JSON",
    )
    parser.add_argument(
        "--binary",
        default="zeroclaw",
        help="Path to zeroclaw binary used for probes",
    )
    parser.add_argument(
        "--state-file",
        default=".ci/connectivity-state.json",
        help="State file for transient-failure tracking",
    )
    parser.add_argument(
        "--output-json",
        default="connectivity-report.json",
        help="Output JSON report path",
    )
    parser.add_argument(
        "--output-markdown",
        default="connectivity-summary.md",
        help="Output markdown summary path",
    )
    parser.add_argument(
        "--raw-log",
        default=".ci/connectivity-raw.log",
        help="Output raw probe log path",
    )
    parser.add_argument(
        "--report-only",
        action="store_true",
        help="Never fail the process, even if gate conditions are hit",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate contract and emit empty report without running probes",
    )
    args = parser.parse_args()

    contract_path = pathlib.Path(args.contract)
    output_json_path = pathlib.Path(args.output_json)
    output_markdown_path = pathlib.Path(args.output_markdown)
    state_path = pathlib.Path(args.state_file)
    raw_log_path = pathlib.Path(args.raw_log)

    if not contract_path.exists():
        print(f"contract file not found: {contract_path}", file=sys.stderr)
        return 2

    try:
        contract_version, threshold, providers = load_contract(contract_path)
    except Exception as exc:
        print(f"invalid contract: {exc}", file=sys.stderr)
        return 2

    if not args.dry_run and not pathlib.Path(args.binary).exists() and "/" in args.binary:
        print(f"probe binary not found: {args.binary}", file=sys.stderr)
        return 2

    previous_state = load_state(state_path)
    current_state: dict[str, Any] = {"providers": {}}

    generated_at = utc_now()
    results: list[ProviderResult] = []
    raw_lines: list[str] = [
        f"# Connectivity probe raw log\n",
        f"generated_at={generated_at}\n",
        f"contract={contract_path}\n",
        f"binary={args.binary}\n",
        f"report_only={args.report_only}\n",
        f"threshold={threshold}\n",
        "\n",
    ]

    if args.dry_run:
        for contract in providers:
            now = utc_now()
            results.append(
                ProviderResult(
                    name=contract.name,
                    provider=contract.provider,
                    required=contract.required,
                    secret_env=contract.secret_env,
                    status="dry_run",
                    category="other",
                    gate=False,
                    attempts=0,
                    timeout_sec=contract.timeout_sec,
                    retries=contract.retries,
                    message="dry-run: probe skipped",
                    model_count=None,
                    started_at=now,
                    ended_at=now,
                    duration_ms=0,
                    notes=contract.notes,
                )
            )
    else:
        for contract in providers:
            started = time.perf_counter()
            started_at = utc_now()

            secret_value = ""
            if contract.secret_env:
                secret_value = os.environ.get(contract.secret_env, "").strip()

            if contract.secret_env and not secret_value:
                category = "auth"
                status = "missing_secret_required" if contract.required else "skipped_missing_secret"
                gate = contract.required
                ended_at = utc_now()
                duration_ms = int((time.perf_counter() - started) * 1000)
                message = f"missing secret env: {contract.secret_env}"
                attempts = 0
                model_count = None
            else:
                ok, category, model_count, attempts, message = run_probe(args.binary, contract)
                status = "ok" if ok else "failed"

                prev = previous_state["providers"].get(contract.provider, {})
                prev_transient = int(prev.get("consecutive_transient_failures", 0))

                if ok:
                    transient = 0
                elif category in {"network", "rate_limit"}:
                    transient = prev_transient + 1
                else:
                    transient = 0

                immediate_gate = category in {"auth", "unavailable", "other"}
                transient_gate = category in {"network", "rate_limit"} and transient >= threshold
                gate = contract.required and (immediate_gate or transient_gate)

                current_state["providers"][contract.provider] = {
                    "name": contract.name,
                    "last_status": status,
                    "last_category": category,
                    "last_message": message,
                    "last_checked_at": utc_now(),
                    "consecutive_transient_failures": transient,
                }

                ended_at = utc_now()
                duration_ms = int((time.perf_counter() - started) * 1000)

            if contract.provider not in current_state["providers"]:
                current_state["providers"][contract.provider] = {
                    "name": contract.name,
                    "last_status": status,
                    "last_category": category,
                    "last_message": message,
                    "last_checked_at": utc_now(),
                    "consecutive_transient_failures": 0,
                }

            result = ProviderResult(
                name=contract.name,
                provider=contract.provider,
                required=contract.required,
                secret_env=contract.secret_env,
                status=status,
                category=category,
                gate=gate,
                attempts=attempts,
                timeout_sec=contract.timeout_sec,
                retries=contract.retries,
                message=clip(message),
                model_count=model_count,
                started_at=started_at,
                ended_at=ended_at,
                duration_ms=duration_ms,
                notes=contract.notes,
            )
            results.append(result)

            raw_lines.append(
                f"[{result.ended_at}] provider={result.provider} status={result.status} "
                f"category={result.category} gate={result.gate} attempts={result.attempts} "
                f"duration_ms={result.duration_ms} message={result.message}\n"
            )

    summary = {
        "ok": sum(1 for r in results if r.status == "ok"),
        "failed": sum(1 for r in results if r.status in {"failed", "missing_secret_required"}),
        "skipped": sum(
            1
            for r in results
            if r.status in {"skipped_missing_secret", "dry_run"}
        ),
        "gate_failures": sum(1 for r in results if r.gate),
        "categories": {
            "auth": sum(1 for r in results if r.category == "auth"),
            "network": sum(1 for r in results if r.category == "network"),
            "unavailable": sum(1 for r in results if r.category == "unavailable"),
            "rate_limit": sum(1 for r in results if r.category == "rate_limit"),
            "other": sum(1 for r in results if r.category == "other"),
        },
    }

    report = {
        "generated_at": generated_at,
        "contract_version": contract_version,
        "consecutive_transient_failures_to_escalate": threshold,
        "report_only": args.report_only,
        "summary": summary,
        "policy": {
            "required_immediate_gate_categories": ["auth", "unavailable", "other"],
            "required_transient_gate_categories": ["network", "rate_limit"],
            "required_transient_gate_threshold": threshold,
            "optional_provider_gating": "never",
        },
        "providers": [r.__dict__ for r in results],
    }

    output_json_path.parent.mkdir(parents=True, exist_ok=True)
    output_markdown_path.parent.mkdir(parents=True, exist_ok=True)
    raw_log_path.parent.mkdir(parents=True, exist_ok=True)

    output_json_path.write_text(
        json.dumps(report, indent=2, ensure_ascii=True) + "\n",
        encoding="utf-8",
    )

    markdown = build_markdown(
        report=report,
        binary=args.binary,
        contract_path=contract_path,
        report_only=args.report_only,
    )
    output_markdown_path.write_text(markdown, encoding="utf-8")
    raw_log_path.write_text("".join(raw_lines), encoding="utf-8")

    save_state(state_path, current_state)

    summary_path = os.environ.get("GITHUB_STEP_SUMMARY", "").strip()
    if summary_path:
        with open(summary_path, "a", encoding="utf-8") as fh:
            fh.write("\n")
            fh.write(markdown)

    print(
        f"connectivity matrix complete: ok={summary['ok']} failed={summary['failed']} "
        f"skipped={summary['skipped']} gate_failures={summary['gate_failures']}"
    )
    print(f"report: {output_json_path}")
    print(f"summary: {output_markdown_path}")
    print(f"state: {state_path}")

    if args.report_only or args.dry_run:
        return 0

    return 1 if summary["gate_failures"] > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
