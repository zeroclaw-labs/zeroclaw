#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "usage: $0 <classify EXIT_CODE|render> JSON_FILE" >&2
    exit 2
}

validate_json() {
    local json_file="$1"

    jq -s -e '
        def valid_dependency:
            type == "object"
            and (.name | type == "string")
            and (.project | type == "string")
            and (.compat | type == "string")
            and (.latest | type == "string")
            and ((.kind == null) or (.kind | type == "string"))
            and ((.platform == null) or (.platform | type == "string"));
        def valid_crate:
            type == "object"
            and (.crate_name | type == "string")
            and (.dependencies | type == "array")
            and all(.dependencies[]; valid_dependency);
        length > 0 and all(.[]; valid_crate)
    ' "$json_file" >/dev/null
}

has_findings() {
    local json_file="$1"
    jq -s -e 'any(.[]; (.dependencies | length) > 0)' "$json_file" >/dev/null
}

classify() {
    local exit_code="$1"
    local json_file="$2"

    case "$exit_code" in
        ''|*[!0-9]*)
            echo "scanner exit code must be a non-negative integer" >&2
            exit 2
            ;;
    esac

    if ! validate_json "$json_file"; then
        echo "cargo-outdated did not emit valid JSON output" >&2
        exit 2
    fi

    case "$exit_code" in
        0)
            if has_findings "$json_file"; then
                echo "cargo-outdated reported findings with a clean exit status" >&2
                exit 2
            fi
            echo clean
            ;;
        10)
            if ! has_findings "$json_file"; then
                echo "cargo-outdated exited 10 without dependency findings" >&2
                exit 2
            fi
            echo outdated
            ;;
        *)
            echo "cargo-outdated failed with exit code ${exit_code}" >&2
            exit 2
            ;;
    esac
}

render() {
    local json_file="$1"

    validate_json "$json_file"
    jq -s -r '
        (["Workspace crate", "Dependency", "Project", "Compat", "Latest", "Kind", "Platform"] | @tsv),
        (.[] as $crate
            | $crate.dependencies[]
            | [
                $crate.crate_name,
                .name,
                .project,
                .compat,
                .latest,
                (.kind // "---"),
                (.platform // "---")
            ]
            | @tsv)
    ' "$json_file"
}

case "${1:-}" in
    classify)
        [[ "$#" -eq 3 ]] || usage
        classify "$2" "$3"
        ;;
    render)
        [[ "$#" -eq 2 ]] || usage
        render "$2"
        ;;
    *)
        usage
        ;;
esac
