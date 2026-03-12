#!/usr/bin/env bash
#
# secrets_guard.sh
# Blocks commits/pushes when added lines contain likely credentials/secrets.
# - Uses gitleaks when available
# - Always runs lightweight regex fallback scanner
#
# Usage:
#   ./scripts/ci/secrets_guard.sh --mode staged
#   ./scripts/ci/secrets_guard.sh --mode range --range "<git-range>"

set -euo pipefail

MODE="staged"
RANGE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
    --mode)
        MODE="${2:-}"
        shift 2
        ;;
    --range)
        RANGE="${2:-}"
        shift 2
        ;;
    *)
        echo "Unknown argument: $1" >&2
        exit 2
        ;;
    esac
done

if [[ "${ZEROCLAW_SKIP_SECRET_SCAN:-0}" == "1" ]]; then
    echo "==> secret-scan: skipped (ZEROCLAW_SKIP_SECRET_SCAN=1)"
    exit 0
fi

if [[ "$MODE" != "staged" && "$MODE" != "range" ]]; then
    echo "Invalid mode '$MODE' (expected: staged|range)" >&2
    exit 2
fi

if [[ "$MODE" == "range" && -z "$RANGE" ]]; then
    echo "Range mode requires --range <git-range>" >&2
    exit 2
fi

run_gitleaks() {
    if ! command -v gitleaks >/dev/null 2>&1; then
        echo "==> secret-scan: gitleaks not found, using regex fallback only."
        return 0
    fi

    if [[ "$MODE" == "staged" ]]; then
        echo "==> secret-scan: gitleaks protect --staged"
        gitleaks protect --staged --redact
    else
        echo "==> secret-scan: gitleaks detect --log-opts \"$RANGE\""
        gitleaks detect --redact --source . --log-opts "$RANGE"
    fi
}

added_patch() {
    if [[ "$MODE" == "staged" ]]; then
        git diff --cached --no-color --unified=0 -- .
    else
        git log --no-color -p --unified=0 "$RANGE" -- .
    fi
}

staged_env_files() {
    git diff --cached --name-only --diff-filter=ACMR | rg '(^|/)\.env$' || true
}

range_env_files() {
    git diff --name-only --diff-filter=ACMR "$RANGE" -- | rg '(^|/)\.env$' || true
}

is_placeholder_value() {
    local lower
    lower="$(echo "$1" | tr '[:upper:]' '[:lower:]')"
    [[ "$lower" == *"example"* ]] ||
        [[ "$lower" == *"your-"* ]] ||
        [[ "$lower" == *"your_"* ]] ||
        [[ "$lower" == *"replace"* ]] ||
        [[ "$lower" == *"changeme"* ]] ||
        [[ "$lower" == *"dummy"* ]] ||
        [[ "$lower" == *"sample"* ]] ||
        [[ "$lower" == *"test"* ]] ||
        [[ "$lower" == "xxx"* ]] ||
        [[ "$lower" == "<"* ]] ||
        [[ "$lower" == *">" ]] ||
        [[ "$1" == '$'* ]]
}

is_identifier_like() {
    [[ "$1" =~ ^[A-Z0-9_:/.-]+$ ]]
}

scan_added_lines_fallback() {
    local findings=0
    local line trimmed

    while IFS= read -r line; do
        trimmed="$(echo "$line" | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
        [[ -z "$trimmed" ]] && continue
        [[ "$trimmed" == \#* ]] && continue
        [[ "$trimmed" == //* ]] && continue

        if [[ "$trimmed" =~ -----BEGIN[[:space:]][A-Z0-9[:space:]]+PRIVATE[[:space:]]KEY----- ]]; then
            echo "secret-scan: private key material detected"
            echo "  > $trimmed"
            findings=1
            continue
        fi

        if [[ "$trimmed" =~ (AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{36}|AIza[0-9A-Za-z_-]{35}|xox[baprs]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9_-]{20,}) ]]; then
            echo "secret-scan: token-like pattern detected"
            echo "  > $trimmed"
            findings=1
            continue
        fi

        if [[ "$trimmed" =~ [Bb]earer[[:space:]]+[A-Za-z0-9._-]{20,} ]]; then
            echo "secret-scan: bearer token-like pattern detected"
            echo "  > $trimmed"
            findings=1
            continue
        fi

        if [[ "$trimmed" =~ eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,} ]]; then
            echo "secret-scan: JWT-like token detected"
            echo "  > $trimmed"
            findings=1
            continue
        fi

        if [[ "$trimmed" =~ ^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*(KEY|TOKEN|SECRET|PASSWORD|PASS|PRIVATE_KEY)[A-Za-z0-9_]*)[[:space:]]*[:=][[:space:]]*[\"\']?([^\"\'[:space:]]{8,}) ]]; then
            local var_name="${BASH_REMATCH[1]}"
            local var_value="${BASH_REMATCH[3]}"

            if ! is_placeholder_value "$var_value" && ! is_identifier_like "$var_value"; then
                echo "secret-scan: suspicious credential assignment for '$var_name'"
                echo "  > $trimmed"
                findings=1
            fi
        fi
    done < <(added_patch | awk '/^\+\+\+ / {next} /^\+/ {sub(/^\+/, "", $0); print}')

    if [[ "$MODE" == "staged" ]]; then
        local env_hits
        env_hits="$(staged_env_files)"
        if [[ -n "$env_hits" ]]; then
            echo "secret-scan: refusing staged .env files:"
            echo "$env_hits" | sed 's/^/  - /'
            findings=1
        fi
    else
        local env_hits
        env_hits="$(range_env_files)"
        if [[ -n "$env_hits" ]]; then
            echo "secret-scan: refusing .env files in push range '$RANGE':"
            echo "$env_hits" | sed 's/^/  - /'
            findings=1
        fi
    fi

    return "$findings"
}

run_gitleaks
if ! scan_added_lines_fallback; then
    echo
    echo "FAIL: potential secrets detected."
    echo "If this is a false positive, replace with placeholders or adjust the line."
    echo "Emergency bypass (not recommended): ZEROCLAW_SKIP_SECRET_SCAN=1"
    exit 1
fi

echo "==> secret-scan: passed"
