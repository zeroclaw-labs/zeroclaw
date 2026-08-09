#!/usr/bin/env bash
# Shell-level regression tests for act-local artifact compatibility policy.
#
# ACT_ARTIFACT_MIN_VERSION is an unreachable sentinel ("999.0.0" as of this
# writing) — no released act version is verified to round-trip the pinned
# artifact protocol, so the integration cases below assert fail-closed for
# every real-world-shaped version, including ones that used to be (or look
# like they could plausibly become) the accepted floor. The one exception is
# a synthetic control that feeds the exact sentinel value back in: that case
# proves the comparison logic itself still opens the path when its condition
# is met, so "everything fails" is because no real release meets the bar, not
# because the comparison is silently broken. Numeric version-ordering and
# parsing behavior is additionally exercised directly against the script's
# pure helper functions, decoupled from whether any given version happens to
# clear the current sentinel.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script_under_test="$repo_root/scripts/dev/act-local.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fixture_root="$tmp/repo"
fake_bin="$tmp/bin"
mkdir -p "$fixture_root/scripts/dev" "$fixture_root/.github/workflows" "$fake_bin"
cp "$script_under_test" "$fixture_root/scripts/dev/act-local.sh"

cat >"$fixture_root/.github/workflows/release-stable-manual.yml" <<'EOF'
name: release
on:
  workflow_dispatch:
jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - run: echo validated
  web:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: output
          path: output.txt
  consumer:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
  publish:
    needs: [package]
    runs-on: ubuntu-latest
    steps:
      - run: echo publish
  reusable-caller:
    uses: ./.github/workflows/reusable-artifact.yml
EOF

cat >"$fixture_root/.github/workflows/reusable-artifact.yml" <<'EOF'
name: reusable artifact child
on:
  workflow_call:
jobs:
  child-upload:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: output
          path: output.txt
EOF

cat >"$fake_bin/act" <<'EOF'
#!/usr/bin/env sh
set -eu

if [ "${1:-}" = "--version" ]; then
  printf '%s\n' "${FAKE_ACT_VERSION_OUTPUT:-act version 0.2.89}"
  exit 0
fi

list=false
selected_job=""
previous=""
for arg in "$@"; do
  if [ "$arg" = "-l" ]; then
    list=true
  elif [ "$previous" = "-j" ]; then
    selected_job="$arg"
  fi
  previous="$arg"
done

if [ "$list" = true ]; then
  printf '%s\n' 'Stage  Job ID    Job name  Workflow name  Workflow file  Events'
  case "$selected_job" in
    '')
      printf '%s\n' \
        '0      validate  validate  release        release.yml    workflow_dispatch' \
        '0      web       web       release        release.yml    workflow_dispatch' \
        '0      consumer  consumer  release        release.yml    workflow_dispatch' \
        '0      package   package   release        release.yml    workflow_dispatch' \
        '0      reusable-caller  reusable-caller  release  release.yml  workflow_dispatch' \
        '1      publish   publish   release        release.yml    workflow_dispatch'
      ;;
    publish)
      printf '%s\n' \
        '0      package   package   release        release.yml    workflow_dispatch' \
        '1      publish   publish   release        release.yml    workflow_dispatch'
      ;;
    *)
      printf '0      %s  %s  release  release.yml  workflow_dispatch\n' \
        "$selected_job" "$selected_job"
      ;;
  esac
  exit 0
fi

printf '%s\n' "$*" >>"$FAKE_ACT_LOG"
EOF

cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env sh
set -eu
if [ "${1:-} ${2:-}" = "auth token" ]; then
  printf '%s\n' fake-token
  exit 0
fi
exit 1
EOF

for tool in docker git; do
  cat >"$fake_bin/$tool" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF
