# Multi-Model Setup and Fallback Chains

This guide introduces multi-model concepts in ZeroClaw, including fallback provider chains, model-level fallbacks, and API key rotation for resilience.

**Last verified: March 28, 2026**

## When to Use Multi-Model Setup

Multi-model configuration is useful for:

- **High reliability**: Automatically fall back to alternative providers when the primary fails
- **Cost optimization**: Route expensive models through fallback chains for rate-limited scenarios
- **Regional resilience**: Use geographically distributed providers to handle region-specific outages
- **Capability flexibility**: Try different models when one lacks required features (e.g., tool calling, vision)
- **Rate limit handling**: Rotate through API keys on `429` (rate limit) responses
- **Development and testing**: Switch between cloud and local models without code changes

## Core Concepts

### Fallback Provider Chains

When a provider experiences a transient error (timeout, connection failure, auth issue), ZeroClaw automatically attempts fallback providers in the order specified.

**Example**: If your primary provider is `openai` but it's temporarily unavailable, ZeroClaw can automatically fall back to `anthropic`, then `groq`.

```toml
[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
```

When the primary provider recovers, ZeroClaw resumes using it (no sticky failover).

### Model-Level Fallbacks

Some models may not be available in all regions, or you might want to use a faster model when a heavy model is rate-limited.

```toml
[reliability]
model_fallbacks = { "claude-opus-4-20250514" = ["claude-sonnet-4-20250514", "gpt-4o"] }
```

If `claude-opus-4-20250514` fails or is unavailable, ZeroClaw tries the fallback models in order while staying within the same provider (unless a provider-level fallback is also configured).

### API Key Rotation

For providers that frequently encounter rate limits, you can supply additional API keys that ZeroClaw will rotate through on `429` responses.

```toml
[reliability]
api_keys = ["sk-key-2", "sk-key-3", "sk-key-4"]
```

The primary `api_key` (configured globally or per-channel) is always tried first; these extras are rotated on rate-limit errors.

### Provider Retries

Each provider attempt includes configurable retries with exponential backoff before moving to the next fallback.

```toml
[reliability]
provider_retries = 2          # Retry count per provider
provider_backoff_ms = 500     # Initial backoff in milliseconds
```

## Configuration Structure

The `[reliability]` section in `config.toml`:

| Key | Type | Default | Purpose |
|---|---|---|---|
| `fallback_providers` | `[string]` | `[]` | Ordered list of fallback provider IDs |
| `model_fallbacks` | `{string: [string]}` | `{}` | Map of model → list of fallback models |
| `api_keys` | `[string]` | `[]` | Additional API keys for rate-limit rotation |
| `provider_retries` | `u32` | `2` | Retry attempts per provider before failover |
| `provider_backoff_ms` | `u64` | `500` | Initial backoff delay in milliseconds |

## Example Configurations

### Basic Fallback Chain

Set up a simple fallback from your primary provider to a backup:

```toml
default_provider = "openai"
default_model = "gpt-4o"

[reliability]
fallback_providers = ["anthropic"]
```

**Behavior**: If OpenAI times out or returns an error, ZeroClaw will retry twice with exponential backoff, then attempt the same request using Anthropic.

### High-Reliability Multi-Provider Setup

Combine provider fallbacks with model fallbacks and API key rotation:

```toml
default_provider = "openai"
default_model = "gpt-4o"
api_key = "sk-openai-primary"

[reliability]
fallback_providers = ["anthropic", "groq", "openrouter"]
api_keys = ["sk-openai-backup-1", "sk-openai-backup-2"]

[reliability.model_fallbacks]
"gpt-4o" = ["gpt-4-turbo", "gpt-3.5-turbo"]
"gpt-4-turbo" = ["gpt-3.5-turbo"]
```

**Behavior**:
1. Try OpenAI `gpt-4o` with primary key (2 retries)
2. On rate-limit, rotate to backup API keys
3. If OpenAI still fails, fall back to Anthropic with same model request (Anthropic will select available equivalent)
4. If Anthropic unavailable, try Groq, then OpenRouter
5. If model not available, try fallback models in order

### Local Development with Cloud Fallback

Use a local Ollama instance as primary, fall back to cloud provider:

