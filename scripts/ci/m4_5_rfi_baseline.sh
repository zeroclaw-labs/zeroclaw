#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'USAGE'
Usage: scripts/ci/m4_5_rfi_baseline.sh [target_dir]

Run reproducible compile-timing probes for the current workspace.
The script prints a markdown table with real-time seconds and pass/fail status
for each benchmark phase.
USAGE
  exit 0
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET_DIR="${1:-${ROOT_DIR}/target-rfi}"

cd "${ROOT_DIR}"

if [[ ! -f Cargo.toml ]]; then
  echo "error: Cargo.toml not found at ${ROOT_DIR}" >&2
  exit 1
fi

run_timed() {
  local label="$1"
  shift

  local timing_file
  timing_file="$(mktemp)"
  local status="pass"

  if /usr/bin/time -p "$@" >/dev/null 2>"${timing_file}"; then
    status="pass"
  else
    status="fail"
  fi

  local real_time
  real_time="$(awk '/^real / { print $2 }' "${timing_file}")"
  rm -f "${timing_file}"

  if [[ -z "${real_time}" ]]; then
    real_time="n/a"
  fi

  printf '| %s | %s | %s |\n' "${label}" "${real_time}" "${status}"

  [[ "${status}" == "pass" ]]
}

printf '# M4-5 RFI Baseline\n\n'
printf '- Timestamp (UTC): %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '- Commit: `%s`\n' "$(git rev-parse --short HEAD)"
printf '- Target dir: `%s`\n\n' "${TARGET_DIR}"
printf '| Phase | real(s) | status |\n'
printf '|---|---:|---|\n'

rm -rf "${TARGET_DIR}"

set +e
run_timed "A: cold cargo check" env CARGO_TARGET_DIR="${TARGET_DIR}" cargo check --workspace --locked
run_timed "B: cold-ish cargo build" env CARGO_TARGET_DIR="${TARGET_DIR}" cargo build --workspace --locked
run_timed "C: warm cargo check" env CARGO_TARGET_DIR="${TARGET_DIR}" cargo check --workspace --locked
touch src/main.rs
run_timed "D: incremental cargo check after touch src/main.rs" env CARGO_TARGET_DIR="${TARGET_DIR}" cargo check --workspace --locked
set -e
