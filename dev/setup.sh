#!/bin/bash
set -euo pipefail

# ── Colors ────────────────────────────────────────────────────────
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── Detect project root ──────────────────────────────────────────
if [ -f "Cargo.toml" ] && grep -q 'name = "zeroclaw"' Cargo.toml 2>/dev/null; then
    ROOT_DIR="$(pwd)"
elif [ -f "../Cargo.toml" ] && grep -q 'name = "zeroclaw"' ../Cargo.toml 2>/dev/null; then
    ROOT_DIR="$(cd .. && pwd)"
else
    echo -e "${RED}❌ Error: Run this script from the project root or dev/ directory.${NC}"
    exit 1
fi

cd "$ROOT_DIR"

echo -e "${CYAN}${BOLD}"
echo "  ╔══════════════════════════════════════╗"
echo "  ║   ZeroClaw Developer Setup           ║"
echo "  ╚══════════════════════════════════════╝"
echo -e "${NC}"

ERRORS=0

# ── 1. Check rustup ──────────────────────────────────────────────
echo -e "${BOLD}[1/5]${NC} Checking Rust toolchain..."

if ! command -v rustup &>/dev/null; then
    echo -e "${RED}  ❌ rustup not found.${NC}"
    echo -e "  Install it with: ${CYAN}curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh${NC}"
    exit 1
fi

# Read expected Rust version from rust-toolchain.toml
EXPECTED_RUST=""
if [ -f "rust-toolchain.toml" ]; then
    EXPECTED_RUST=$(grep -oP 'channel\s*=\s*"\K[^"]+' rust-toolchain.toml 2>/dev/null || \
                    sed -n 's/.*channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml)
fi

# rustup will auto-install from rust-toolchain.toml, just verify it works
RUST_VERSION=$(rustc --version 2>/dev/null || true)
if [ -n "$RUST_VERSION" ]; then
    echo -e "  ${GREEN}✅${NC} $RUST_VERSION"
    if [ -n "$EXPECTED_RUST" ]; then
        echo -e "  ${GREEN}✅${NC} rust-toolchain.toml specifies ${CYAN}$EXPECTED_RUST${NC}"
    fi
else
    echo -e "${YELLOW}  ⚠️  Rust compiler not available yet. Running 'rustup show' to trigger install...${NC}"
    rustup show
    RUST_VERSION=$(rustc --version 2>/dev/null || true)
    if [ -n "$RUST_VERSION" ]; then
        echo -e "  ${GREEN}✅${NC} $RUST_VERSION"
    else
        echo -e "${RED}  ❌ Failed to install Rust toolchain.${NC}"
        exit 1
    fi
fi

# ── 2. Configure git hooks ───────────────────────────────────────
echo ""
echo -e "${BOLD}[2/5]${NC} Configuring git hooks..."

if [ -d ".githooks" ]; then
    git config core.hooksPath .githooks
    echo -e "  ${GREEN}✅${NC} Pre-push hook enabled (fmt + clippy + test)"
else
    echo -e "  ${YELLOW}⚠️  .githooks/ directory not found, skipping${NC}"
    ERRORS=$((ERRORS + 1))
fi

# ── 3. Copy .env.example ───────────────────────────────────────
echo ""
echo -e "${BOLD}[3/5]${NC} Checking environment file..."

if [ -f ".env" ]; then
    echo -e "  ${GREEN}✅${NC} .env already exists (not overwriting)"
elif [ -f ".env.example" ]; then
    cp .env.example .env
    echo -e "  ${GREEN}✅${NC} Copied .env.example → .env"
else
    echo -e "  ${YELLOW}⚠️  No .env.example found, skipping${NC}"
    ERRORS=$((ERRORS + 1))
fi

# ── 4. Build smoke test ──────────────────────────────────────────
echo ""
echo -e "${BOLD}[4/5]${NC} Running build smoke test (cargo check)..."

if cargo check --locked 2>&1; then
    echo -e "  ${GREEN}✅${NC} Build check passed"
else
    echo -e "  ${YELLOW}⚠️  Build check failed (see errors above)${NC}"
    echo -e "  ${YELLOW}     Try: cargo check  (without --locked) or cargo update${NC}"
    ERRORS=$((ERRORS + 1))
fi

# ── 5. Verify project structure ──────────────────────────────────
echo ""
echo -e "${BOLD}[5/5]${NC} Verifying project structure..."

MISSING=""
for f in Cargo.toml Cargo.lock rust-toolchain.toml src/lib.rs src/main.rs .githooks/pre-push; do
    if [ ! -f "$f" ]; then
        MISSING="$MISSING $f"
    fi
done

if [ -z "$MISSING" ]; then
    echo -e "  ${GREEN}✅${NC} All expected files present"
else
    echo -e "  ${YELLOW}⚠️  Missing files:${MISSING}${NC}"
    ERRORS=$((ERRORS + 1))
fi

# ── Summary ──────────────────────────────────────────────────────
echo ""
echo -e "${CYAN}──────────────────────────────────────────${NC}"

if [ "$ERRORS" -eq 0 ]; then
    echo -e "${GREEN}${BOLD}  ✅ Setup complete! You're ready to contribute.${NC}"
else
    echo -e "${YELLOW}${BOLD}  ⚠️  Setup completed with $ERRORS warning(s).${NC}"
fi

echo ""
echo -e "  ${BOLD}Next steps:${NC}"
echo -e "    ${CYAN}cargo build${NC}              Build the project"
echo -e "    ${CYAN}cargo test --locked${NC}      Run the test suite"
echo -e "    ${CYAN}cargo clippy${NC}             Lint the code"
echo -e "    ${CYAN}zeroclaw onboard${NC}         Configure runtime settings"
echo ""
echo -e "  ${BOLD}Useful commands:${NC}"
echo -e "    ${CYAN}./dev/cli.sh up${NC}          Start Docker dev environment"
echo -e "    ${CYAN}./scripts/ci/rust_quality_gate.sh${NC}  Run full CI quality checks"
echo ""
