# Gemma 4 Thinking Mode & Function Calling Reference

Category: concept
Created: 2026-04-09
Last updated: 2026-04-09
Related: [[zeroclaw]], [[vllm]], [[litellm]]

## Summary

Gemma 4's thinking mode and function calling have specific requirements for thought
management, tool response formatting, and content safety (no chevrons). This page
captures the key constraints discovered during the Qwen 3.5 → Gemma 4 migration.

## Thinking Mode Activation

Include `<|think|>` in system instruction. vLLM handles this via `--reasoning-parser gemma4`.
The model uses a `thought` channel for internal reasoning, separated from visible content.

## Reasoning + Function Calling Lifecycle

1. User asks question
2. Model thinks privately (thought channel)
3. Model halts generation, requests tool call
4. Application executes tool, appends response
5. Model reads response, generates final answer

## Thought Stripping Rules

### Standard turns
Strip thoughts from previous turn before passing history back. Without this, the model
receives its own prior reasoning as input, which degrades quality.
ZeroClaw setting: `strip_prior_reasoning = true`

### Function calling exception
During a SINGLE turn with tool calls, thoughts must NOT be removed between calls.
This allows the model to maintain reasoning context across multi-tool sequences.

### Agentic long-running tasks
For agents that run many turns, summarize and re-inject previous thoughts as standard
text to prevent cyclical reasoning loops. There is no strict format expected — the model
accepts summarized reasoning in whatever format the application provides.

## Tool Response Format

Native Gemma 4 format:
`response:function_name{key:value,key2:string_value}`

Characters that collide with this syntax:
- `{` `}` — structural delimiters
- `:` — key-value separator
- `"` — conflicts with string delimiter tokens

ZeroClaw uses pipe-delimited `key=value | key=value` format via `flatten_json_responses`
to avoid all three collision characters.

## Chevron Sensitivity

Any `<` or `>` in prompts, tool descriptions, retry prompts, or skill files collides with
Gemma's `<|...|>` template delimiters. This causes 400 errors or garbage output.

### Known incidents
- Retry prompt contained `<tool_call>...</tool_call>` → 400 Bad Request on every retry
- Old system prompts with `<!-- -->` HTML comments → garbage output

### Prevention
All text reaching the model must be chevron-free. Verified during the skill rewrite process.

## ZeroClaw Configuration for Gemma 4

```toml
strip_prior_reasoning = true

[presentation]
flatten_json_responses = true
simplify_tool_schemas = true
show_reasoning = true
```

## Open Questions

- Gap: should ZeroClaw implement thought summarization for long-running agentic sessions?
- Gap: how does vLLM's reasoning parser interact with strip_prior_reasoning? Does vLLM strip
  thoughts server-side, making the ZeroClaw-side strip redundant?
