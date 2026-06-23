#!/usr/bin/env bash
# agent-preflight.sh — pre-PR validation gate for automated (and human) contributors.
#
# Mirrors CI's fmt / lint / check / test command lines locally before a pull
# request is opened, so agentic coding pipelines catch the same class of
# failures before pushing. Local toolchains and system packages can still differ
# from the CI runner. Idempotent: safe to run repeatedly (it auto-applies
# rustfmt, everything else is read-only).
#
# When a container runtime + act are present it also runs the release-runbook
# pre-release gate (Step 3: dry-run the release workflows locally with act) via
# scripts/dev/act-local.sh, so the same release-stable-manual / cross-platform
# build jobs that run at tag time are exercised before merge — not just at
# release. See: maintainers/release-runbook "Dry-run the release workflows".
#
#   scripts/agent-preflight.sh ["<proposed PR title>"]
#
# Exit 0 = local CI-parity gates passed. Non-zero = fix the reported failures first.
# Honors CARGO_BUILD_JOBS / RUSTFLAGS from the environment.
# Env toggles for the pre-release gate:
#   PREFLIGHT_SKIP_RELEASE_GATE=1     force-skip the act dry-run (fast path)
#   PREFLIGHT_REQUIRE_RELEASE_GATE=1  treat missing act/runtime as a hard failure
#                                     (set this on CI / build hosts such as the
#                                     Ultra and Cerberus battery runners)
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2
fail=0
run() { printf '\n\033[1m==> %s\033[0m\n' "$*"; "$@" || { echo "::error::FAILED: $*"; fail=1; }; }

# 1. Format — auto-apply then verify (idempotent). Format gates all of CI.
printf '\n\033[1m==> cargo fmt --all (auto-apply)\033[0m\n'; cargo fmt --all || fail=1
run cargo fmt --all -- --check

# 2. Repo quality gate (clippy -D warnings + provider-dispatch SSOT gate).
run ./scripts/ci/rust_quality_gate.sh --strict

# 2b. CI's Lint job uses the curated ci-all feature set, not --all-features.
run cargo clippy --workspace --exclude zeroclaw-desktop --all-targets --features ci-all -- -D warnings

# 3. CI's Check matrix.
run cargo check --locked --features ci-all
run cargo check --locked --no-default-features

# 4. CI's Test job, with a cargo-nextest fallback for local machines.
if cargo nextest --version >/dev/null 2>&1; then
  run cargo nextest run --locked --workspace --exclude zeroclaw-desktop
else
  printf '\n\033[33m(note) cargo-nextest not found; falling back to cargo test with CI workspace/exclude flags.\033[0m\n'
  run cargo test --locked --workspace --exclude zeroclaw-desktop
fi

# 4b. Architecture guards from CI's Lint job.
run cargo test --test architecture tests_that_persist_config_isolate_the_path
run cargo test --test architecture user_facing_strings_route_through_fluent

# 5. PR title — Conventional Commits with scope (the `main` CI check).
if [ "${1-}" != "" ]; then
  run ./scripts/check-pr-title.sh "$1"
else
  printf '\n\033[33m(note) pass your proposed PR title as $1 to validate it: scripts/agent-preflight.sh "fix(scope): ..."\033[0m\n'
fi

# 6. Pre-release gate — dry-run the release/CI workflows locally with act.
#    The runbook "pre-release PR gate" (release-runbook Step 3): exercises the
#    release-stable-manual + cross-platform-build jobs that only fire at tag
#    time, catching workflow_dispatch-only breakage before merge. Heavy
#    (container builds), so it runs only when a container runtime + act are
#    available; absence is a loud note locally and a hard failure on hosts that
#    set PREFLIGHT_REQUIRE_RELEASE_GATE=1.
release_gate() {
  if [ "${PREFLIGHT_SKIP_RELEASE_GATE:-0}" = "1" ]; then
    printf '\n\033[33m(skip) pre-release gate disabled via PREFLIGHT_SKIP_RELEASE_GATE=1\033[0m\n'
    return 0
  fi
  local have_act=0 runtime_up=0
  if command -v act >/dev/null 2>&1 || gh extension list 2>/dev/null | grep -q 'gh act'; then
    have_act=1
  fi
  if docker info >/dev/null 2>&1 || podman info >/dev/null 2>&1; then
    runtime_up=1
  fi
  if [ "$have_act" -ne 1 ] || [ "$runtime_up" -ne 1 ] || [ ! -x scripts/dev/act-local.sh ]; then
    local why="pre-release gate skipped: needs act (gh extension install nektos/gh-act) + a running container runtime (docker/podman) + scripts/dev/act-local.sh. Run it on a capable host: ./scripts/dev/act-local.sh --all"
    if [ "${PREFLIGHT_REQUIRE_RELEASE_GATE:-0}" = "1" ]; then
      echo "::error::$why"
      fail=1
    else
      printf '\n\033[33m(note) %s\033[0m\n' "$why"
    fi
    return 0
  fi
  run ./scripts/dev/act-local.sh --all
}
release_gate

echo
if [ "$fail" -ne 0 ]; then
  echo "================================================================"
  echo " PREFLIGHT FAILED — do NOT open a PR until the above are fixed."
  echo " (Automated pipelines: treat a non-zero exit as a hard gate.)"
  echo "================================================================"
  exit 1
fi
echo "PREFLIGHT PASSED — local CI-parity + pre-release gates passed; ready to open a PR."
