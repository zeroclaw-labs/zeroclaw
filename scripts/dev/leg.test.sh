#!/usr/bin/env bash
# Exercise the real partition, runner and CI shell with a fake Cargo launcher.
set -euo pipefail
unset CDPATH
source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/leg-test.XXXXXX")"
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/scripts/dev" "$fixture/dev/ci" "$fixture/bin"
cp "$source_root/scripts/dev/leg.sh" "$fixture/scripts/dev/"
cp "$source_root/dev/ci/test-partition.json" "$fixture/dev/ci/"
cat > "$fixture/scripts/dev/cargo-local.sh" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
event="$(printf '%q ' "$@")"
event="${event% }"
printf '%s\n' "$event" >> "$TEST_LEG_TRACE"
# A child must not consume the extra-run loop's next record.
cat >/dev/null
if [[ "$event" == "${TEST_LEG_FAIL_EVENT:-}" ]]; then exit 7; fi
CARGO
chmod +x "$fixture/scripts/dev/cargo-local.sh"
cp "$fixture/scripts/dev/cargo-local.sh" "$fixture/bin/cargo"
export PATH="$fixture/bin:$PATH" TEST_LEG_TRACE="$fixture/trace"
export LEG_RUNNER=test TEST_LEG_FAIL_EVENT=
cd "$fixture"

run() {
  : > "$TEST_LEG_TRACE"
  status=0
  "$@" > "$fixture/output" 2>&1 || status=$?
}

check() {
  local expected_status="$1"
  shift
  if [[ "$status" -ne "$expected_status" ]]; then
    cat "$fixture/output" >&2
    echo "expected exit $expected_status, got $status" >&2
    exit 1
  fi
  : > "$fixture/expected"
  if [[ $# -gt 0 ]]; then printf '%s\n' "$@" > "$fixture/expected"; fi
  diff -u "$fixture/expected" "$TEST_LEG_TRACE"
}

app='-p zeroclawlabs -p zeroclaw-config --features zeroclawlabs/nodes'
migration='-p zeroclawlabs --test component --features zeroclawlabs/nodes\,zeroclawlabs/memory-tachi -- memory_kernel_migration_cli'
for LEG_RUNNER in test nextest; do
  prefix=test
  [[ "$LEG_RUNNER" == test ]] || prefix='nextest run'
  main="$prefix --locked $app"
  extra="$prefix --locked $migration"

  run bash scripts/dev/leg.sh app
  check 0 "$main" "$extra"
  run bash scripts/dev/leg.sh --no-run app
  check 0 "$prefix --locked --no-run $app" "$prefix --locked --no-run $migration"
  # Preserve old post-leg arguments: only the explicit pre-leg mode covers all phases.
  run bash scripts/dev/leg.sh app --no-run
  check 0 "$main --no-run" "$extra"
  run bash scripts/dev/leg.sh app -- 'name with spaces'
  check 0 "$main -- name\ with\ spaces" "$extra"
  run bash scripts/dev/leg.sh --no-run --dry-run app
  check 0
  TEST_LEG_FAIL_EVENT="$main" run bash scripts/dev/leg.sh app
  check 7 "$main"
  TEST_LEG_FAIL_EVENT="$extra" run bash scripts/dev/leg.sh app
  check 7 "$main" "$extra"
done

LEG_RUNNER=test
run bash scripts/dev/leg.sh runtime
check 0 \
  'test --locked -p zeroclaw-runtime --features zeroclaw-runtime/heavy-tests\,zeroclaw-runtime/memory-tachi' \
  'test --locked -p zeroclaw-runtime --features zeroclaw-runtime/heavy-tests\,zeroclaw-runtime/memory-tachi\,zeroclaw-runtime/sandbox-landlock -- landlock'
run bash scripts/dev/leg.sh channels
check 0 \
  'test --locked -p zeroclaw-gateway --features zeroclaw-gateway/nodes\,zeroclaw-gateway/memory-tachi' \
  'test --locked -p zeroclaw-gateway --features zeroclaw-gateway/nodes\,zeroclaw-gateway/memory-tachi\,zeroclaw-gateway/voice-surface -- surface_tests'

# Run the actual CI step's shell (its env is set as in the workflow), not a copy
# of its command construction. No YAML dependency is needed for this block.
awk '
  /^      - name: Run tests$/ { step = 1; next }
  step && /^        run: \|$/ { body = 1; next }
  body && /^          / { sub(/^          /, ""); print; next }
  body && /^$/ { print; next }
  body { exit }
' "$source_root/.github/workflows/ci.yml" > "$fixture/ci-run.sh"
[[ -s "$fixture/ci-run.sh" ]] || { echo 'missing CI test step' >&2; exit 1; }
export LEG=app LEG_RUNNER=nextest CARGO_PROFILE_DEV_DEBUG=line-tables-only
main="nextest run --locked --no-fail-fast $app"
extra="nextest run --locked --no-fail-fast $migration"
run bash -e -o pipefail "$fixture/ci-run.sh"
check 0 "$main" "$extra"
TEST_LEG_FAIL_EVENT="$main" run bash -e -o pipefail "$fixture/ci-run.sh"
check 7 "$main"

# Exercise multiple extra phases and prove a failed extra prevents later work.
jq '.extra_runs.app += [{"name":"following","features":["following"],"filter":"following"}]' \
  dev/ci/test-partition.json > "$fixture/partition.json"
mv "$fixture/partition.json" dev/ci/test-partition.json
main="nextest run --locked $app"
extra="nextest run --locked $migration"
run bash scripts/dev/leg.sh app
check 0 "$main" "$extra" \
  'nextest run --locked -p zeroclawlabs -p zeroclaw-config --features zeroclawlabs/nodes\,following -- following'
TEST_LEG_FAIL_EVENT="$extra" run bash scripts/dev/leg.sh app
check 7 "$main" "$extra"

echo 'PASS: targeted extra builds, compile-only phases, CI parity, legacy arguments and failure propagation'
