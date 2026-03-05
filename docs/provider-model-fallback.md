# Provider Model Fallback Configuration

This guide explains how to configure ZeroClaw to handle quota exhaustion and model failures gracefully using provider and model fallback chains.

## Problem: Rate Limits and Model Quota

When using AI providers, you may encounter:

1. **Rate limits (429)** - Your primary provider has no available quota
2. **Model-specific quota** - A specific model (e.g., `gemini-2.0-flash-exp`) is exhausted but other models from the same provider work
3. **Model incompatibility** - Fallback providers don't support the same model names

## Solution 1: Provider Fallback with Model-Specific Defaults

Configure fallback providers with their own default models:

```toml
# config.toml
default_provider = "google"
default_model = "gemini-2.0-flash-exp"

[reliability]
# When Gemini fails, fall back to OpenAI Codex
fallback_providers = ["openai-codex:codex-1"]

# Provider-specific configuration
[[providers.openai_codex]]
profile = "codex-1"
model = "gpt-4o-mini"  # Use this model when falling back to Codex
```

**How it works:**

1. Primary request goes to Gemini with `gemini-2.0-flash-exp`
2. If Gemini returns 429 (rate limited), ZeroClaw tries the fallback
3. Fallback provider (OpenAI Codex) uses its configured model (`gpt-4o-mini`) instead of trying the Gemini-specific model
4. No "400 Bad Request: model not supported" errors!

## Solution 2: Model Fallback Within Same Provider

When quota for one model is exhausted but other models from the same provider work:

```toml
# config.toml
default_provider = "google"
default_model = "gemini-2.0-flash-exp"

[reliability]
# Try alternative Gemini models when quota is exhausted
[reliability.model_fallbacks]
"gemini-2.0-flash-exp" = ["gemini-1.5-pro", "gemini-1.5-flash"]
```

**How it works:**

1. Request tries `gemini-2.0-flash-exp` first
2. If it fails with rate limit (429), retry with `gemini-1.5-pro`
3. If that also fails, retry with `gemini-1.5-flash`
4. All retries use the same provider (Gemini)

## Solution 3: Combined Provider + Model Fallback

For maximum reliability, combine both strategies:

```toml
# config.toml
default_provider = "google"
default_model = "gemini-2.0-flash-exp"

[reliability]
# Provider fallback chain
fallback_providers = ["anthropic", "openai-codex:codex-1"]

# Model fallback within each provider
[reliability.model_fallbacks]
"gemini-2.0-flash-exp" = ["gemini-1.5-pro"]
"claude-opus-4" = ["claude-sonnet-4"]

# Provider-specific models for fallbacks
[[providers.openai_codex]]
profile = "codex-1"
model = "gpt-4o-mini"
```

**Fallback order:**

1. `gemini-2.0-flash-exp` on Google
2. `gemini-1.5-pro` on Google (model fallback)
3. `claude-opus-4` on Anthropic (provider fallback)
4. `claude-sonnet-4` on Anthropic (model fallback)
5. `gpt-4o-mini` on OpenAI Codex (provider fallback with default model)

## Retry Configuration

Fine-tune retry behavior:

```toml
[reliability]
provider_retries = 2        # Retry each provider 2 times before moving to next
provider_backoff_ms = 500   # Wait 500ms between retries (exponential backoff)
```

## Real-World Example: Multi-Region Gemini Setup

```toml
# config.toml
default_provider = "google"
default_model = "gemini-2.0-flash-exp"

[reliability]
# Rotate through API keys (round-robin on 429)
api_keys = [
    "sk-key-for-project-a",
    "sk-key-for-project-b",
    "sk-key-for-project-c"
]

# Model fallback within Gemini
[reliability.model_fallbacks]
"gemini-2.0-flash-exp" = [
    "gemini-1.5-pro-latest",
    "gemini-1.5-flash-8b"
]

# Provider fallback if all Gemini quota exhausted
fallback_providers = ["anthropic", "openai"]
```

**What happens when quota runs out:**

1. Try `gemini-2.0-flash-exp` with `sk-key-for-project-a`
2. 429 → rotate to `sk-key-for-project-b` and retry
3. 429 → rotate to `sk-key-for-project-c` and retry
4. Still failing → try `gemini-1.5-pro-latest` (model fallback)
5. Still failing → try `gemini-1.5-flash-8b` (model fallback)
6. Still failing → fall back to Anthropic Claude
7. Still failing → fall back to OpenAI

## Logging and Monitoring

When fallback occurs, ZeroClaw logs:

```
[INFO] Provider recovered (failover/retry)
  provider="openai-codex"
  model="gpt-4o-mini"
  original_model="gemini-2.0-flash-exp"
  requested_model="gemini-2.0-flash-exp"
  attempt=1
```

Monitor these logs to:
- Detect when quotas are running low
- Identify which fallback paths are used most
- Optimize your provider and model configuration

## Best Practices

1. **Test your fallback chain** - Ensure all providers in your chain have valid credentials
2. **Use model fallbacks first** - Cheaper to try different models from same provider than switching providers
3. **Set appropriate retry counts** - Too many retries slow down responses; too few miss transient failures
4. **Monitor costs** - Fallback models may have different pricing
5. **Keep provider-specific models updated** - When adding new providers, configure their default models

## See Also

- [Config Reference](config-reference.md) - Full configuration schema
- [Providers Reference](providers-reference.md) - Supported providers and authentication
- [Operations Runbook](operations-runbook.md) - Production deployment guide
