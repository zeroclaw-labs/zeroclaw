#!/bin/bash
# Quick smoke test for Telegram integration
# Run this before committing code changes

set -e

echo "🔥 Quick Telegram Smoke Test"
echo ""

# Test 1: Compile check
echo -n "1. Compiling... "
cargo build --release --quiet 2>&1 && echo "✓" || { echo "✗ FAILED"; exit 1; }

# Test 2: Unit tests
echo -n "2. Running tests... "
cargo test telegram_split --lib --quiet 2>&1 && echo "✓" || { echo "✗ FAILED"; exit 1; }

# Test 3: Health check
echo -n "3. Health check... "
timeout 7 target/release/zeroclaw channel doctor &>/dev/null && echo "✓" || echo "⚠ (configure bot first)"

# Test 4: File checks
echo -n "4. Code structure... "
grep -q "TELEGRAM_MAX_MESSAGE_LENGTH" src/channels/telegram.rs && \
grep -q "split_message_for_telegram" src/channels/telegram.rs && \
grep -q "tokio::time::timeout" src/channels/telegram.rs && \
echo "✓" || { echo "✗ FAILED"; exit 1; }

echo ""
echo "✅ Quick tests passed! Run ./test_telegram_integration.sh for full suite."
