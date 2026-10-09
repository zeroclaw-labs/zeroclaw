#!/usr/bin/env python3
"""Materialize the selector's event policy before GitHub assigns any runners.

HIGH_RISK in scope.py is canonical. GitHub job conditions cannot read checkout
files or workflow env, so their expressions are generated and checked here.
Existing per-job prerequisites remain alongside this event filter.
"""

import argparse
import json
from pathlib import Path
import re

from scope import HIGH_RISK, ROOT


def expression():
    labels = json.dumps(sorted(HIGH_RISK), separators=(",", ":"))
    return ("!(github.event_name == 'workflow_dispatch' && inputs.acceptance_cost == true) && "
            "(github.event_name != 'pull_request' || "
            "!contains(fromJSON('[\"labeled\",\"unlabeled\"]'), github.event.action) || "
            "!github.event.label.name || "
            f"contains(fromJSON('{labels}'), github.event.label.name))")


def selected(event, action=None, label=None, acceptance_cost=False):
    if event == "workflow_dispatch" and acceptance_cost:
        return False
    return event != "pull_request" or action not in ("labeled", "unlabeled") or not label or label.lower() in HIGH_RISK


def materialize(workflow):
    guard = expression()
    # The generated expression has a distinctive prefix and a bounded final
    # label list; replace its previous version before updating policy values.
    previous = re.compile(r"!\(github.event_name == 'workflow_dispatch' && inputs.acceptance_cost == true\) && "
                          r"\(github.event_name != 'pull_request' \|\| "
                          r"!contains\(fromJSON\('\[\"labeled\",\"unlabeled\"\]'\), github.event.action\) \|\| "
                          r"!github.event.label.name \|\| contains\(fromJSON\('[^']+'\), github.event.label.name\)\)")
    workflow = previous.sub(guard, workflow)
    # A no-op must never join/cancel the active code-run group. Relevant label
    # changes still supersede old runs and reevaluate full acceptance.
    group = "${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}"
    workflow = re.sub(r"(?m)^  group: .+$", "  group: " + group + "-${{ (" + guard + ") && 'quality' || github.run_id }}", workflow, count=1)
    header, jobs = workflow.split("\njobs:\n", 1)
    blocks = re.split(r"(?m)(?=^  [a-z][a-z0-9-]*:\n)", jobs)
    for index, block in enumerate(blocks):
        match = re.match(r"  ([a-z][a-z0-9-]*):\n", block)
        if not match or match[1] == "acceptance-cost":
            continue
        if match[1] == "crates-preflight":
            # The guarded detector is its required success() prerequisite.
            # Keep the release contract's exact publish/preflight condition.
            blocks[index] = re.sub(r"(?m)^    if: .+$", "    if: needs.crates-preflight-changes.outputs.run == 'true'", block, count=1)
            continue
        if match[1] == "master-debounce":
            # Its existing push-only predicate already excludes all label and
            # measurement events; preserve the architecture gate's exact rule.
            blocks[index] = re.sub(r"(?m)^    if: .+$", "    if: github.event_name == 'push'", block, count=1)
            continue
        condition = re.search(r"(?m)^    if: (.+)$", block)
        if condition:
            value = condition[1]
            if guard not in value:
                if value.startswith("${{") and value.endswith("}}"):
                    value = value[3:-2].strip()
                block = block[:condition.start()] + "    if: ${{ (" + guard + ") && (" + value + ") }}" + block[condition.end():]
        else:
            offset = match.end()
            block = block[:offset] + "    if: ${{ " + guard + " }}\n" + block[offset:]
        if match[1] == "gate":
            block = re.sub(r"(?m)^    name: .+$", "    name: ${{ (" + guard + ") && 'CI Required Gate' || 'Quality Gate not requested' }}", block, count=1)
        blocks[index] = block
    return header + "\njobs:\n" + "".join(blocks)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    path = ROOT / ".github/workflows/ci.yml"
    current = path.read_text()
    expected = materialize(current)
    if args.write:
        path.write_text(expected)
    elif current != expected:
        raise SystemExit("CI event filters drifted; run python3 tests/system/runtime_acceptance/workflow_policy.py --write")


if __name__ == "__main__":
    main()
