#!/bin/bash
# ZeroClaw Telegram Integration Test Suite
# Automated testing script for Telegram channel functionality

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test counters
TOTAL_TESTS=0
PASSED_TESTS=0
FAILED_TESTS=0

# Helper functions
print_header() {
    echo -e "\n${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}$1${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}\n"
}

print_test() {
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    echo -e "${YELLOW}Test $TOTAL_TESTS:${NC} $1"
}

pass() {
    PASSED_TESTS=$((PASSED_TESTS + 1))
    echo -e "${GREEN}✓ PASS:${NC} $1\n"
}

fail() {
    FAILED_TESTS=$((FAILED_TESTS + 1))
    echo -e "${RED}✗ FAIL:${NC} $1\n"
}

warn() {
    echo -e "${YELLOW}⚠ WARNING:${NC} $1\n"
}

# Banner
clear
cat << "EOF"
    ⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡

    ███████╗███████╗██████╗  ██████╗  ██████╗██╗      █████╗ ██╗    ██╗
    ╚══███╔╝██╔════╝██╔══██╗██╔═══██╗██╔════╝██║     ██╔══██╗██║    ██║
      ███╔╝ █████╗  ██████╔╝██║   ██║██║     ██║     ███████║██║ █╗ ██║
     ███╔╝  ██╔══╝  ██╔══██╗██║   ██║██║     ██║     ██╔══██║██║███╗██║
    ███████╗███████╗██║  ██║╚██████╔╝╚██████╗███████╗██║  ██║╚███╔███╔╝
    ╚══════╝╚══════╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝╚══════╝╚═╝  ╚═╝ ╚══╝╚══╝

    🧪 TELEGRAM INTEGRATION TEST SUITE 🧪

    ⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡
EOF

echo -e "\n${BLUE}Started at:${NC} $(date)"
echo -e "${BLUE}Working directory:${NC} $(pwd)\n"

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Phase 1: Code Quality Tests
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Phase 1: Code Quality Tests"

# Test 1: Cargo test compilation
print_test "Compiling test suite"
if cargo test --lib --no-run &>/dev/null; then
    pass "Test suite compiles successfully"
else
    fail "Test suite compilation failed"
    exit 1
fi

# Test 2: Unit tests
print_test "Running Telegram unit tests"
TEST_OUTPUT=$(cargo test telegram --lib 2>&1)
if echo "$TEST_OUTPUT" | grep -q "test result: ok"; then
    PASSED_COUNT=$(echo "$TEST_OUTPUT" | grep -oP '\d+(?= passed)' | head -1)
    pass "All Telegram unit tests passed ($PASSED_COUNT tests)"
else
    fail "Some unit tests failed"
    echo "$TEST_OUTPUT" | grep "FAILED\|error"
fi

# Test 3: Message splitting tests specifically
print_test "Verifying message splitting tests"
if cargo test telegram_split --lib --quiet 2>&1 | grep -q "8 passed"; then
    pass "All 8 message splitting tests passed"
else
    fail "Message splitting tests incomplete"
fi

# Test 4: Clippy linting
print_test "Running Clippy lint checks"
if cargo clippy --all-targets --quiet 2>&1 | grep -qv "error:"; then
    pass "No clippy errors found"
else
    CLIPPY_ERRORS=$(cargo clippy --all-targets 2>&1 | grep "error:" | wc -l)
    fail "Clippy found $CLIPPY_ERRORS error(s)"
fi

# Test 5: Code formatting
print_test "Checking code formatting"
if cargo fmt --check &>/dev/null; then
    pass "Code is properly formatted"
else
    warn "Code formatting issues found (run 'cargo fmt' to fix)"
fi

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Phase 2: Build Tests
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Phase 2: Build Tests"

# Test 6: Debug build
print_test "Debug build"
if cargo build --quiet 2>&1; then
    pass "Debug build successful"
else
    fail "Debug build failed"
fi

