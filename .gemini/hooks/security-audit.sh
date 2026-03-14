#!/bin/bash

# ZeroClaw Security Audit Hook
# Performs basic secret scanning and dependency auditing.

set -e

echo "Running ZeroClaw Security Audit Hook..."

# 1. Simple Secret Scanning (check for common API key patterns)
# Looking for things like sk-..., ghp_..., etc.
SECRETS_FOUND=$(grep -rE "sk-[a-zA-Z0-9]{32}|ghp_[a-zA-Z0-9]{36}" . --exclude-dir=".git" --exclude-dir="target" --exclude=".gemini/hooks/security-audit.sh" || true)

if [ -n "$SECRETS_FOUND" ]; then
    echo "Error: Potential secrets found in the following files:"
    echo "$SECRETS_FOUND"
    exit 1
fi

echo "No obvious secrets found."

# 2. Dependency Audit (Cargo)
if [ -f "Cargo.toml" ]; then
    echo "Auditing Rust dependencies..."
    # Note: Requires cargo-audit to be installed for full functionality.
    # We'll just check if it's there, otherwise skip to avoid breaking the hook.
    if command -v cargo-audit >/dev/null 2>&1; then
        cargo audit
    else
        echo "Warning: cargo-audit not found. Skipping dependency audit."
    fi
fi

echo "Security audit passed!"
exit 0
