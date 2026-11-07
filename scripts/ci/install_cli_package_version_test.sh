#!/usr/bin/env bash
# Tests install.sh cli_package_version and offline installer output.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALL_SH="$ROOT/install.sh"

if [[ ! -f "$INSTALL_SH" ]]; then
  echo "FAIL: install.sh not found at $INSTALL_SH"
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

if ! awk '
  /^cli_package_version\(\) \{/ { in_func = 1 }
  in_func { print }
  in_func && /^}$/ { found = 1; exit }
  END { if (!found) exit 1 }
' "$INSTALL_SH" >"$tmp"; then
  echo "FAIL: could not extract cli_package_version() from install.sh"
  exit 1
fi

bash -n "$tmp"

# shellcheck disable=SC1090
source "$tmp"

pass=0
fail=0
failures=()

assert_version() {
  local input="$1" expected="$2" actual
  actual="$(cli_package_version "$input" || true)"
  if [[ "$actual" == "$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("input='$input' got='$actual' want='$expected'")
  fi
}

assert_version "zeroclaw 0.8.7 (99bd353)" "0.8.7"
assert_version "zeroclaw 0.8.7 (v0.8.7-33-ge6e4b0486)" "0.8.7"
assert_version "zeroclaw 0.8.5" "0.8.5"

if [[ "$fail" -ne 0 ]]; then
  echo "FAIL: cli_package_version ($fail failed, $pass passed)"
  for f in "${failures[@]}"; do
    echo "  - $f"
  done
  exit 1
fi

echo "PASS: cli_package_version ($pass cases)"

# Exercise the production entrypoint with only synthetic binaries and archives.
# Use an empty environment, isolated HOME and TMPDIR, and a tool allowlist
# with stubs for downloads, compilation, and installed binary commands.
fixture=$(mktemp -d)
trap 'rm -f "$tmp"; rm -rf "$fixture"' EXIT
mkdir -p "$fixture/tools" "$fixture/archive" "$fixture/source" "$fixture/temp"
for tool in awk basename cat chmod cp cut dirname du grep gzip head install mkdir mktemp paste rm sed sha256sum sort tar tr; do
  ln -s "$(command -v "$tool")" "$fixture/tools/$tool"
done
cat >"$fixture/tools/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; *) exit 1 ;; esac
EOF
cat >"$fixture/tools/ldd" <<'EOF'
#!/bin/sh
echo 'ldd (GNU libc) 2.39'
EOF
cat >"$fixture/tools/rustc" <<'EOF'
#!/bin/sh
[ "$*" = --version ] || exit 1
echo 'rustc 1.95.0'
EOF
cat >"$fixture/archive/zeroclaw" <<'EOF'
#!/bin/sh
case "$*" in
  --version) printf '%s\n' "$FIXTURE_VERSION" ;;
  --help) echo agent ;;
  'service stop'|'service uninstall') : ;;
  *) echo "Unexpected fixture binary invocation: $*" >&2; exit 1 ;;
esac
EOF
cat >"$fixture/tools/cargo" <<'EOF'
#!/bin/sh
[ "$*" = 'install --path . --locked --force --no-default-features' ] || exit 1
mkdir -p "$CARGO_HOME/bin"
cp "$FIXTURE_ROOT/archive/zeroclaw" "$CARGO_HOME/bin/zeroclaw"
EOF
cat >"$fixture/tools/curl" <<'EOF'
#!/bin/sh
case "$*" in
  '-fsSL https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest')
    echo '{"tag_name": "v0.8.7"}' ;;
  '-fsSL https://github.com/zeroclaw-labs/zeroclaw/releases/download/v0.8.7/SHA256SUMS -o '*)
    cp "$FIXTURE_ROOT/SHA256SUMS" "$4" ;;
  '-fSL --progress-bar https://github.com/zeroclaw-labs/zeroclaw/releases/download/v0.8.7/zeroclaw-x86_64-unknown-linux-gnu.tar.gz -o '*)
    cp "$FIXTURE_ROOT/release.tar.gz" "$5" ;;
  *) echo "Unexpected fixture download: $*" >&2; exit 1 ;;
