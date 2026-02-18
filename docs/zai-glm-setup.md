# Z.AI GLM Coding Plan Setup

ZeroClaw supports Z.AI's GLM models through multiple endpoints. This guide covers the recommended setup for best tool calling support.

## Overview

Z.AI provides GLM models through two API styles:

| Endpoint | API Style | Tool Calling | Recommended |
|----------|-----------|--------------|-------------|
| `/api/coding/paas/v4` | OpenAI-compatible | Limited (text output) | ❌ Not recommended |
| `/api/anthropic` | Anthropic-compatible | Full support | ✅ Recommended |

## Why Anthropic-Compatible Endpoint?

The OpenAI-compatible endpoint (`/api/coding/paas/v4`) outputs tool calls as text instead of properly executing them. The Anthropic-compatible endpoint handles tool calling correctly.

## Setup

### Quick Start

```bash
# Configure with Anthropic-compatible endpoint
zeroclaw onboard \
  --provider "anthropic-custom:https://api.z.ai/api/anthropic" \
  --api-key "YOUR_ZAI_API_KEY"
```

### Manual Configuration

Edit `~/.zeroclaw/config.toml`:

```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "anthropic-custom:https://api.z.ai/api/anthropic"
default_model = "glm-4.7"
default_temperature = 0.7
```

### Environment Variables (Alternative)

```bash
export ANTHROPIC_API_KEY="YOUR_ZAI_API_KEY"
```

Then use provider:

```toml
default_provider = "anthropic-custom:https://api.z.ai/api/anthropic"
```

## Available Models

| Model | Description |
|-------|-------------|
| `glm-4.5` | Stable release |
| `glm-4.5-air` | Lightweight version |
| `glm-4.6` | Improved reasoning |
| `glm-4.7` | Current recommended |
| `glm-5` | Latest (may have rate limits) |

## Verify Setup

```bash
# Test the endpoint
curl -X POST "https://api.z.ai/api/anthropic/v1/messages" \
  -H "x-api-key: YOUR_ZAI_API_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "glm-4.7",
    "max_tokens": 100,
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# Check ZeroClaw status
zeroclaw status
```

## Troubleshooting

### Tool Calls Shown as Text

**Symptom:** Bot outputs raw JSON like `<toolcall>shell{...}` instead of executing tools.

**Solution:** Use the Anthropic-compatible endpoint (`anthropic-custom:https://api.z.ai/api/anthropic`) instead of the OpenAI-compatible one (`zai`).

### Rate Limiting

**Symptom:** `rate_limited` errors.

**Solution:** 
- Switch to `glm-4.7` (more stable) if using `glm-5`
- Wait and retry
- Check your Z.AI plan limits

### Context Lost

**Symptom:** Bot doesn't remember previous messages.

**Solution:** Enable memory:

```toml
[memory]
backend = "sqlite"
auto_save = true
```

## Full Example Config

```toml
api_key = "YOUR_ZAI_API_KEY"
default_provider = "anthropic-custom:https://api.z.ai/api/anthropic"
default_model = "glm-4.7"
default_temperature = 0.7

[autonomy]
level = "full"

[memory]
backend = "sqlite"
auto_save = true

[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = ["YOUR_USER_ID"]
```

## Getting Z.AI API Key

1. Go to [Z.AI](https://z.ai)
2. Sign up for a Coding Plan
3. Get your API key from the dashboard
4. Format: `id.secret` (e.g., `abc123.xyz789`)
