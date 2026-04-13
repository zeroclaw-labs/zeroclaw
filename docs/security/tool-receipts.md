# Tool Execution Receipts

## Overview

Tool receipts are cryptographic HMAC-SHA256 signatures that prove a tool actually executed. When enabled, every successful tool execution produces a receipt that the LLM cannot forge — because the signing key is ephemeral, per-session, and never exposed to the model.

This addresses a class of LLM failure where the model claims to have used a tool (or denies having used one) without any independent verification. Receipts create ground truth about what actually ran.

Based on: Basu, A. (2026). "Tool Receipts, Not Zero-Knowledge Proofs: Practical Hallucination Detection for AI Agents." [arXiv:2603.10060](https://doi.org/10.48550/arXiv.2603.10060).

---

## Configuration

```toml
[agent.tool_receipts]
enabled = true           # Generate HMAC receipts for tool executions (default: false)
show_in_response = true  # Append receipts to user-visible messages (default: false)
```

Both options default to `false` — no behavioral change for existing users.

---

## How it works

1. When the agent loop starts, an ephemeral 256-bit key is generated (never logged, never sent to the LLM).
2. After each successful tool execution, the runtime computes:
   ```
   receipt = HMAC-SHA256(key, tool_name | args | result | timestamp)
   ```
3. The receipt is appended to the tool result as `[receipt: zc-receipt-{timestamp}-{hash}]` before the result is returned to the LLM.
4. The system prompt instructs the LLM to preserve receipts verbatim when referencing tool results.

### Receipt format

```
zc-receipt-1774608496-gzpEBuUIRYX1vd4fQl4oYkqhq4-GnoJDStmlYzvQiWA
          ^timestamp  ^base64url-encoded HMAC-SHA256 digest
```

The `zc-receipt-` prefix distinguishes real receipts from fabricated ones. The LLM cannot compute a valid HMAC because it doesn't know the session key and cannot perform the math.

---

## What receipts detect

| Scenario | Without receipts | With receipts |
|----------|-----------------|---------------|
| LLM claims it ran a tool but didn't | Undetectable | No receipt exists — fabrication detected |
| LLM fabricates a tool result | Undetectable | HMAC won't match — tampering detected |
| LLM denies running tools it actually ran | Unverifiable | Receipts in log prove execution |
| LLM fabricates a receipt string | Plausible-looking | HMAC verification fails — forgery detected |

### What receipts don't prevent

- The LLM can still say anything in its text output — receipts don't suppress responses.
- The LLM can answer questions without using tools at all. Receipts only verify tool calls that were made, not tool calls that should have been made.

---

## Viewing receipts

### In debug logs

```bash
RUST_LOG=zeroclaw::agent=debug zeroclaw daemon
```

Look for:
```
Tool receipt generated tool=shell receipt=zc-receipt-1774604899-fVRG...
```

### In user-visible messages

When `show_in_response = true`, the bot's response includes:

```
Here's the weather in Istanbul: 16°C, sunny.

---
Tool receipts:
  weather: zc-receipt-1774608496-gzpEBuUIRYX1vd4fQl4oYkqhq4-GnoJDStmlYzvQiWA
```

### Inline in LLM responses

The system prompt instructs the LLM to echo receipts when referencing tool results. These appear inline in the response. The leak detector is configured to NOT redact `zc-receipt-` tokens.

### LLM-echoed receipt blocks

The LLM may independently include a `Tool receipts:` block in its response text — it sees receipts in conversation history and can reproduce them. This is separate from the system-appended receipts block. The behavior can be controlled via system prompt instructions in `AGENTS.md` by telling the model whether or not to include tool receipts in its output. If both the LLM and the system append receipts, the user may see duplicate blocks.

---

## Security properties

- **Ephemeral keys**: A new key is generated for each agent session. Keys are never persisted, logged, or sent to the LLM.
- **HMAC-SHA256**: Standard cryptographic MAC. The digest binds the tool name, arguments, result, and timestamp together — changing any input invalidates the receipt.
- **No new dependencies**: Uses `hmac`, `sha2`, `ring`, and `base64` — all already in the dependency tree.
- **No performance impact**: Receipt generation adds <1ms per tool call (HMAC computation is negligible).

---

## Current limitations

- **Passive only**: Receipts are generated and logged but not validated against LLM responses. The system does not block responses with missing or invalid receipts.
- **No persistent audit**: Receipts are in debug logs and conversation history but not stored in a queryable database.
- **No cross-session verification**: Ephemeral keys mean receipts cannot be verified after the session ends.
- **Config activation pending**: The `[agent.tool_receipts]` config section is not yet wired. Receipts are currently controlled programmatically via the `ReceiptGenerator` API. Config-driven activation is tracked as a follow-up.

---

## Related docs

- [Audit Logging](audit-logging.md) — broader audit trail proposal
- [Agnostic Security](agnostic-security.md) — security model overview
- [Config Reference](../reference/api/config-reference.md) — full config options
