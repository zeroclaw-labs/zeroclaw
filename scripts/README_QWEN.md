# Qwen Provider Testing Scripts

These scripts were used for initial validation and testing of the Qwen OAuth provider integration.

## Scripts

### qwen_model_probe.sh

Tests availability of different Qwen models through OAuth API.

**Usage:**
```bash
./scripts/qwen_model_probe.sh
```

**Output:** CSV with model availability results (saved to `docs/qwen_model_test_results.csv`)

---

### qwen_context_test.sh

Tests context window limits by sending progressively larger prompts.

**Usage:**
```bash
./scripts/qwen_context_test.sh
```

**Tests:** 1K, 2K, 4K, 8K, 16K, 32K, 65K, 131K token contexts

---

### qwen_latency_benchmark.sh

Measures response latency for Qwen provider.

**Usage:**
```bash
./scripts/qwen_latency_benchmark.sh [iterations]
```

**Default:** 3 iterations

**Output:** Individual and average response times

---

## Test Results

See full test report: [`docs/qwen-provider-test-report.md`](../docs/qwen-provider-test-report.md)

**Summary:**
- ✅ Available model: `qwen3-coder-plus`
- ✅ Context window: ~32K tokens
- ✅ Average latency: ~2.8s
- ✅ Quota tracking: Static display (`?/1000`)

---

**Date:** 2026-02-24
**Status:** Testing complete, scripts preserved for future validation
