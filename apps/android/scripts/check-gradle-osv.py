#!/usr/bin/env python3
"""Fail when a resolved Android runtime dependency has an OSV advisory."""

from __future__ import annotations

import argparse
import http.client
import json
import os
import re
import ssl
import subprocess
import sys
import time
from pathlib import Path

OSV_HOST = "api.osv.dev"
OSV_PATH = "/v1/querybatch"
DEPENDENCY = re.compile(
    r"---\s+([^:\s]+):([^:\s]+):([^\s]+)(?:\s+->\s+([^\s]+))?"
)
RUNTIME_CONFIGURATIONS = (
    "debugRuntimeClasspath",
    "releaseRuntimeClasspath",
)


def parse_components(report: str) -> set[tuple[str, str]]:
    components: set[tuple[str, str]] = set()
    for line in report.splitlines():
        match = DEPENDENCY.search(line)
        if not match:
            continue
        group, artifact, requested, selected = match.groups()
        version = (selected or requested).strip("()")
        if version and version != "project":
            components.add((f"{group}:{artifact}", version))
    return components


def merge_components(reports: list[str]) -> set[tuple[str, str]]:
    components: set[tuple[str, str]] = set()
    for report in reports:
        components.update(parse_components(report))
    return components


def query_osv(components: set[tuple[str, str]]) -> list[dict]:
    ordered = sorted(components)
    payload = json.dumps(
        {
            "queries": [
                {
                    "version": version,
                    "package": {"name": name, "ecosystem": "Maven"},
                }
                for name, version in ordered
            ]
        }
    ).encode()
    # python.org macOS installs do not always load the system trust store, while
    # GitHub's Linux runner does. Prefer an explicit operator bundle, then the
    # standard Unix/macOS bundle when present; verification always remains on.
    cert_file = os.environ.get("SSL_CERT_FILE")
    if not cert_file and Path("/etc/ssl/cert.pem").is_file():
        cert_file = "/etc/ssl/cert.pem"
    context = ssl.create_default_context(cafile=cert_file)
    for attempt in range(3):
        connection = http.client.HTTPSConnection(OSV_HOST, timeout=60, context=context)
        try:
            connection.request(
                "POST",
                OSV_PATH,
                body=payload,
                headers={
                    "Content-Type": "application/json",
                    "User-Agent": "zeroclaw-android-osv-check",
                },
            )
            response = connection.getresponse()
            if response.status == 429 or response.status >= 500:
                raise OSError(f"OSV transient HTTP {response.status}")
            if response.status != 200:
                raise RuntimeError(f"OSV returned HTTP {response.status}")
            results = json.loads(response.read())["results"]
            break
        except (TimeoutError, OSError, http.client.HTTPException):
            if attempt == 2:
                raise
            time.sleep(2**attempt)
        finally:
            connection.close()
    findings = []
    for (name, version), result in zip(ordered, results, strict=True):
        vulnerabilities = sorted(vuln["id"] for vuln in result.get("vulns", []))
        if vulnerabilities:
            findings.append(
                {"package": name, "version": version, "vulnerabilities": vulnerabilities}
            )
    return findings


def gradle_report(android_dir: Path, configuration: str) -> str:
    result = subprocess.run(
        [
            str(android_dir / "gradlew"),
            "--no-daemon",
            ":app:dependencies",
            "--configuration",
            configuration,
            "--console=plain",
        ],
        cwd=android_dir,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if result.returncode:
        print(result.stdout, file=sys.stderr)
        raise RuntimeError(f"Gradle dependency report failed for {configuration}")
    return result.stdout


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--report",
        action="append",
        type=Path,
        help="Read an existing Gradle dependency report instead of invoking Gradle",
    )
    args = parser.parse_args()
    android_dir = Path(__file__).resolve().parents[1]
    reports = (
        [path.read_text() for path in args.report]
        if args.report
        else [
            gradle_report(android_dir, configuration)
            for configuration in RUNTIME_CONFIGURATIONS
        ]
    )
    components = merge_components(reports)
    if not components:
        print("No Maven components found in Gradle reports", file=sys.stderr)
        return 2
    findings = query_osv(components)
    if findings:
        print("Resolved Android dependencies have OSV advisories:", file=sys.stderr)
        for finding in findings:
            ids = ", ".join(finding["vulnerabilities"])
            print(
                f"- {finding['package']}:{finding['version']}: {ids}",
                file=sys.stderr,
            )
        return 1
    print(f"OSV: {len(components)} resolved Maven components, 0 advisories")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
