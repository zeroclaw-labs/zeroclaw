#!/usr/bin/env bash
# Shell-level regression tests for act-local artifact compatibility policy.

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

run_case 'act version 0.2.89' release-stable-manual:web
expect_status 'unsupported explicit artifact job' 1
expect_output 'unsupported policy' 'not yet satisfied by any release'
expect_output 'hosted fallback' 'Use GitHub-hosted Actions as the fallback'
expect_log_empty 'unsupported explicit artifact job'

run_case 'act version 0.2.90' release-stable-manual:web
expect_status 'minimum supported version' 0
expect_log_count 'minimum supported version' 1

run_case 'act version v1.0.0' release-stable-manual:web
expect_status 'supported major version with v prefix' 0
expect_log_count 'supported major version with v prefix' 1

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

run_case 'act version 0.2.90' --all
expect_status 'supported all sweep' 0
expect_log_count 'supported all sweep' 2

run_case 'act version 0.10.0' --all
expect_status 'minor versions compare numerically' 0
expect_log_count 'minor versions compare numerically' 2

printf 'passed: %d\n' "$pass"
printf 'failed: %d\n' "$fail"
if ((fail > 0)); then
  exit 1
fi
