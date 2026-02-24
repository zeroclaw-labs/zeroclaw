#!/usr/bin/env python3
"""Probe provider API endpoints and generate connectivity matrix artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import socket
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def dns_check(hostname: str, port: int) -> tuple[bool, str]:
    try:
        socket.getaddrinfo(hostname, port, type=socket.SOCK_STREAM)
        return (True, "ok")
    except Exception as exc:  # pragma: no cover - operational error surface
        return (False, str(exc))


def http_probe(url: str, method: str, timeout_s: int) -> tuple[bool, int | None, str, int]:
    req = urllib.request.Request(url=url, method=method, headers={"User-Agent": "zeroclaw-ci-probe/1.0"})
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            elapsed_ms = int((time.perf_counter() - start) * 1000)
            code = int(resp.getcode())
            # For connectivity probes, 2xx-4xx indicates endpoint is reachable.
            return (200 <= code < 500, code, "ok", elapsed_ms)
    except urllib.error.HTTPError as exc:
        elapsed_ms = int((time.perf_counter() - start) * 1000)
        code = int(exc.code)
        return (200 <= code < 500, code, f"http_error:{code}", elapsed_ms)
    except Exception as exc:  # pragma: no cover - operational error surface
        elapsed_ms = int((time.perf_counter() - start) * 1000)
        return (False, None, str(exc), elapsed_ms)


def build_markdown(rows: list[dict], timeout_s: int, critical_failures: list[dict]) -> str:
    lines: list[str] = []
    lines.append("# Provider Connectivity Matrix")
    lines.append("")
    lines.append(f"- Generated at: `{dt.datetime.now(dt.timezone.utc).isoformat()}`")
    lines.append(f"- Timeout per endpoint: `{timeout_s}s`")
    lines.append(f"- Total endpoints: `{len(rows)}`")
    lines.append(f"- Reachable endpoints: `{sum(1 for r in rows if r['reachable'])}`")
    lines.append(f"- Critical failures: `{len(critical_failures)}`")
    lines.append("")
    lines.append("| Provider | Endpoint | Critical | DNS | HTTP | Reachable | Latency (ms) | Notes |")
    lines.append("| --- | --- | --- | --- | ---:| --- | ---:| --- |")
    for row in rows:
        lines.append(
            f"| `{row['provider']}` | `{row['url']}` | `{row['critical']}` | `{row['dns_ok']}` | "
            f"`{row['http_status'] if row['http_status'] is not None else 'n/a'}` | "
            f"`{row['reachable']}` | `{row['latency_ms']}` | {row['notes']} |"
        )
    lines.append("")
    if critical_failures:
        lines.append("## Critical Probe Failures")
        for row in critical_failures:
            lines.append(f"- `{row['provider']}` -> `{row['url']}` ({row['notes']})")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate provider connectivity matrix.")
    parser.add_argument(
        "--config",
        default=".github/connectivity/providers.json",
        help="Path to providers connectivity config JSON",
    )
    parser.add_argument("--timeout", type=int, default=8, help="HTTP probe timeout in seconds")
    parser.add_argument("--output-json", required=True, help="Output JSON path")
    parser.add_argument("--output-md", required=True, help="Output markdown path")
    parser.add_argument(
        "--fail-on-critical",
        action="store_true",
        help="Return non-zero if any critical endpoint is unreachable",
    )
    args = parser.parse_args()

    config = json.loads(Path(args.config).read_text(encoding="utf-8"))
    timeout_s = int(config.get("global_timeout_seconds", args.timeout))
    providers = config.get("providers", [])

    rows: list[dict] = []
    for item in providers:
        provider = str(item.get("id", "")).strip()
        url = str(item.get("url", "")).strip()
        if not provider or not url:
            continue

        critical = bool(item.get("critical", False))
        method = str(item.get("method", "HEAD")).upper().strip()
        parsed = urllib.parse.urlparse(url)
        host = parsed.hostname or ""
        port = parsed.port or (443 if parsed.scheme == "https" else 80)

        dns_ok, dns_note = dns_check(host, port)
        reachable = False
        http_status: int | None = None
        notes = dns_note
        latency_ms = 0

        if dns_ok:
            reachable, http_status, notes, latency_ms = http_probe(url, method, timeout_s)
            if not reachable and method == "HEAD":
                # Some providers reject HEAD but respond to GET.
                reachable, http_status, notes, latency_ms = http_probe(url, "GET", timeout_s)

        rows.append(
            {
                "provider": provider,
                "url": url,
                "critical": critical,
                "dns_ok": dns_ok,
                "http_status": http_status,
                "reachable": bool(dns_ok and reachable),
                "latency_ms": latency_ms,
                "notes": notes,
            }
        )

    critical_failures = [r for r in rows if r["critical"] and not r["reachable"]]
    payload = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "timeout_seconds": timeout_s,
        "total_endpoints": len(rows),
        "reachable_endpoints": sum(1 for r in rows if r["reachable"]),
        "critical_failures": len(critical_failures),
        "rows": rows,
    }

    json_path = Path(args.output_json)
    md_path = Path(args.output_md)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    md_path.write_text(build_markdown(rows, timeout_s, critical_failures), encoding="utf-8")

    if args.fail_on_critical and critical_failures:
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
