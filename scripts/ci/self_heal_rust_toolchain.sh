#!/usr/bin/env bash
set -euo pipefail

# Remove corrupted toolchain installs that can break rustc startup on long-lived runners.
# Usage: ./scripts/ci/self_heal_rust_toolchain.sh [toolchain]

TOOLCHAIN="${1:-1.92.0}"

# Use per-job Rust homes on self-hosted runners to avoid cross-runner corruption/races.
if [ -n "${RUNNER_TEMP:-}" ]; then
  CARGO_HOME="${RUNNER_TEMP%/}/cargo-home"
  RUSTUP_HOME="${RUNNER_TEMP%/}/rustup-home"
  mkdir -p "${CARGO_HOME}" "${RUSTUP_HOME}"
  export CARGO_HOME RUSTUP_HOME
  export PATH="${CARGO_HOME}/bin:${PATH}"
  if [ -n "${GITHUB_ENV:-}" ]; then
    {
      echo "CARGO_HOME=${CARGO_HOME}"
      echo "RUSTUP_HOME=${RUSTUP_HOME}"
    } >> "${GITHUB_ENV}"
  fi
  if [ -n "${GITHUB_PATH:-}" ]; then
    echo "${CARGO_HOME}/bin" >> "${GITHUB_PATH}"
  fi
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "rustup not installed yet; skipping rust toolchain self-heal."
  exit 0
fi

if rustc "+${TOOLCHAIN}" --version >/dev/null 2>&1 && cargo "+${TOOLCHAIN}" --version >/dev/null 2>&1; then
  echo "Rust toolchain ${TOOLCHAIN} is healthy (rustc + cargo present)."
  exit 0
fi

echo "Rust toolchain ${TOOLCHAIN} appears unhealthy (missing rustc/cargo); removing cached installs."
for candidate in \
  "${TOOLCHAIN}" \
  "${TOOLCHAIN}-x86_64-apple-darwin" \
  "${TOOLCHAIN}-aarch64-apple-darwin" \
  "${TOOLCHAIN}-x86_64-unknown-linux-gnu" \
  "${TOOLCHAIN}-aarch64-unknown-linux-gnu"
do
  rustup toolchain uninstall "${candidate}" >/dev/null 2>&1 || true
done
