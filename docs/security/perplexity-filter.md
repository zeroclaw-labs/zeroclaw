# Perplexity Filter (Opt-In)

ZeroClaw provides an opt-in lightweight statistical filter that detects
adversarial suffixes (for example, GCG-style optimized gibberish tails)
before messages are sent to an LLM provider.

## Scope

- Applies to channel and gateway inbound messages before provider execution.
- Does not require external model calls or heavyweight guard models.
- Disabled by default for compatibility and latency predictability.

## How It Works

The filter evaluates a trailing prompt window using:

1. Character-class bigram perplexity.
2. Suffix punctuation ratio.
3. GCG-like token pattern checks (mixed punctuation + letters + digits).

The message is blocked only when anomaly criteria are met.

## Config

```toml
[security.perplexity_filter]
enable_perplexity_filter = true
perplexity_threshold = 16.5
suffix_window_chars = 72
min_prompt_chars = 40
symbol_ratio_threshold = 0.25
```

## Latency

The implementation is O(n) over prompt length and avoids network calls.
Local debug-safe regression includes a strict `<50ms` budget test for a
typical multi-sentence prompt payload.

## Tuning Guidance

- Increase `perplexity_threshold` if you see false positives.
- Increase `symbol_ratio_threshold` to reduce blocking of technical strings.
- Increase `min_prompt_chars` to ignore short prompts where statistics are weak.
- Keep the feature disabled unless you explicitly need this extra defense layer.
