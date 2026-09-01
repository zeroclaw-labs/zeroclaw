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

echo "monthly outdated result tests passed"
