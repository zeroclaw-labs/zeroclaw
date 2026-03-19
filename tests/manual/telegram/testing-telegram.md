# Telegram Integration Testing Guide

This guide covers testing the Telegram channel integration for JhedaiClaw.

## 🚀 Quick Start

### Automated Tests

```bash
# Full test suite (20+ tests, ~2 minutes)
./tests/telegram/test_telegram_integration.sh

# Quick smoke test (~10 seconds)
./tests/telegram/quick_test.sh

# Just unit tests
cargo test telegram --lib
```

## 📋 Test Coverage

### Automated Tests (20 tests)

The `test_telegram_integration.sh` script runs:

**Phase 1: Code Quality (5 tests)**

- ✅ Test compilation
- ✅ Unit tests (24 tests)
- ✅ Message splitting tests (8 tests)
- ✅ Clippy linting
- ✅ Code formatting

**Phase 2: Build Tests (3 tests)**

- ✅ Debug build
- ✅ Release build
- ✅ Binary size verification (<10MB)

**Phase 3: Configuration Tests (4 tests)**

- ✅ Config file exists
- ✅ Telegram section configured
- ✅ Bot token set
- ✅ User allowlist configured

**Phase 4: Health Check Tests (2 tests)**

- ✅ Health check timeout (<5s)
- ✅ Telegram API connectivity

**Phase 5: Feature Validation (6 tests)**

- ✅ Message splitting function
- ✅ Message length constant (4096)
- ✅ Timeout implementation
- ✅ chat_id validation
- ✅ Duration import
- ✅ Continuation markers

### Manual Tests (6 tests)

After running automated tests, perform these manual checks:

1. **Basic messaging**

   ```bash
   jhedaiclaw channel start
   ```

   - Send "Hello bot!" in Telegram
   - Verify response within 3 seconds

2. **Long message splitting**

   ```bash
   # Generate 5000+ char message
   python3 -c 'print("test " * 1000)'
   ```

   - Paste into Telegram
   - Verify: Message split into chunks
   - Verify: Markers show `(continues...)` and `(continued)`
   - Verify: All chunks arrive in order

3. **Unauthorized user blocking**

   ```toml
   # Edit ~/.jhedaiclaw/config.toml
   allowed_users = ["999999999"]
   ```

   - Send message to bot
   - Verify: Warning in logs
   - Verify: Message ignored
   - Restore correct user ID

4. **Rate limiting**
   - Send 10 messages rapidly
   - Verify: All processed
   - Verify: No "Too Many Requests" errors
   - Verify: Responses have delays

5. **Mention-only mode (group chats)**

   ```toml
   # Edit ~/.jhedaiclaw/config.toml
   [channels.telegram]
   mention_only = true
   ```

   - Add bot to a group chat
   - Send message without @botname mention
   - Verify: Bot does not respond
   - Send message with @botname mention
   - Verify: Bot responds and mention is stripped
   - DM/private chat should always work regardless of mention_only

6. **Error logging**

   ```bash
   RUST_LOG=debug jhedaiclaw channel start
   ```

   - Check for unexpected errors
   - Verify proper error handling

7. **Health check timeout**

   ```bash
   time jhedaiclaw channel doctor
   ```

   - Verify: Completes in <5 seconds

## 🔍 Test Results Interpretation

### Success Criteria

- All 20 automated tests pass ✅
- Health check completes in <5s ✅
- Binary size <10MB ✅
- No clippy warnings ✅
- All manual tests pass ✅

### Common Issues

**Issue: Health check times out**

```
Solution: Check bot token is valid
  curl "https://api.telegram.org/bot<TOKEN>/getMe"
```

**Issue: Bot doesn't respond**

```
Solution: Check user allowlist
  1. Send message to bot
  2. Check logs for user_id
  3. Update config: allowed_users = ["YOUR_ID"]
  4. Run: jhedaiclaw onboard --channels-only
```

**Issue: Message splitting not working**

```
Solution: Verify code changes
  grep -n "split_message_for_telegram" src/channels/telegram.rs
  grep -n "TELEGRAM_MAX_MESSAGE_LENGTH" src/channels/telegram.rs
```

## 🧪 Test Scenarios

### Scenario 1: First-Time Setup

