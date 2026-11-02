#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
classifier="${script_dir}/monthly_outdated_result.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

clean_json="${fixture_dir}/clean.json"
outdated_json="${fixture_dir}/outdated.json"
empty_json="${fixture_dir}/empty.json"
malformed_json="${fixture_dir}/malformed.json"
invalid_schema_json="${fixture_dir}/invalid-schema.json"

cat > "$clean_json" <<'JSON'
{"crate_name":"zeroclaw","dependencies":[]}
{"crate_name":"zeroclaw-runtime","dependencies":[]}
JSON

cat > "$outdated_json" <<'JSON'
{"crate_name":"zeroclaw","dependencies":[{"name":"serde","project":"1.0.0","compat":"1.0.1","latest":"1.1.0","kind":"Normal","platform":null}]}
{"crate_name":"zeroclaw-runtime","dependencies":[]}
JSON

: > "$empty_json"
printf '%s\n' 'error: failed to parse manifest' > "$malformed_json"
printf '%s\n' '{"crate_name":"zeroclaw","dependencies":"not-an-array"}' > "$invalid_schema_json"

expect_state() {
    local expected="$1"
    local exit_code="$2"
    local json_file="$3"
    local actual

    actual="$(bash "$classifier" classify "$exit_code" "$json_file")"
    if [[ "$actual" != "$expected" ]]; then
        echo "expected state '$expected', got '$actual'" >&2
        exit 1
    fi
}

expect_failure() {
    local exit_code="$1"
    local json_file="$2"

    if bash "$classifier" classify "$exit_code" "$json_file" >/dev/null 2>&1; then
        echo "expected classifier failure for exit ${exit_code} and ${json_file}" >&2
        exit 1
    fi
}

expect_state clean 0 "$clean_json"
expect_state outdated 10 "$outdated_json"
expect_failure 1 "$empty_json"
expect_failure 1 "$malformed_json"
expect_failure 1 "$outdated_json"
expect_failure 1 "$clean_json"
expect_failure 0 "$outdated_json"
expect_failure 10 "$clean_json"
expect_failure 10 "$invalid_schema_json"
expect_failure 2 "$outdated_json"
expect_failure not-a-number "$outdated_json"

rendered="$(bash "$classifier" render "$outdated_json")"
grep -F $'zeroclaw\tserde\t1.0.0\t1.0.1\t1.1.0\tNormal\t---' <<< "$rendered" >/dev/null

python3 - "$classifier" "$fixture_dir" <<'PY'
import subprocess
import sys
from pathlib import Path

classifier, fixture_dir = sys.argv[1:]
run_url = "https://example.test/actions/runs/1"
artifact_url = "https://example.test/actions/runs/1/artifacts/1"
reports = {
    "short": "Workspace crate\tDependency\nzeroclaw\tserde\n",
    "ascii": "zeroclaw\tdependency\t1.0.0\t1.0.1\t2.0.0\tNormal\t---\n" * 400,
    "unicode": ("\U0001f980" * 40 + "\n") * 400,
}

for name, report in reports.items():
    report_file = Path(fixture_dir) / f"{name}-report.txt"
    report_file.write_text(report, encoding="utf-8")
    body_bytes = subprocess.check_output(
        ["bash", classifier, "issue-body", str(report_file), run_url, artifact_url]
    )
    body = body_bytes.decode("utf-8")
    assert len(body_bytes) < 60000, f"{name}: body exceeds byte budget"
    assert f"Workflow run: {run_url}" in body, f"{name}: missing run URL"
    assert f"[Download the complete scan report]({artifact_url})" in body, f"{name}: missing artifact link"
    preview = body.split("```\n", 1)[1].split("\n```", 1)[0]
    if name == "short":
        assert preview == report, "short: report changed"
        assert "The preview is truncated." not in body, "short: unexpected truncation"
    else:
        assert "The preview is truncated." in body, f"{name}: missing truncation notice"
        assert 0 < len(preview) <= 12000, f"{name}: invalid preview length"
        assert len(preview) < len(report), f"{name}: report was not truncated"
        assert report.startswith(preview), f"{name}: preview is not an intact report prefix"
        assert preview.endswith("\n"), f"{name}: preview ends with a partial line"

over_budget = subprocess.run(
    ["bash", classifier, "issue-body", str(report_file), "x" * 60000, artifact_url],
    capture_output=True,
)
assert over_budget.returncode != 0, "oversized metadata: expected formatter failure"
assert b"issue body exceeds the reporting budget" in over_budget.stderr, "oversized metadata: wrong failure"
PY

echo "monthly outdated result tests passed"
