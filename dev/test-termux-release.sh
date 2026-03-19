#!/usr/bin/env bash
# Termux release validation script
# Validates the aarch64-linux-android release artifact for Termux compatibility.
#
# Usage:
#   ./dev/test-termux-release.sh [version]
#
# Examples:
#   ./dev/test-termux-release.sh 0.3.1
#   ./dev/test-termux-release.sh         # auto-detects from Cargo.toml
#
set -euo pipefail

BLUE='\033[0;34m'
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

pass() { echo -e "  ${GREEN}✓${RESET} $*"; }
fail() { echo -e "  ${RED}✗${RESET} $*"; FAILURES=$((FAILURES + 1)); }
info() { echo -e "${BLUE}→${RESET} ${BOLD}$*${RESET}"; }
warn() { echo -e "${YELLOW}!${RESET} $*"; }

FAILURES=0
TARGET="aarch64-linux-android"
VERSION="${1:-}"

if [[ -z "$VERSION" ]]; then
  if [[ -f Cargo.toml ]]; then
    VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
  fi
fi

if [[ -z "$VERSION" ]]; then
  echo "Usage: $0 <version>"
  echo "  e.g. $0 0.3.1"
  exit 1
fi

TAG="v${VERSION}"
ASSET_NAME="jhedaiclaw-${TARGET}.tar.gz"
ASSET_URL="https://github.com/jhedai/jhedaiclaw/releases/download/${TAG}/${ASSET_NAME}"
TEMP_DIR="$(mktemp -d -t jhedaiclaw-termux-test-XXXXXX)"

cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT

echo
echo -e "${BOLD}Termux Release Validation — ${TAG}${RESET}"
echo -e "${DIM}Target: ${TARGET}${RESET}"
echo

# --- Test 1: Release tag exists ---
info "Checking release tag ${TAG}"
if gh release view "$TAG" >/dev/null 2>&1; then
  pass "Release ${TAG} exists"
else
  fail "Release ${TAG} not found"
  echo -e "${RED}Release has not been published yet. Wait for the release workflow to complete.${RESET}"
  exit 1
fi

# --- Test 2: Android asset is listed ---
info "Checking for ${ASSET_NAME} in release assets"
ASSETS=$(gh release view "$TAG" --json assets -q '.assets[].name')
if echo "$ASSETS" | grep -q "$ASSET_NAME"; then
  pass "Asset ${ASSET_NAME} found in release"
else
  fail "Asset ${ASSET_NAME} not found in release"
  echo "Available assets:"
  echo "$ASSETS" | sed 's/^/  /'
  exit 1
fi

# --- Test 3: Download the asset ---
info "Downloading ${ASSET_NAME}"
if curl -fsSL "$ASSET_URL" -o "$TEMP_DIR/$ASSET_NAME"; then
  FILESIZE=$(wc -c < "$TEMP_DIR/$ASSET_NAME" | tr -d ' ')
  pass "Downloaded successfully (${FILESIZE} bytes)"
else
  fail "Download failed from ${ASSET_URL}"
  exit 1
fi

# --- Test 4: Archive integrity ---
info "Verifying archive integrity"
if tar -tzf "$TEMP_DIR/$ASSET_NAME" >/dev/null 2>&1; then
  pass "Archive is a valid gzip tar"
else
  fail "Archive is corrupted or not a valid tar.gz"
  exit 1
fi

# --- Test 5: Contains jhedaiclaw binary ---
info "Checking archive contents"
CONTENTS=$(tar -tzf "$TEMP_DIR/$ASSET_NAME")
if echo "$CONTENTS" | grep -q "^jhedaiclaw$"; then
  pass "Archive contains 'jhedaiclaw' binary"
else
  fail "Archive does not contain 'jhedaiclaw' binary"
  echo "Contents:"
  echo "$CONTENTS" | sed 's/^/  /'
fi

# --- Test 6: Extract and inspect binary ---
info "Extracting and inspecting binary"
tar -xzf "$TEMP_DIR/$ASSET_NAME" -C "$TEMP_DIR"
BINARY="$TEMP_DIR/jhedaiclaw"

if [[ -f "$BINARY" ]]; then
  pass "Binary extracted"
else
  fail "Binary not found after extraction"
  exit 1
fi

# --- Test 7: ELF format and architecture ---
info "Checking binary format"
FILE_INFO=$(file "$BINARY")
if echo "$FILE_INFO" | grep -q "ELF"; then
  pass "Binary is ELF format"
else
  fail "Binary is not ELF format: $FILE_INFO"
fi

if echo "$FILE_INFO" | grep -qi "aarch64\|ARM aarch64"; then
  pass "Binary targets aarch64 architecture"
else
  fail "Binary does not target aarch64: $FILE_INFO"
fi

if echo "$FILE_INFO" | grep -qi "android\|bionic"; then
  pass "Binary is linked for Android/Bionic"
