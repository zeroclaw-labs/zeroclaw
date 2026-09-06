#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"

python3 - "$repo_root/.github/workflows/ci.yml" <<'PY'
import re
import sys
from pathlib import Path

workflow = Path(sys.argv[1]).read_text()
command = (
    "cargo nextest run --locked --no-fail-fast "
    "-p zeroclaw-hardware --features hardware --lib"
)


def job(name: str) -> str:
    match = re.search(
        rf"(?ms)^  {re.escape(name)}:\n(?P<body>.*?)(?=^  [a-z0-9][a-z0-9-]*:\n|\Z)",
        workflow,
    )
    if match is None:
        raise AssertionError(f"missing workflow job: {name}")
    return match.group("body")


linux = job("test")
windows = job("windows-test")

assert linux.count(command) == 1, "required Linux Test job must execute hardware lib tests"
assert windows.count(command) == 1, "advisory Windows job must execute hardware lib tests"
assert "hardware_status=${PIPESTATUS[0]}" in windows
assert "if (( hardware_status != 0 ))" in windows
assert 'failure_inventory="${failure_inventory:+$failure_inventory,}hardware"' in windows
assert "| Hardware feature status | %s |" in windows

for lane in (linux, windows):
    assert "--run-ignored" not in lane
    assert "--ignored" not in lane

print("hardware feature test lanes preserve required Linux and advisory Windows coverage")
PY
