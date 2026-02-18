# Z.AI GLM Setup

ZeroClaw supports Z.AI's GLM models through OpenAI-compatible endpoints.
This guide covers practical setup options that match current ZeroClaw provider behavior.

## Overview

ZeroClaw supports these Z.AI aliases and endpoints out of the box:

| Alias | Endpoint | Notes |
|-------|----------|-------|
| `zai` | `https://api.z.ai/api/coding/paas/v4` | Global endpoint |
| `zai-cn` | `https://open.bigmodel.cn/api/paas/v4` | China endpoint |

If you need a custom base URL, see `docs/custom-providers.md`.

## Setup

### Quick Start

```bash
zeroclaw onboard \
  --provider "zai" \
  --api-key "YOUR_ZAI_API_KEY"
```

### Manual Configuration

Edit `~/.zeroclaw/config.toml`:

```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "zai"
default_model = "glm-5"
default_temperature = 0.7
```

## Available Models

| Model | Description |
|-------|-------------|
| `glm-5` | Default in onboarding; strongest reasoning |
| `glm-4.7` | Strong general-purpose quality |
| `glm-4.6` | Balanced baseline |
| `glm-4.5-air` | Lower-latency option |

Model availability can vary by account/region, so use the `/models` API when in doubt.

## Verify Setup

### Test with curl

```bash
# Test OpenAI-compatible endpoint
curl -X POST "https://api.z.ai/api/coding/paas/v4/chat/completions" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

Expected response:
```json
{
  "choices": [{
    "message": {
      "content": "Hello! How can I help you today?",
      "role": "assistant"
    }
  }]
}
```

### Test with ZeroClaw CLI

```bash
# Test agent directly
echo "Hello" | zeroclaw agent

# Check status
zeroclaw status
```

## Environment Variables

Add to your `.env` file:

```bash
# Z.AI API Key
ZAI_API_KEY=your-id.secret

# Optional generic key (used by many providers)
# API_KEY=your-id.secret
```

The key format is `id.secret` (for example: `abc123.xyz789`).

## Troubleshooting

### Rate Limiting

**Symptom:** `rate_limited` errors

**Solution:**
- Wait and retry
- Check your Z.AI plan limits
- Try `glm-4.5-air` for lower latency and higher quota tolerance

### Authentication Errors

**Symptom:** 401 or 403 errors

**Solution:**
- Verify your API key format is `id.secret`
- Check the key hasn't expired
- Ensure no extra whitespace in the key

### Model Not Found

**Symptom:** Model not available error

**Solution:**
- List available models:
```bash
curl -s "https://api.z.ai/api/coding/paas/v4/models" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" | jq '.data[].id'
```

## Getting an API Key

1. Go to [Z.AI](https://z.ai)
2. Sign up for a Coding Plan
3. Generate an API key from the dashboard
4. Key format: `id.secret` (e.g., `abc123.xyz789`)

## Related Documentation

- [ZeroClaw README](../README.md)
- [Custom Provider Endpoints](./custom-providers.md)
- [Contributing Guide](../CONTRIBUTING.md)
