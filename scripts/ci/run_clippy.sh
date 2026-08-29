#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat >&2 <<'EOF'
Usage: run_clippy.sh --scope workspace|tools --summary-title TITLE --log-name NAME [--target TRIPLE]
EOF
    exit 2
}

scope=""
target=""
summary_title=""
log_name=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --scope)
            [ "$#" -ge 2 ] || usage
            scope="$2"
            shift 2
            ;;
        --target)
            [ "$#" -ge 2 ] || usage
            target="$2"
            shift 2
            ;;
        --summary-title)
            [ "$#" -ge 2 ] || usage
            summary_title="$2"
            shift 2
            ;;
        --log-name)
            [ "$#" -ge 2 ] || usage
            log_name="$2"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$scope" ] || usage
[ -n "$summary_title" ] || usage
[ -n "$log_name" ] || usage
case "$log_name" in
    */*) usage ;;
esac

command=(cargo clippy --locked)
case "$scope" in
    workspace)
        command+=(
            --workspace
            --exclude zeroclaw-desktop
            --all-targets
            --features ci-all
        )
        if [ -n "$target" ]; then
            command+=(--target "$target")
        fi
        ;;
    tools)
        [ -n "$target" ] || usage
        command+=(
            -p zeroclaw-tools
            --all-targets
            --all-features
            --target "$target"
            --no-deps
        )
        ;;
    *)
        usage
        ;;
esac
command+=(-- -D warnings)

cargo_log="${RUNNER_TEMP:?RUNNER_TEMP must be set}/${log_name}"
SECONDS=0
set +e
"${command[@]}" 2>&1 | tee "$cargo_log"
cargo_status=${PIPESTATUS[0]}
set -e
duration_seconds=$SECONDS

workspace_path_compiles="$(grep -E -c 'Compiling.*\([^)]*zeroclaw' "$cargo_log" || true)"
total_compiles="$(grep -c 'Compiling' "$cargo_log" || true)"
downloaded_crates="$(grep -c 'Downloaded' "$cargo_log" || true)"
cache_hit="${RUST_CACHE_HIT:-unknown}"
runner_os="${RUNNER_OS:-unknown}"
summary_file="${GITHUB_STEP_SUMMARY:-/dev/null}"

{
    echo "### ${summary_title}"
    echo ""
    echo "| Field | Value |"
    echo "| --- | --- |"
    if [ -n "$target" ]; then
        echo "| Target | \`${target}\` |"
    fi
    echo "| Runner OS | \`${runner_os}\` |"
    echo "| Rust cache exact hit | \`${cache_hit}\` |"
    echo "| Clippy duration | \`${duration_seconds}s\` |"
    echo "| Clippy status | \`${cargo_status}\` |"
    echo "| Workspace path compile lines | \`${workspace_path_compiles}\` |"
    echo "| Total compile lines | \`${total_compiles}\` |"
    echo "| Downloaded crate lines | \`${downloaded_crates}\` |"
} >> "$summary_file"

exit "$cargo_status"
