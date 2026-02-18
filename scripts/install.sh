#!/usr/bin/env bash
# install.sh — Build and install ZeroClaw from source.
# Usage:
#   curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
#   # or
#   bash scripts/install.sh

set -euo pipefail

# --- Logging --------------------------------------------------------------

info()  { printf ">>> %s\n" "$*"; }
warn()  { printf ">>> [WARN] %s\n" "$*"; }
error() { printf ">>> [ERROR] %s\n" "$*" >&2; }
die()   { error "$@"; exit 1; }

# --- Step 1: Detect OS and install system dependencies --------------------

install_system_deps() {
    info "Detecting operating system..."

    local os
    os="$(uname -s)"

    case "${os}" in
        Linux)
            install_linux_deps
            ;;
        Darwin)
            install_macos_deps
            ;;
        *)
            die "Unsupported operating system: ${os}. Only Linux and macOS are supported."
            ;;
    esac
}

install_linux_deps() {
    if command -v apt-get >/dev/null 2>&1; then
        info "Detected Debian/Ubuntu — installing build-essential, pkg-config, git..."
        sudo apt-get update -qq
        sudo apt-get install -y build-essential pkg-config git
    elif command -v dnf >/dev/null 2>&1; then
        info "Detected Fedora/RHEL — installing Development Tools, pkg-config, git..."
        sudo dnf groupinstall -y "Development Tools"
        sudo dnf install -y pkg-config git
    else
        die "Unsupported Linux distribution. Please install a C compiler, pkg-config, and git manually, then re-run this script."
    fi
}

install_macos_deps() {
    if ! xcode-select -p >/dev/null 2>&1; then
        info "Installing Xcode Command Line Tools..."
        xcode-select --install
        warn "A dialog may have appeared. Please complete the Xcode CLT installation, then re-run this script."
        exit 0
    else
        info "Xcode Command Line Tools already installed."
    fi

    if ! command -v git >/dev/null 2>&1; then
        die "git not found. Please install git (e.g. via Homebrew) and re-run this script."
    fi
}

# --- Step 2: Install Rust -------------------------------------------------

install_rust() {
    if command -v rustc >/dev/null 2>&1; then
        info "Rust already installed ($(rustc --version))."
    else
        info "Installing Rust via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        # shellcheck source=/dev/null
        source "${HOME}/.cargo/env"
        info "Rust installed ($(rustc --version))."
    fi
}

# --- Step 3: Clone repository ---------------------------------------------

CLONE_DIR="/tmp/zeroclaw-install"

clone_repo() {
    if [ -d "${CLONE_DIR}" ]; then
        info "Removing previous clone at ${CLONE_DIR}..."
        rm -rf "${CLONE_DIR}"
    fi

    info "Cloning ZeroClaw repository..."
    git clone --depth 1 https://github.com/zeroclaw-labs/zeroclaw.git "${CLONE_DIR}"
}

# --- Step 4: Build and install --------------------------------------------

build_and_install() {
    info "Building ZeroClaw (release mode)..."
    cargo build --release --locked --manifest-path "${CLONE_DIR}/Cargo.toml"

    info "Installing ZeroClaw binary..."
    cargo install --path "${CLONE_DIR}" --force --locked
}

# --- Step 5: Cleanup ------------------------------------------------------

cleanup() {
    info "Cleaning up build directory..."
    rm -rf "${CLONE_DIR}"
}

# --- Step 6: Success message ----------------------------------------------

print_success() {
    echo ""
    info "ZeroClaw installed successfully!"
    echo ""
    echo "  To use zeroclaw in your current shell, run:"
    echo ""
    echo "    source \"\${HOME}/.cargo/env\""
    echo ""
    echo "  To make it permanent, add the line above to your shell profile"
    echo "  (~/.bashrc, ~/.zshrc, etc.)."
    echo ""
    echo "  Then get started:"
    echo ""
    echo "    zeroclaw onboard"
    echo ""
}

# --- Main -----------------------------------------------------------------

main() {
    echo ""
    info "ZeroClaw — Source Install"
    echo ""

    install_system_deps
    echo ""
    install_rust
    echo ""
    clone_repo
    echo ""
    build_and_install
    echo ""
    cleanup
    echo ""
    print_success
}

main