esac
EOF
chmod +x "$fixture/tools/"{uname,ldd,rustc,cargo,curl} "$fixture/archive/zeroclaw"
cat >"$fixture/source/Cargo.toml" <<'EOF'
[package]
name = "zeroclaw"
version = "0.8.7"
rust-version = "1.88.0"
edition = "2024"
[features]
default = []
EOF
tar -czf "$fixture/release.tar.gz" -C "$fixture/archive" zeroclaw
printf '%s  zeroclaw-x86_64-unknown-linux-gnu.tar.gz\n' \
  "$(sha256sum "$fixture/release.tar.gz" | awk '{print $1}')" >"$fixture/SHA256SUMS"

assert_output() {
  if ! grep -F -- "$2" "$1"; then
    cat "$1" >&2
    echo "FAIL: missing installer output: $2" >&2
    exit 1
  fi
}

case_count=0
for version_line in 'zeroclaw 0.8.7 (99bd353)' 'zeroclaw 0.8.7 (v0.8.7-33-ge6e4b0486)' 'zeroclaw 0.8.5'; do
  package_version=$(printf '%s\n' "$version_line" | awk '{print $2}')
  old_version_line=${version_line/0.8.7/0.8.4}
  old_version_line=${old_version_line/0.8.5/0.8.3}
  old_package_version=$(printf '%s\n' "$old_version_line" | awk '{print $2}')
  for mode in prebuilt source uninstall; do
    case_count=$((case_count + 1))
    home="$fixture/home-$case_count"
    mkdir -p "$home/.cargo/bin" "$home/old-bin"
    sed 's/FIXTURE_VERSION/FIXTURE_OLD_VERSION/g' "$fixture/archive/zeroclaw" >"$home/old-bin/zeroclaw"
    chmod +x "$home/old-bin/zeroclaw"
    args=("--$mode" --no-modify-path --skip-quickstart)
    if [[ "$mode" == source ]]; then
      args+=(--minimal --apps none)
    elif [[ "$mode" == uninstall ]]; then
      cp "$fixture/archive/zeroclaw" "$home/.cargo/bin/zeroclaw"
    fi
    output="$fixture/output-$case_count"
    echo "Fixture: $mode, LC_ALL=C, non-TTY, input=$version_line, existing=$old_version_line"
    (
      cd "$fixture/source"
      env -i HOME="$home" SHELL=/bin/sh LC_ALL=C TMPDIR="$fixture/temp" \
        PATH="$home/old-bin:$fixture/tools" FIXTURE_ROOT="$fixture" \
        FIXTURE_VERSION="$version_line" FIXTURE_OLD_VERSION="$old_version_line" /bin/sh "$INSTALL_SH" "${args[@]}" </dev/null
    ) >"$output" 2>&1 || { cat "$output" >&2; exit 1; }
    if [[ "$mode" == uninstall ]]; then
      assert_output "$output" "Another zeroclaw found at $home/old-bin/zeroclaw (v$old_package_version)"
      [[ ! -e "$home/.cargo/bin/zeroclaw" ]]
    else
      if [[ "$mode" == prebuilt ]]; then
        assert_output "$output" "Checksum verified"
        if grep -Fq "source install" "$output"; then
          echo "FAIL: prebuilt fixture fell back to source" >&2
          exit 1
        fi
      fi
      assert_output "$output" "Installed: $home/.cargo/bin/zeroclaw (v$package_version,"
      if [[ "$mode" == source ]]; then
        assert_output "$output" "zeroclaw found at $home/old-bin/zeroclaw (v$old_package_version)"
        assert_output "$output" "zeroclaw in your PATH is $home/old-bin/zeroclaw (v$old_package_version)"
        assert_output "$output" "It will shadow the v$package_version binary you just installed"
      fi
    fi
    [[ ! -e "$home/.profile" && ! -e "$home/.bashrc" ]]
  done
done
echo "PASS: installer entrypoint ($case_count offline cases; all five version display sites)"
