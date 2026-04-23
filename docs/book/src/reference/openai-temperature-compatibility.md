# OpenAI Temperature Compatibility Reference

This document provides empirical evidence for temperature parameter compatibility across OpenAI models.

## Summary

Different OpenAI model families have different temperature requirements:

- **Reasoning models** (o-series, gpt-5 base variants): Only accept `temperature=1.0`
- **Search models**: Do not accept temperature parameter (must be omitted)
- **Standard models** (gpt-3.5, gpt-4, gpt-4o): Accept flexible temperature values (0.0-2.0)

## Tested Models

### Models Requiring temperature=1.0

| Model | Accepts 0.7 | Accepts 1.0 | Recommendation |
|-------|-------------|-------------|----------------|
| o1 | ❌ | ✅ | USE_1.0 |
| o1-2024-12-17 | ❌ | ✅ | USE_1.0 |
| o3 | ❌ | ✅ | USE_1.0 |
| o3-2025-04-16 | ❌ | ✅ | USE_1.0 |
| o3-mini | ❌ | ✅ | USE_1.0 |
| o3-mini-2025-01-31 | ❌ | ✅ | USE_1.0 |
| o4-mini | ❌ | ✅ | USE_1.0 |
| o4-mini-2025-04-16 | ❌ | ✅ | USE_1.0 |
| gpt-5 | ❌ | ✅ | USE_1.0 |
| gpt-5-2025-08-07 | ❌ | ✅ | USE_1.0 |
| gpt-5-mini | ❌ | ✅ | USE_1.0 |
| gpt-5-mini-2025-08-07 | ❌ | ✅ | USE_1.0 |
| gpt-5-nano | ❌ | ✅ | USE_1.0 |
| gpt-5-nano-2025-08-07 | ❌ | ✅ | USE_1.0 |
| gpt-5.1-chat-latest | ❌ | ✅ | USE_1.0 |
| gpt-5.2-chat-latest | ❌ | ✅ | USE_1.0 |
| gpt-5.3-chat-latest | ❌ | ✅ | USE_1.0 |

### Models Accepting Flexible Temperature (0.7 works)

All standard GPT models accept flexible temperature values:
- gpt-3.5-turbo (all variants)
- gpt-4 (all variants)
- gpt-4-turbo (all variants)
- gpt-4o (all variants)
- gpt-4o-mini (all variants)
- gpt-4.1 (all variants)
- gpt-5-chat-latest
- gpt-5.2, gpt-5.2-2025-12-11
- gpt-5.4, gpt-5.4-2026-03-05

### Models Requiring Temperature Omission

Search-preview models do not accept temperature parameter:
- gpt-4o-mini-search-preview
- gpt-4o-search-preview
- gpt-5-search-api

## Implementation

The `adjust_temperature_for_model()` function in `src/providers/openai.rs` automatically adjusts temperature to 1.0 for reasoning models while preserving user-specified values for standard models.

## Testing Methodology

Models were tested with:
1. No temperature parameter (baseline)
2. temperature=0.7 (common default)
3. temperature=1.0 (reasoning model requirement)

Results were validated against actual OpenAI API responses.

## References

- OpenAI API Documentation: https://platform.openai.com/docs/api-reference/chat
- Related Issue: Temperature errors with o1/o3/gpt-5 models
