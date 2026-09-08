#!/usr/bin/env bash
# Bounded cargo-fuzz smoke runner for the secure-transport parsers (threat A12).
#
# Fuzzes the untrusted-input parsers: the relay wire protocol (Control::from_json,
# decode_data) and the TLS crate (sign_csr CSR parse, client_cert_node_id x509
# parse). Each target runs for FUZZ_SECONDS (default 20). The fuzz crates are
# SEPARATE workspaces, so a normal `cargo build`/clippy never touches libfuzzer;
# only this script (and CI) builds them, under nightly.
#
#   scripts/dev/fuzz.sh                 # ~20s per target
#   FUZZ_SECONDS=120 scripts/dev/fuzz.sh
#   FUZZ_BUILD_ONLY=1 scripts/dev/fuzz.sh   # just compile the harnesses (CI gate)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DURATION="${FUZZ_SECONDS:-20}"
BUILD_ONLY="${FUZZ_BUILD_ONLY:-0}"

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "cargo-fuzz not installed. Install it with:  cargo install cargo-fuzz" >&2
  exit 1
fi

# cargo-fuzz needs a nightly toolchain; accept a dated nightly if that is what is
# installed (rustup has no implicit 'nightly' alias for a dated install).
NIGHTLY="$(rustup toolchain list 2>/dev/null | grep -oE 'nightly[^ ]*' | head -n1 || true)"
if [ -z "$NIGHTLY" ]; then
  echo "no nightly toolchain found. Install one with:  rustup toolchain install nightly" >&2
  exit 1
fi

run_crate() {
  # $1 = parent crate dir (holding fuzz/); remaining args = target names.
  local crate_dir="$1"; shift
  for target in "$@"; do
    echo "== fuzz ${crate_dir} :: ${target} (toolchain ${NIGHTLY}) =="
    if [ "$BUILD_ONLY" = "1" ]; then
      ( cd "${ROOT}/${crate_dir}" && cargo "+${NIGHTLY}" fuzz build "${target}" )
    else
      ( cd "${ROOT}/${crate_dir}" && cargo "+${NIGHTLY}" fuzz run "${target}" -- -max_total_time="${DURATION}" )
    fi
  done
}

run_crate crates/zeroclaw-relay-proto control_from_json decode_data
run_crate crates/zeroclaw-tls sign_csr client_cert_node_id

echo "fuzz smoke complete"