```bash
# 1. Run automated tests
./tests/telegram/test_telegram_integration.sh

# 2. Configure Telegram
jhedaiclaw onboard
# Select Telegram channel
# Enter bot token (from @BotFather)
# Enter your user ID

# 3. Verify health
jhedaiclaw channel doctor

# 4. Start channel
jhedaiclaw channel start

# 5. Send test message in Telegram
```

### Scenario 2: After Code Changes

```bash
# 1. Quick validation
./tests/telegram/quick_test.sh

# 2. Full test suite
./tests/telegram/test_telegram_integration.sh

# 3. Manual smoke test
jhedaiclaw channel start
# Send message in Telegram
```

### Scenario 3: Production Deployment

```bash
# 1. Full test suite
./tests/telegram/test_telegram_integration.sh

# 2. Load test (optional)
# Send 100 messages rapidly
for i in {1..100}; do
  echo "Test message $i" | \
    curl -X POST "https://api.telegram.org/bot<TOKEN>/sendMessage" \
         -d "chat_id=<CHAT_ID>" \
         -d "text=Message $i"
done

# 3. Monitor logs
RUST_LOG=info jhedaiclaw daemon

# 4. Check metrics
jhedaiclaw status
```

## 📊 Performance Benchmarks

Expected values after all fixes:

| Metric                 | Expected   | How to Measure                     |
| ---------------------- | ---------- | ---------------------------------- |
| Health check time      | <5s        | `time jhedaiclaw channel doctor`   |
| First response time    | <3s        | Time from sending to receiving     |
| Message split overhead | <50ms      | Check logs for timing              |
| Memory usage           | <10MB      | `ps aux \| grep jhedaiclaw`        |
| Binary size            | ~3-4MB     | `ls -lh target/release/jhedaiclaw` |
| Unit test coverage     | 61/61 pass | `cargo test telegram --lib`        |

## 🐛 Debugging Failed Tests

### Debug Unit Tests

```bash
# Verbose output
cargo test telegram --lib -- --nocapture

# Specific test
cargo test telegram_split_over_limit -- --nocapture

# Show ignored tests
cargo test telegram --lib -- --ignored
```

### Debug Integration Issues

```bash
# Maximum logging
RUST_LOG=trace jhedaiclaw channel start

# Check Telegram API directly
curl "https://api.telegram.org/bot<TOKEN>/getMe"
curl "https://api.telegram.org/bot<TOKEN>/getUpdates"

# Validate config
cat ~/.jhedaiclaw/config.toml | grep -A 3 "\[channels_config.telegram\]"
```

### Debug Build Issues

```bash
# Clean build
cargo clean
cargo build --release

# Check dependencies
cargo tree | grep telegram

# Update dependencies
cargo update
```

## 🎯 CI/CD Integration

Add to your CI pipeline:

```yaml
# .github/workflows/test.yml
name: Test Telegram Integration

on: [push, pull_request]

jobs:
  test:
    runs-on: blacksmith-2vcpu-ubuntu-2404
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Run tests
        run: |
          cargo test telegram --lib
          cargo clippy --all-targets -- -D warnings
      - name: Check formatting
        run: cargo fmt --check
```

## 📝 Test Checklist

Before merging code:

- [ ] `./tests/telegram/quick_test.sh` passes
- [ ] `./tests/telegram/test_telegram_integration.sh` passes
- [ ] Manual tests completed
- [ ] No new clippy warnings
- [ ] Code is formatted (`cargo fmt`)
- [ ] Documentation updated
- [ ] CHANGELOG.md updated

## 🚨 Emergency Rollback

If tests fail in production:

```bash
# 1. Check git history
git log --oneline src/channels/telegram.rs

# 2. Rollback to previous version
git revert <commit-hash>

# 3. Rebuild
cargo build --release

# 4. Restart service
jhedaiclaw service restart

# 5. Verify
jhedaiclaw channel doctor
```

## 📚 Additional Resources

- [Telegram Bot API Documentation](https://core.telegram.org/bots/api)
- [JhedaiClaw Main README](../../README.md)
- [Contributing Guide](../../CONTRIBUTING.md)
- [Issue Tracker](https://github.com/jhedai/jhedaiclaw/issues)