```toml
default_provider = "ollama"
default_model = "llama2:70b"
api_url = "http://localhost:11434"

[reliability]
fallback_providers = ["openrouter", "groq"]
```

**Behavior**: If Ollama goes down or times out, automatically use OpenRouter or Groq instead without configuration changes.

### Cost Optimization: Heavy Model with Fast Fallback

Use an expensive reasoning model for complex tasks, but fall back to a faster model:

```toml
default_provider = "anthropic"
default_model = "claude-opus-4-20250514"

[reliability]
model_fallbacks = { "claude-opus-4-20250514" = ["claude-sonnet-4-20250514"] }
```

**Behavior**: When Opus is rate-limited or slow, automatically use Sonnet (typically 2–3x faster and cheaper).

## Multi-Region Setup

For organizations with multi-region deployments:

```toml
# Primary US region
default_provider = "anthropic"
default_model = "claude-sonnet-4-20250514"

[reliability]
# Fall back to EU region provider if US Anthropic is down
fallback_providers = ["bedrock"]  # AWS Bedrock in multiple regions
provider_retries = 3
provider_backoff_ms = 1000
```

Ensure each fallback provider has credentials in your environment:

```bash
export ANTHROPIC_API_KEY="..."
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
```

## Hot Reload Behavior

The `[reliability]` section is hot-reloadable. While a channel or gateway is running, updates to `config.toml` take effect on the next inbound message without requiring a restart.

Updated fields:
- `fallback_providers`
- `model_fallbacks`
- `api_keys`
- `provider_retries`
- `provider_backoff_ms`

## Error Handling and Fallback Triggers

Fallback is triggered by:

- **Timeout**: Provider did not respond within the configured timeout
- **Connection error**: Network/DNS failure
- **Auth error**: Invalid credentials (retries only if transient auth service issues detected)
- **Rate limit (429)**: HTTP 429; triggers API key rotation first, then provider fallback
- **Service unavailable (503)**: Temporary service issue
- **Model not found**: Triggers model fallback chain if configured

Fallback is **not** triggered by:

- **Invalid request (400)**: Malformed input; retrying won't help
- **Permanent auth failure**: Invalid API key format
- **Model output errors**: The model responded but returned an error

## Debugging Fallback Activity

Enable runtime traces to debug fallback behavior:

```toml
[observability]
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
```

Then query traces:

```bash
# Show all fallback events
zeroclaw doctor traces --contains "fallback"

# Show provider retry details
zeroclaw doctor traces --contains "provider"

# Show rate-limit rotation
zeroclaw doctor traces --contains "429"
```

## Best Practices

1. **Order by reliability**: Put most reliable providers first in `fallback_providers`
2. **Test fallback chains**: Verify fallback behavior before production use
3. **Monitor API key rotation**: Track rate-limit events to know when rotation is active
4. **Keep model fallbacks semantically similar**: Don't fall back from a reasoning model to a chat model without intention
5. **Use environment variables**: Store sensitive API keys in env, not config
6. **Document fallback intent**: Add comments in config explaining why each fallback exists
7. **Verify multi-model credentials**: Ensure all fallback providers have valid credentials set

## Credential Resolution

Each fallback provider resolves credentials independently using the standard resolution order:

1. Explicit credential from config/CLI
2. Provider-specific environment variable
3. Generic fallback: `ZEROCLAW_API_KEY`, then `API_KEY`

**Important**: The primary provider's API key is not automatically reused by fallback providers. Set credentials for each provider separately.

Example:

```bash
export OPENAI_API_KEY="sk-..."
export ANTHROPIC_API_KEY="claude-..."
export GROQ_API_KEY="gsk-..."
```

## Limits and Constraints

- Maximum fallback providers: Limited by configuration file size (typically 100+ chains are supported)
- Maximum model fallbacks per model: No hard limit
- API key rotation: All keys are tried before timing out
- Retry attempts: Configurable per provider with exponential backoff
- Total timeout budget: Cumulative across retries and fallbacks; channel-level timeout still applies

## Related Documentation

- [Config Reference: Reliability Section](/docs/reference/api/config-reference.md#reliability)
- [Providers Reference: Fallback Provider Chains](/docs/reference/api/providers-reference.md#fallback-provider-chains)
- [Observability and Debugging](/docs/ops/observability.md)