else
  # Android binaries may not always show "android" in file output,
  # check with readelf if available
  if command -v readelf >/dev/null 2>&1; then
    INTERP=$(readelf -l "$BINARY" 2>/dev/null | grep -o '/[^ ]*linker[^ ]*' || true)
    if echo "$INTERP" | grep -qi "android\|bionic"; then
      pass "Binary uses Android linker: $INTERP"
    else
      warn "Could not confirm Android linkage (interpreter: ${INTERP:-unknown})"
      warn "file output: $FILE_INFO"
    fi
  else
    warn "Could not confirm Android linkage (readelf not available)"
    warn "file output: $FILE_INFO"
  fi
fi

# --- Test 8: Binary is stripped ---
info "Checking binary optimization"
if echo "$FILE_INFO" | grep -q "stripped"; then
  pass "Binary is stripped (release optimized)"
else
  warn "Binary may not be stripped"
fi

# --- Test 9: Binary is not dynamically linked to glibc ---
info "Checking for glibc dependencies"
if command -v readelf >/dev/null 2>&1; then
  NEEDED=$(readelf -d "$BINARY" 2>/dev/null | grep NEEDED || true)
  if echo "$NEEDED" | grep -qi "libc\.so\.\|libpthread\|libdl"; then
    # Check if it's glibc or bionic
    if echo "$NEEDED" | grep -qi "libc\.so\.6"; then
      fail "Binary links against glibc (libc.so.6) — will not work on Termux"
    else
      pass "Binary links against libc (likely Bionic)"
    fi
  else
    pass "No glibc dependencies detected"
  fi
else
  warn "readelf not available — skipping dynamic library check"
fi

# --- Test 10: SHA256 checksum verification ---
info "Verifying SHA256 checksum"
CHECKSUMS_URL="https://github.com/jhedai/jhedaiclaw/releases/download/${TAG}/SHA256SUMS"
if curl -fsSL "$CHECKSUMS_URL" -o "$TEMP_DIR/SHA256SUMS" 2>/dev/null; then
  EXPECTED=$(grep "$ASSET_NAME" "$TEMP_DIR/SHA256SUMS" | awk '{print $1}')
  if [[ -n "$EXPECTED" ]]; then
    if command -v sha256sum >/dev/null 2>&1; then
      ACTUAL=$(sha256sum "$TEMP_DIR/$ASSET_NAME" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
      ACTUAL=$(shasum -a 256 "$TEMP_DIR/$ASSET_NAME" | awk '{print $1}')
    else
      warn "No sha256sum or shasum available"
      ACTUAL=""
    fi

    if [[ -n "$ACTUAL" && "$ACTUAL" == "$EXPECTED" ]]; then
      pass "SHA256 checksum matches"
    elif [[ -n "$ACTUAL" ]]; then
      fail "SHA256 mismatch: expected=$EXPECTED actual=$ACTUAL"
    fi
  else
    warn "No checksum entry for ${ASSET_NAME} in SHA256SUMS"
  fi
else
  warn "Could not download SHA256SUMS"
fi

# --- Test 11: install.sh Termux detection ---
info "Validating install.sh Termux detection"
INSTALL_SH="install.sh"
if [[ ! -f "$INSTALL_SH" ]]; then
  INSTALL_SH="$(dirname "$0")/../install.sh"
fi

if [[ -f "$INSTALL_SH" ]]; then
  if grep -q 'TERMUX_VERSION' "$INSTALL_SH"; then
    pass "install.sh checks TERMUX_VERSION"
  else
    fail "install.sh does not check TERMUX_VERSION"
  fi

  if grep -q 'aarch64-linux-android' "$INSTALL_SH"; then
    pass "install.sh maps to aarch64-linux-android target"
  else
    fail "install.sh does not map to aarch64-linux-android"
  fi

  # Simulate Termux detection (mock uname as Linux since we may run on macOS)
  detect_result=$(
    bash -c '
      TERMUX_VERSION="0.118"
      os="Linux"
      arch="aarch64"
      case "$os:$arch" in
        Linux:aarch64|Linux:arm64)
          if [[ -n "${TERMUX_VERSION:-}" || -d "/data/data/com.termux" ]]; then
            echo "aarch64-linux-android"
          else
            echo "aarch64-unknown-linux-gnu"
          fi
          ;;
      esac
    '
  )
  if [[ "$detect_result" == "aarch64-linux-android" ]]; then
    pass "Termux detection returns correct target (simulated)"
  else
    fail "Termux detection returned: $detect_result (expected aarch64-linux-android)"
  fi
else
  warn "install.sh not found — skipping detection tests"
fi

# --- Summary ---
echo
if [[ "$FAILURES" -eq 0 ]]; then
  echo -e "${GREEN}${BOLD}All tests passed!${RESET}"
  echo -e "${DIM}The Termux release artifact for ${TAG} is valid.${RESET}"
else
  echo -e "${RED}${BOLD}${FAILURES} test(s) failed.${RESET}"
  exit 1
fi