done
chmod +x "$fixture_root/scripts/dev/act-local.sh" "$fake_bin"/*

pass=0
fail=0
last_output=""
last_status=0
act_log="$tmp/act.log"

run_case() {
  local version_output="$1"
  shift
  : >"$act_log"
  set +e
  last_output="$({
    PATH="$fake_bin:$PATH" \
      ACT_LOCAL_ARTIFACT_DIR="$tmp/artifacts" \
      FAKE_ACT_LOG="$act_log" \
      FAKE_ACT_VERSION_OUTPUT="$version_output" \
      "$fixture_root/scripts/dev/act-local.sh" "$@"
  } 2>&1)"
  last_status=$?
  set -e
}

record_pass() {
  pass=$((pass + 1))
}

record_fail() {
  fail=$((fail + 1))
  printf 'FAIL: %s\n' "$1"
  printf '%s\n' "$last_output"
}

expect_status() {
  local name="$1" expected="$2"
  if [[ "$last_status" -eq "$expected" ]]; then
    record_pass
  else
    record_fail "$name: expected status $expected, got $last_status"
  fi
}

expect_output() {
  local name="$1" needle="$2"
  if grep -qF "$needle" <<<"$last_output"; then
    record_pass
  else
    record_fail "$name: missing output '$needle'"
  fi
}

expect_log_empty() {
  local name="$1"
  if [[ ! -s "$act_log" ]]; then
    record_pass
  else
    record_fail "$name: act job ran unexpectedly"
  fi
}

expect_log_count() {
  local name="$1" expected="$2" actual
  actual="$(wc -l <"$act_log" | tr -d ' ')"
  if [[ "$actual" -eq "$expected" ]]; then
    record_pass
  else
    record_fail "$name: expected $expected act runs, got $actual"
  fi
}

# ── Direct unit tests for the pure version helpers ─────────────────
#
# Extract parse_released_act_version and version_at_least verbatim from
# the script under test and source them into this shell. This exercises
# numeric-ordering and v-prefix-parsing behavior directly, independent of
# whatever ACT_ARTIFACT_MIN_VERSION currently is — so these assertions
# stay meaningful even though the sentinel makes every integration case
# below fail closed.
version_funcs="$tmp/version_funcs.sh"
awk '
  /^parse_released_act_version\(\) \{/ { grab = 1 }
  /^version_at_least\(\) \{/ { grab = 1 }
  grab { print }
  grab && /^}/ { grab = 0 }
' "$script_under_test" >"$version_funcs"
# shellcheck source=/dev/null
. "$version_funcs"

assert_version_at_least() {
  local name="$1" current="$2" minimum="$3" expected="$4" actual
  if version_at_least "$current" "$minimum"; then actual=0; else actual=1; fi
  if [[ "$actual" -eq "$expected" ]]; then
    record_pass
  else
    record_fail "$name: version_at_least('$current', '$minimum') expected exit $expected, got $actual"
  fi
}

assert_parsed_version() {
  local name="$1" version_output="$2" expected="$3" actual
  actual="$(printf '%s\n' "$version_output" | parse_released_act_version)"
  if [[ "$actual" == "$expected" ]]; then
    record_pass
  else
    record_fail "$name: expected parsed version '$expected', got '$actual'"
  fi
}

# Numeric, not lexicographic: "0.10.0" sorts before "0.2.90" as strings
# (the character '1' < '2'), but 0.10.0 is the newer release (minor 10 >
# minor 2). A lexicographic-comparison bug would get this backwards.
assert_version_at_least 'minor version compares numerically, not lexicographically' '0.10.0' '0.2.90' 0
assert_version_at_least 'numeric minor comparison is not symmetric' '0.2.90' '0.10.0' 1
assert_version_at_least 'equal versions satisfy at-least' '1.2.3' '1.2.3' 0
assert_version_at_least 'lower patch fails at-least' '1.2.2' '1.2.3' 1
assert_version_at_least 'higher major overrides lower minor/patch' '2.0.0' '1.9.9' 0

assert_parsed_version 'v-prefix is stripped' 'act version v1.0.0' '1.0.0'
assert_parsed_version 'unprefixed version passes through' 'act version 0.2.89' '0.2.89'
assert_parsed_version 'prerelease suffix is rejected (empty parse)' 'act version 0.2.90-rc.1' ''
assert_parsed_version 'unparseable version-output line is rejected' 'act development build' ''

run_case 'act version 0.2.89' release-stable-manual:web
expect_status 'unsupported explicit artifact job' 1
expect_output 'unsupported policy' 'not yet satisfied by any release'
expect_output 'hosted fallback' 'Use GitHub-hosted Actions as the fallback'
expect_log_empty 'unsupported explicit artifact job'

# 0.2.90 was the previous (pre-sentinel) accepted floor and is a
# plausible near-future real act release. If upstream ever publishes it,
# the helper must still refuse to run artifact jobs against it — the
# sentinel is unreachable until a specific release passes a real
# round-trip, so no numbered release, however close, opens the path on
# its own. This is the regression guard for the latent auto-open
# condition the sentinel fix closes.
run_case 'act version 0.2.90' release-stable-manual:web
expect_status 'unverified 0.2.90 still fails closed' 1
expect_output 'unverified 0.2.90 fail message' 'not yet satisfied by any release'
expect_output 'unverified 0.2.90 hosted fallback' 'Use GitHub-hosted Actions as the fallback'
expect_log_empty 'unverified 0.2.90 still fails closed'

# A high major version with a "v" prefix must still fail closed against
# the sentinel — but it must fail via the version-comparison "does not
# support" message, not the "could not parse" message, proving the
# v-prefix was stripped and parsed correctly before being compared.
run_case 'act version v1.0.0' release-stable-manual:web
expect_status 'high major version with v prefix still fails closed' 1
expect_output 'v-prefix parsed before comparison' 'act 1.0.0 does not support'
expect_log_empty 'high major version with v prefix still fails closed'

run_case $'act version 0.2.90-rc.1\nruntime version 1.25.0' release-stable-manual:web
expect_status 'prerelease fails closed' 1
expect_output 'prerelease parse error' 'could not parse the released act version'
expect_log_empty 'prerelease fails closed'

run_case 'act development build' release-stable-manual:web
expect_status 'unparseable version fails closed' 1
expect_output 'unparseable version error' 'could not parse the released act version'
expect_log_empty 'unparseable version fails closed'

run_case 'act version 0.2.89' release-stable-manual:validate
expect_status 'old act can run non-artifact job' 0
expect_log_count 'old act can run non-artifact job' 1

run_case 'act version 0.2.89' release-stable-manual:consumer
expect_status 'unsupported artifact consumer' 1
expect_log_empty 'unsupported artifact consumer'

run_case 'act version 0.2.89' release-stable-manual:publish
expect_status 'dependency artifact producer is preflighted' 1
expect_log_empty 'dependency artifact producer is preflighted'

# reusable-caller has no artifact-action step of its own — it only has
# `uses: ./.github/workflows/reusable-artifact.yml`. The artifact
# requirement lives in the *called* workflow (reusable-artifact.yml's
# child-upload job). This exercises job_local_reusable_workflows: the
# preflight must still catch it and fail before the job starts.
run_case 'act version 0.2.89' release-stable-manual:reusable-caller
expect_status 'unsupported local reusable workflow artifact job' 1
expect_output 'reusable workflow policy' 'not yet satisfied by any release'
expect_output 'reusable workflow hosted fallback' 'Use GitHub-hosted Actions as the fallback'
expect_log_empty 'unsupported local reusable workflow artifact job'

run_case 'act version 0.2.89' --all
expect_status 'unsupported all sweep' 1
expect_output 'all policy context' 'error: --all requires the pinned artifact actions'
expect_log_empty 'all preflight runs before every job'

# Same latent-auto-open guard as above, but for --all: a plausible
# near-future release must not let the atomic preflight open the sweep.
run_case 'act version 0.2.90' --all
expect_status 'unverified 0.2.90 still fails closed for --all' 1
expect_output 'all policy context (unverified 0.2.90)' 'error: --all requires the pinned artifact actions'
expect_log_empty 'unverified 0.2.90 still fails closed for --all'

# A "higher-looking" minor version (10 > 2 in the second component) must
# still fail closed against the sentinel — numeric ordering is exercised
# directly against version_at_least above; this integration case confirms
# the full --all path applies the same fail-closed policy regardless of
# how a version happens to sort relative to the old, no-longer-relevant
# 0.2.90 floor.
run_case 'act version 0.10.0' --all
expect_status 'higher minor version still fails closed for --all' 1
expect_log_empty 'higher minor version still fails closed for --all'

# Synthetic control: feed the exact sentinel value back in. This proves
# the comparison logic itself still opens the path when its condition is
# genuinely met — i.e. every case above fails because no real act release
# meets the bar, not because version_at_least or the preflight wiring is
# silently broken and would refuse every input regardless of value.
# "999.0.0" is not a real, installable, or recommended act version; it
# exists only to keep this test suite honest about *why* everything else
# fails closed. Do not read this as install guidance.
run_case 'act version 999.0.0' release-stable-manual:web
expect_status 'sentinel exactly met opens the path (synthetic control)' 0
expect_log_count 'sentinel exactly met opens the path (synthetic control)' 1

run_case 'act version 999.0.0' --all
expect_status 'sentinel exactly met opens --all (synthetic control)' 0
expect_log_count 'sentinel exactly met opens --all (synthetic control)' 2

printf 'passed: %d\n' "$pass"
printf 'failed: %d\n' "$fail"
if ((fail > 0)); then
  exit 1
fi