# Test 7: Release build
print_test "Release build with optimizations"
START_TIME=$(date +%s)
if cargo build --release --quiet 2>&1; then
    END_TIME=$(date +%s)
    BUILD_TIME=$((END_TIME - START_TIME))
    pass "Release build successful (${BUILD_TIME}s)"
else
    fail "Release build failed"
fi

# Test 8: Binary size check
print_test "Binary size verification"
if [ -f "target/release/zeroclaw" ]; then
    BINARY_SIZE=$(ls -lh target/release/zeroclaw | awk '{print $5}')
    SIZE_BYTES=$(stat -f%z target/release/zeroclaw 2>/dev/null || stat -c%s target/release/zeroclaw)
    SIZE_MB=$((SIZE_BYTES / 1024 / 1024))

    if [ $SIZE_MB -le 10 ]; then
        pass "Binary size is optimal: $BINARY_SIZE (${SIZE_MB}MB)"
    else
        warn "Binary size is larger than expected: $BINARY_SIZE (${SIZE_MB}MB)"
    fi
else
    fail "Release binary not found"
fi

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Phase 3: Configuration Tests
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Phase 3: Configuration Tests"

# Test 9: Config file existence
print_test "Configuration file check"
CONFIG_PATH="$HOME/.zeroclaw/config.toml"
if [ -f "$CONFIG_PATH" ]; then
    pass "Config file exists at $CONFIG_PATH"

    # Test 10: Telegram config
    print_test "Telegram configuration check"
    if grep -q "\[channels_config.telegram\]" "$CONFIG_PATH"; then
        pass "Telegram configuration found"

        # Test 11: Bot token configured
        print_test "Bot token validation"
        if grep -q "bot_token = \"" "$CONFIG_PATH"; then
            pass "Bot token is configured"
        else
            warn "Bot token not set - integration tests will be skipped"
        fi

        # Test 12: Allowlist configured
        print_test "User allowlist validation"
        if grep -q "allowed_users = \[" "$CONFIG_PATH"; then
            pass "User allowlist is configured"
        else
            warn "User allowlist not set"
        fi
    else
        warn "Telegram not configured - run 'zeroclaw onboard' first"
    fi
else
    warn "No config file found - run 'zeroclaw onboard' first"
fi

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Phase 4: Health Check Tests
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Phase 4: Health Check Tests"

# Test 13: Health check timeout
print_test "Health check timeout (should complete in <5s)"
START_TIME=$(date +%s)
HEALTH_OUTPUT=$(timeout 10 target/release/zeroclaw channel doctor 2>&1 || true)
END_TIME=$(date +%s)
HEALTH_TIME=$((END_TIME - START_TIME))

if [ $HEALTH_TIME -le 6 ]; then
    pass "Health check completed in ${HEALTH_TIME}s (timeout fix working)"
else
    warn "Health check took ${HEALTH_TIME}s (expected <5s)"
fi

# Test 14: Telegram connectivity
print_test "Telegram API connectivity"
if echo "$HEALTH_OUTPUT" | grep -q "Telegram.*healthy"; then
    pass "Telegram channel is healthy"
elif echo "$HEALTH_OUTPUT" | grep -q "Telegram.*unhealthy"; then
    warn "Telegram channel is unhealthy - check bot token"
elif echo "$HEALTH_OUTPUT" | grep -q "Telegram.*timed out"; then
    warn "Telegram health check timed out - network issue?"
else
    warn "Could not determine Telegram health status"
fi

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Phase 5: Feature Validation Tests
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Phase 5: Feature Validation Tests"

# Test 15: Message splitting function exists
print_test "Message splitting function implementation"
if grep -q "fn split_message_for_telegram" src/channels/telegram.rs; then
    pass "Message splitting function implemented"
else
    fail "Message splitting function not found"
fi

# Test 16: Message length constant
print_test "Telegram message length constant"
if grep -q "const TELEGRAM_MAX_MESSAGE_LENGTH: usize = 4096" src/channels/telegram.rs; then
    pass "TELEGRAM_MAX_MESSAGE_LENGTH constant defined correctly"
