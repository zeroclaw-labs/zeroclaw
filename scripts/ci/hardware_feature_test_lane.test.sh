#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"

python3 - "$repo_root/.github/workflows/ci.yml" "$repo_root/.github/workflows/windows-tests.yml" <<'PY'
import re
import sys
from pathlib import Path

required_workflow = Path(sys.argv[1]).read_text()
advisory_workflow = Path(sys.argv[2]).read_text()
command = (
    "cargo nextest run --locked --no-fail-fast "
    "-p zeroclaw-hardware --features hardware --lib"
)


def job(workflow: str, name: str) -> str:
    match = re.search(
        rf"(?ms)^  {re.escape(name)}:\n(?P<body>.*?)(?=^  [a-z0-9][a-z0-9-]*:\n|\Z)",
        workflow,
    )
    if match is None:
        raise AssertionError(f"missing workflow job: {name}")
    return match.group("body")


linux = job(required_workflow, "test")
windows = job(advisory_workflow, "windows-test")

assert linux.count(command) == 1, "required Linux Test job must execute hardware lib tests"
assert windows.count(command) == 1, "advisory Windows job must execute hardware lib tests"
assert "hardware_status=${PIPESTATUS[0]}" in windows
assert "if (( hardware_status != 0 ))" in windows
assert 'failure_inventory="${failure_inventory:+$failure_inventory,}hardware"' in windows
assert "| Hardware feature status | %s |" in windows

def steps(job_body: str) -> list[str]:
    return re.split(r"(?m)^      - ", job_body)[1:]


# The hardware lanes must never opt into ignored tests: in zeroclaw-hardware
# those are physical-device cases that may flash attached hardware. The
# Windows job runs nothing else that needs ignored tests, so it keeps a
# job-wide ban.
hardware_steps = [step for step in steps(linux) if command in step]
assert len(hardware_steps) == 1, "exactly one Linux step runs the hardware lib tests"
for lane in (hardware_steps[0], windows):
    assert "--run-ignored" not in lane
    assert "--ignored" not in lane

# Other steps in the Linux Test job may run ignored tests of their own (for
# example the informational gateway golden-frame replay), but only through a
# filterset that names a package other than zeroclaw-hardware, so they can
# never select the physical-device cases.
for step in steps(linux):
    if "--run-ignored" not in step and "--ignored" not in step:
        continue
    packages = re.findall(r"package\(([^)]+)\)", step)
    assert packages, "an ignored-test step in the Linux Test job must filter by package()"
    assert "zeroclaw-hardware" not in step, (
        "an ignored-test step in the Linux Test job must not select zeroclaw-hardware"
    )

print("hardware feature test lanes preserve required Linux and advisory Windows coverage")
PY
