# 🧪 Test Execution Guide

## Quick Reference

```bash
# Full automated test suite (~2 min)
./test_telegram_integration.sh

# Quick smoke test (~10 sec)
./quick_test.sh

# Just compile and unit test (~30 sec)
cargo test telegram --lib
```

## 📝 What Was Created For You

### 1. **test_telegram_integration.sh** (Main Test Suite)
   - **20+ automated tests** covering all fixes
   - **6 test phases**: Code quality, build, config, health, features, manual
   - **Colored output** with pass/fail indicators
   - **Detailed summary** at the end

   ```bash
   ./test_telegram_integration.sh
   ```

### 2. **quick_test.sh** (Fast Validation)
   - **4 essential tests** for quick feedback
   - **<10 second** execution time
   - Perfect for **pre-commit** checks

   ```bash
   ./quick_test.sh
   ```

### 3. **generate_test_messages.py** (Test Helper)
   - Generates test messages of various lengths
   - Tests message splitting functionality
   - 8 different message types

   ```bash
   # Generate a long message (>4096 chars)
   python3 test_helpers/generate_test_messages.py long

   # Show all message types
   python3 test_helpers/generate_test_messages.py all
   ```

### 4. **TESTING_TELEGRAM.md** (Complete Guide)
   - Comprehensive testing documentation
   - Troubleshooting guide
   - Performance benchmarks
   - CI/CD integration examples

## 🚀 Step-by-Step: First Run

### Step 1: Run Automated Tests

```bash
cd /Users/abdzsam/zeroclaw

# Make scripts executable (already done)
chmod +x test_telegram_integration.sh quick_test.sh

# Run the full test suite
./test_telegram_integration.sh
```

**Expected output:**
```
⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡

███████╗███████╗██████╗  ██████╗  ██████╗██╗      █████╗ ██╗    ██╗
...

🧪 TELEGRAM INTEGRATION TEST SUITE 🧪

Phase 1: Code Quality Tests
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Test 1: Compiling test suite
✓ PASS: Test suite compiles successfully

Test 2: Running Telegram unit tests
✓ PASS: All Telegram unit tests passed (24 tests)
...

Test Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
Total Tests:   20
Passed:        20
Failed:        0
Warnings:      0

Pass Rate:     100%

✓ ALL AUTOMATED TESTS PASSED! 🎉
```

### Step 2: Configure Telegram (if not done)

```bash
# Interactive setup
zeroclaw onboard --interactive

# Or channels-only setup
zeroclaw onboard --channels-only
```

When prompted:
1. Select **Telegram** channel
2. Enter your **bot token** from @BotFather
3. Enter your **Telegram user ID** or username

### Step 3: Verify Health

```bash
zeroclaw channel doctor
```

**Expected output:**
```
🩺 ZeroClaw Channel Doctor

  ✅ Telegram  healthy

Summary: 1 healthy, 0 unhealthy, 0 timed out
```

### Step 4: Manual Testing

#### Test 1: Basic Message

```bash
# Terminal 1: Start the channel
zeroclaw channel start
```

**In Telegram:**
- Find your bot
- Send: `Hello bot!`
- **Verify**: Bot responds within 3 seconds

#### Test 2: Long Message (Split Test)

```bash
# Generate a long message
python3 test_helpers/generate_test_messages.py long
```

- **Copy the output**
- **Paste into Telegram** to your bot
- **Verify**:
  - Message is split into 2+ chunks
  - First chunk ends with `(continues...)`
  - Middle chunks have `(continued)` and `(continues...)`
  - Last chunk starts with `(continued)`
  - All chunks arrive in order

#### Test 3: Word Boundary Splitting

```bash
python3 test_helpers/generate_test_messages.py word
```

- Send to bot
- **Verify**: Splits at word boundaries (not mid-word)

## 🎯 Test Results Checklist

After running all tests, verify:

### Automated Tests
- [ ] ✅ All 20 automated tests passed
- [ ] ✅ Build completed successfully
- [ ] ✅ Binary size <10MB
- [ ] ✅ Health check completes in <5s
- [ ] ✅ No clippy warnings

### Manual Tests
- [ ] ✅ Bot responds to basic messages
- [ ] ✅ Long messages split correctly
- [ ] ✅ Continuation markers appear
- [ ] ✅ Word boundaries respected
- [ ] ✅ Allowlist blocks unauthorized users
- [ ] ✅ No errors in logs

### Performance
- [ ] ✅ Response time <3 seconds
- [ ] ✅ Memory usage <10MB
- [ ] ✅ No message loss
- [ ] ✅ Rate limiting works (100ms delays)

## 🐛 Troubleshooting

### Issue: Tests fail to compile

```bash
# Clean build
cargo clean
cargo build --release

# Update dependencies
cargo update
```

### Issue: "Bot token not configured"

```bash
# Check config
cat ~/.zeroclaw/config.toml | grep -A 5 telegram

# Reconfigure
zeroclaw onboard --channels-only
```

### Issue: Health check fails

```bash
# Test bot token directly
curl "https://api.telegram.org/bot<YOUR_TOKEN>/getMe"

# Should return: {"ok":true,"result":{...}}
```

### Issue: Bot doesn't respond

```bash
# Enable debug logging
RUST_LOG=debug zeroclaw channel start

# Look for:
# - "Telegram channel listening for messages..."
# - "ignoring message from unauthorized user" (if allowlist issue)
# - Any error messages
```

## 📊 Performance Benchmarks

After all fixes, you should see:

| Metric | Target | Command |
|--------|--------|---------|
| Unit test pass | 24/24 | `cargo test telegram --lib` |
| Build time | <30s | `time cargo build --release` |
| Binary size | ~3-4MB | `ls -lh target/release/zeroclaw` |
| Health check | <5s | `time zeroclaw channel doctor` |
| First response | <3s | Manual test in Telegram |
| Message split | <50ms | Check debug logs |
| Memory usage | <10MB | `ps aux \| grep zeroclaw` |

## 🔄 CI/CD Integration

Add to your workflow:

```bash
# Pre-commit hook
#!/bin/bash
./quick_test.sh

# CI pipeline
./test_telegram_integration.sh
```

## 📚 Next Steps

1. **Run the tests:**
   ```bash
   ./test_telegram_integration.sh
   ```

2. **Fix any failures** using the troubleshooting guide

3. **Complete manual tests** using the checklist

4. **Deploy to production** when all tests pass

5. **Monitor logs** for any issues:
   ```bash
   zeroclaw daemon
   # or
   RUST_LOG=info zeroclaw channel start
   ```

## 🎉 Success!

If all tests pass:
- ✅ Message splitting works (4096 char limit)
- ✅ Health check has 5s timeout
- ✅ Empty chat_id is handled safely
- ✅ All 24 unit tests pass
- ✅ Code is production-ready

**Your Telegram integration is ready to go!** 🚀

---

## 📞 Support

- Issues: https://github.com/theonlyhennygod/zeroclaw/issues
- Docs: `./TESTING_TELEGRAM.md`
- Help: `zeroclaw --help`
