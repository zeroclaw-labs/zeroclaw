#!/usr/bin/env bash
set -euo pipefail

# Installs mold linker and cargo-nextest for fast dev builds.
# Run once: ./dev/setup-fast-builds.sh

if ! command -v mold &>/dev/null; then
  echo "Installing mold linker..."
  if command -v apt-get &>/dev/null; then
    sudo apt-get install -y mold
  elif command -v brew &>/dev/null; then
    brew install mold
  elif command -v dnf &>/dev/null; then
    sudo dnf install -y mold
  elif command -v pacman &>/dev/null; then
    sudo pacman -S --noconfirm mold
  else
    echo "Could not auto-install mold. See https://github.com/rui314/mold"
    exit 1
  fi
fi

if ! command -v clang &>/dev/null; then
  echo "Installing clang (needed as linker driver for mold)..."
  if command -v apt-get &>/dev/null; then
    sudo apt-get install -y clang
  elif command -v brew &>/dev/null; then
    echo "clang ships with Xcode CLI tools on macOS."
  elif command -v dnf &>/dev/null; then
    sudo dnf install -y clang
  fi
fi

if ! cargo nextest --version &>/dev/null 2>&1; then
  echo "Installing cargo-nextest..."
  cargo install cargo-nextest --locked
fi

echo ""
echo "Done!"
echo "  mold:    $(mold --version 2>/dev/null)"
echo "  clang:   $(clang --version 2>/dev/null | head -1)"
echo "  nextest: $(cargo nextest --version 2>/dev/null)"