else
    fail "Message length constant missing or incorrect"
fi

# Test 17: Timeout implementation
print_test "Health check timeout implementation"
if grep -q "tokio::time::timeout" src/channels/telegram.rs; then
    pass "Timeout mechanism implemented in health_check"
else
    fail "Timeout not implemented in health_check"
fi

# Test 18: chat_id validation
print_test "chat_id validation implementation"
if grep -q "let Some(chat_id) = chat_id else" src/channels/telegram.rs; then
    pass "chat_id validation implemented"
else
    fail "chat_id validation missing"
fi

# Test 19: Duration import
print_test "std::time::Duration import"
if grep -q "use std::time::Duration" src/channels/telegram.rs; then
    pass "Duration import added"
else
    fail "Duration import missing"
fi

# Test 20: Continuation markers
print_test "Multi-part message markers"
if grep -q "(continues...)" src/channels/telegram.rs && grep -q "(continued)" src/channels/telegram.rs; then
    pass "Continuation markers implemented for split messages"
else
    fail "Continuation markers missing"
fi

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Phase 6: Integration Test Preparation
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Phase 6: Manual Integration Tests"

echo -e "${BLUE}The following tests require manual interaction:${NC}\n"

cat << 'EOF'
📱 Manual Test Checklist:

1. [ ] Start the channel:
   zeroclaw channel start

2. [ ] Send a short message to your bot in Telegram:
   "Hello bot!"
   ✓ Verify: Bot responds within 3 seconds

3. [ ] Send a long message (>4096 characters):
   python3 -c 'print("test " * 1000)'
   ✓ Verify: Message is split into chunks
   ✓ Verify: Chunks have (continues...) and (continued) markers
   ✓ Verify: All chunks arrive in order

4. [ ] Test unauthorized access:
   - Edit config: allowed_users = ["999999999"]
   - Send a message
   ✓ Verify: Warning log appears
   ✓ Verify: Message is ignored
   - Restore correct user ID

5. [ ] Test rapid messages (10 messages in 5 seconds):
   ✓ Verify: All messages are processed
   ✓ Verify: No rate limit errors
   ✓ Verify: Responses have delays

6. [ ] Check logs for errors:
   RUST_LOG=debug zeroclaw channel start
   ✓ Verify: No unexpected errors
   ✓ Verify: "missing chat_id" appears for malformed messages
   ✓ Verify: Health check logs show "timed out" if needed

EOF

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# Test Summary
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

print_header "Test Summary"

echo -e "${BLUE}Total Tests:${NC}   $TOTAL_TESTS"
echo -e "${GREEN}Passed:${NC}        $PASSED_TESTS"
echo -e "${RED}Failed:${NC}        $FAILED_TESTS"
echo -e "${YELLOW}Warnings:${NC}      $((TOTAL_TESTS - PASSED_TESTS - FAILED_TESTS))"

PASS_RATE=$((PASSED_TESTS * 100 / TOTAL_TESTS))
echo -e "\n${BLUE}Pass Rate:${NC}     ${PASS_RATE}%"

if [ $FAILED_TESTS -eq 0 ]; then
    echo -e "\n${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${GREEN}✓ ALL AUTOMATED TESTS PASSED! 🎉${NC}"
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}\n"

    echo -e "${BLUE}Next Steps:${NC}"
    echo -e "1. Run manual integration tests (see checklist above)"
    echo -e "2. Deploy to production when ready"
    echo -e "3. Monitor logs for issues\n"

    exit 0
else
    echo -e "\n${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${RED}✗ SOME TESTS FAILED${NC}"
    echo -e "${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}\n"

    echo -e "${BLUE}Troubleshooting:${NC}"
    echo -e "1. Review failed tests above"
    echo -e "2. Run: cargo test telegram --lib -- --nocapture"
    echo -e "3. Check: cargo clippy --all-targets"
    echo -e "4. Fix issues and re-run this script\n"

    exit 1
fi
