#!/usr/bin/env bash
set -euo pipefail

attempts="${CI_SMOKE_BUILD_ATTEMPTS:-3}"

if ! [[ "$attempts" =~ ^[0-9]+$ ]] || [ "$attempts" -lt 1 ]; then
    echo "::error::CI_SMOKE_BUILD_ATTEMPTS must be a positive integer (got: ${attempts})" >&2
    exit 2
fi

IFS=',' read -r -a retryable_codes <<< "${CI_SMOKE_RETRY_CODES:-143,137}"

is_retryable_code() {
    local code="$1"
    local candidate=""
    for candidate in "${retryable_codes[@]}"; do
        candidate="${candidate//[[:space:]]/}"
        if [ "$candidate" = "$code" ]; then
            return 0
        fi
    done
    return 1
}

build_cmd=(cargo build --package zeroclaw --bin zeroclaw --profile release-fast --locked)

attempt=1
while [ "$attempt" -le "$attempts" ]; do
    echo "::group::Smoke build attempt ${attempt}/${attempts}"
    echo "Running: ${build_cmd[*]}"
    set +e
    "${build_cmd[@]}"
    code=$?
    set -e
    echo "::endgroup::"

    if [ "$code" -eq 0 ]; then
        echo "Smoke build succeeded on attempt ${attempt}/${attempts}."
        exit 0
    fi

    if [ "$attempt" -ge "$attempts" ] || ! is_retryable_code "$code"; then
        echo "::error::Smoke build failed with exit code ${code} on attempt ${attempt}/${attempts}."
        exit "$code"
    fi

    echo "::warning::Smoke build exited with ${code} (transient runner interruption suspected). Retrying..."
    sleep 10
    attempt=$((attempt + 1))
done

echo "::error::Smoke build did not complete successfully."
exit 1
