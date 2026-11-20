#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ratchet="${root_dir}/scripts/ci/cli_only_ratchet.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

tree="${fixture_dir}/tree.txt"
axum="${fixture_dir}/axum.txt"
symbols="${fixture_dir}/symbols.txt"

reset_fixtures() {
  printf '%s\n' \
    'axum v0.8.9' \
    'zeroclaw v0.8.6 (/repo)' \
    'zeroclaw-channels v0.8.6 (/repo/crates/zeroclaw-channels)' > "$tree"
  printf '%s\n' 'zeroclaw-channels v0.8.6 (/repo/crates/zeroclaw-channels) (*)' > "$axum"
  printf '%s\n' '0000000000001000 T zeroclaw_runtime::daemon::run' > "$symbols"
}

expect_pass() {
  if ! bash "$ratchet" check "$tree" "$axum" "$symbols" >/dev/null; then
    echo "expected the ratchet to pass: $1" >&2
    exit 1
  fi
}

expect_fail() {
  if bash "$ratchet" check "$tree" "$axum" "$symbols" >/dev/null 2>&1; then
    echo "expected the ratchet to fail: $1" >&2
    exit 1
  fi
}

reset_fixtures
expect_pass "axum only through the channel listeners"

reset_fixtures
: > "$axum"
sed -i.bak '/^axum /d' "$tree"
expect_pass "no axum at all"

reset_fixtures
echo 'zeroclaw-gateway v0.8.6 (/repo/crates/zeroclaw-gateway)' >> "$tree"
expect_fail "the gateway crate is linked"

reset_fixtures
echo 'include_dir v0.7.4' >> "$tree"
expect_fail "dashboard assets are embedded"

reset_fixtures
echo 'zeroclaw-gateway v0.8.6 (/repo/crates/zeroclaw-gateway)' >> "$axum"
expect_fail "axum through a crate that is not a listener exception"

reset_fixtures
echo '0000000000002000 T zeroclaw_gateway::run_gateway' >> "$symbols"
expect_fail "gateway symbols in the binary"

reset_fixtures
: > "$symbols"
expect_fail "a stripped binary cannot prove anything"

# `run` mode, with stub `cargo` and `nm` first on PATH. It must exit 0 when every
# check passes: an EXIT trap that read a function-local variable once turned a
# passing run into exit 1 after "CLI-only ratchet passed.".
stub_dir="${fixture_dir}/bin"
mkdir -p "$stub_dir"
cat > "${stub_dir}/cargo" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  run\ *) echo "agent-runtime,schema-export" ;;
  *" -i axum "*) printf '%s\n' 'axum v0.8.9' 'zeroclaw-channels v0.8.6 (/repo/crates/zeroclaw-channels)' ;;
  tree\ *)
    printf '%s\n' 'axum v0.8.9' 'zeroclaw v0.8.6 (/repo)' 'zeroclaw-channels v0.8.6 (/repo/crates/zeroclaw-channels)'
    if [ -n "${RATCHET_STUB_GATEWAY:-}" ]; then
      echo 'zeroclaw-gateway v0.8.6 (/repo/crates/zeroclaw-gateway)'
    fi
    ;;
  *) exit 2 ;;
esac
STUB
cat > "${stub_dir}/nm" <<'STUB'
#!/usr/bin/env bash
echo '0000000000001000 T zeroclaw_runtime::daemon::run'
STUB
chmod +x "${stub_dir}/cargo" "${stub_dir}/nm"

if ! PATH="${stub_dir}:${PATH}" bash "$ratchet" run x86_64-unknown-linux-gnu /dev/null >/dev/null; then
  echo "expected run mode to pass when every check passes" >&2
  exit 1
fi
if RATCHET_STUB_GATEWAY=1 PATH="${stub_dir}:${PATH}" bash "$ratchet" run x86_64-unknown-linux-gnu /dev/null >/dev/null 2>&1; then
  echo "expected run mode to fail: the gateway crate is linked" >&2
  exit 1
fi

echo "cli_only_ratchet tests passed."
