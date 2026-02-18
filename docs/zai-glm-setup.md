# Z.AI GLM Coding Plan Setup

ZeroClaw supports Z.AI's GLM models through multiple endpoints. This guide covers the recommended configuration.

## Overview

Z.AI provides GLM models through two API styles:

| Endpoint | API Style | Provider String |
|----------|-----------|-----------------|
| `/api/coding/paas/v4` | OpenAI-compatible | `zai` |
| `/api/anthropic` | Anthropic-compatible | `anthropic-custom:https://api.z.ai/api/anthropic` |

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
default_model = "glm-4.7"
default_temperature = 0.7
```

## Available Models

| Model | Description |
|-------|-------------|
| `glm-4.5` | Stable release |
| `glm-4.5-air` | Lightweight version |
| `glm-4.6` | Improved reasoning |
| `glm-4.7` | Current recommended |
| `glm-5` | Latest |

## Verify Setup

### Test with curl

```bash
# Test OpenAI-compatible endpoint
curl -X POST "https://api.z.ai/api/coding/paas/v4/chat/completions" \
  -H "Authorization: Bearer YOUR_ZAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-4.7",
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

# Or use the provider-specific variable
# The key format is: id.secret (e.g., abc123.xyz789)
```

## Troubleshooting

### Rate Limiting

**Symptom:** `rate_limited` errors

**Solution:**
- Wait and retry
- Check your Z.AI plan limits
- Try `glm-4.7` instead of `glm-5` (more stable availability)

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
- [Contributing Guide](../CONTRIBUTING.md)
