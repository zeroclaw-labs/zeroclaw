#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd || pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd || pwd)"

usage() {
  cat <<'EOF'
Usage:
  scripts/test-local-installer.sh [mock|real]

Modes:
  mock  Safe default. Does NOT install anything.
        Verifies local install.sh forwards one-click defaults:
        --install-system-deps --install-rust --interactive-onboard
  real  Runs local install.sh from this repository (actual install flow).

Examples:
  ./scripts/test-local-installer.sh
  ./scripts/test-local-installer.sh mock
  ./scripts/test-local-installer.sh real
EOF
}

mode="${1:-mock}"

case "$mode" in
  -h|--help)
    usage
    exit 0
    ;;
  mock)
    demo_dir="$(mktemp -d /tmp/zc-local-installer-test.XXXXXX)"
    trap 'rm -rf "$demo_dir"' EXIT

    cp "$ROOT_DIR/install.sh" "$demo_dir/install.sh"
    chmod +x "$demo_dir/install.sh"

    cat > "$demo_dir/mock-bootstrap.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "[mock-bootstrap] received args: $*"
EOF
    chmod +x "$demo_dir/mock-bootstrap.sh"

    echo "==> Running mock local installer test"
    echo "    source: $ROOT_DIR/install.sh"

    if command -v script >/dev/null 2>&1; then
      output="$(
        cd "$demo_dir" && script -q /dev/null bash -lc \
          "ZEROCLAW_BOOTSTRAP_URL=file://$demo_dir/mock-bootstrap.sh bash ./install.sh"
      )"
    else
      output="$(
        cd "$demo_dir" && ZEROCLAW_BOOTSTRAP_URL="file://$demo_dir/mock-bootstrap.sh" bash ./install.sh
      )"
    fi

    echo "$output"

    expected="--install-system-deps --install-rust --interactive-onboard"
    if [[ "$output" != *"$expected"* ]]; then
      echo "error: expected forwarded args not found: $expected" >&2
      exit 1
    fi

    echo "✅ mock test passed"
    ;;
  real)
    echo "==> Running REAL local installer flow from repo"
    echo "    source: $ROOT_DIR/install.sh"
    exec bash "$ROOT_DIR/install.sh"
    ;;
  *)
    echo "error: unknown mode '$mode'" >&2
    echo
    usage
    exit 1
    ;;
esac
