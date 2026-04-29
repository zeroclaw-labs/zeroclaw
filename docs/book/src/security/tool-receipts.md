# Tool Receipts

Tool receipts are cryptographic proofs that a tool actually ran. Every tool invocation — approved, blocked, or auto-approved — produces an HMAC-SHA256 digest over the call and its result. The digest is appended to the tool-result text and passed back to the model as part of the conversation.

The practical outcome: the model cannot claim to have run a tool it didn't run, and it cannot fabricate a tool result. Both produce receipt mismatches the runtime detects.

## The threat model

An LLM is a string generator. By default, nothing prevents it from narrating a tool call it never made ("I ran `git log` and the latest commit is…"), or inventing a result for a tool call ("The weather API says 72°F" — when the call timed out). For an agent with autonomy, this is more than a correctness issue — it's a deniability issue.

Tool receipts close that gap with the cheapest possible construct: a symmetric MAC with an ephemeral per-session key.

Based on: Basu, A. (2026). "Tool Receipts, Not Zero-Knowledge Proofs: Practical Hallucination Detection for AI Agents." [arXiv:2603.10060](https://doi.org/10.48550/arXiv.2603.10060).

## How it works

1. At agent-loop startup, a 256-bit key is generated. It's ephemeral — never written to disk, never sent to the model, never logged.
2. After each tool invocation, the runtime computes:
   ```
   receipt = HMAC-SHA256(key, tool_name || args || result || timestamp)
   ```
3. The receipt is appended to the tool-result text as:
   ```
   [receipt: zc-receipt-<timestamp>-<base64url-digest>]
   ```
4. The tool result (with the receipt) is fed back to the model.

The model sees every receipt in its conversation history. It can echo them in text it produces to the user. But it cannot produce a *new* valid receipt — the HMAC requires the session key, which the model doesn't have.

### Receipt shape

```
zc-receipt-1774608496-gzpEBuUIRYX1vd4fQl4oYkqhq4-GnoJDStmlYzvQiWA
          ^ epoch seconds     ^ base64url(HMAC-SHA256 digest)
```

The `zc-receipt-` prefix exists so the leak detector doesn't redact them (receipts are safe to surface; they contain no secret material).

## What receipts detect

| Scenario | Without receipts | With receipts |
|---|---|---|
| Model claims it ran a tool, didn't | Undetectable | No receipt — fabrication visible |
| Model fabricates a result for a real call | Undetectable | HMAC mismatches on verification |
| Model denies a call it did make | Unverifiable | Receipt in log proves it |
| Model fabricates a plausible receipt string | Plausible | HMAC verification fails |

### What receipts don't do

- **Don't constrain text output.** The model can still say things unrelated to any tool call.
- **Don't force tool use.** Receipts are only generated when a tool is called; they don't help with "the model answered from prior knowledge when it should have looked something up".
- **Don't travel across sessions.** Ephemeral keys mean a receipt from session A can't be verified in session B.

## Viewing receipts

### In debug logs

```bash
RUST_LOG=zeroclaw::agent=debug zeroclaw daemon
```

Produces:

```
DEBUG Tool receipt generated tool=shell receipt=zc-receipt-1774604899-fVRG...
```

### In user-visible replies

If `[agent.tool_receipts] show_in_response = true`, the reply includes a trailing block:

```
Here's the weather in Istanbul: 16°C, sunny.

---
Tool receipts:
  weather: zc-receipt-1774608496-gzpEBuUIRYX1vd4fQl4oYkqhq4-GnoJDStmlYzvQiWA
```

### In the LLM's own output

Because the model sees receipts in its context, it may echo them when describing tool results. The leak detector is configured to pass `zc-receipt-*` tokens through unmodified so this echoing works. If both the runtime and the model include a receipts block, the user sees two — strip one via channel-specific formatting rules.

## Configuration

```toml
[agent.tool_receipts]
enabled = true
show_in_response = false    # append trailing "Tool receipts:" block
inject_system_prompt = true # instruct the model to echo receipts verbatim
```

## Security properties

- **Ephemeral key per session.** Never persisted, never logged, never in the model's context. Compromising long-term storage gains nothing.
- **Standard MAC primitives.** `hmac` + `sha2` from the Rust ecosystem.
- **Negligible overhead.** <1 ms per tool call.
- **No new external dependencies.**

## What receipts are *not*

- Not ZK proofs. The runtime can verify receipts because it holds the key. A third party cannot.
- Not cross-signed with the conversation hash. Tampering with the prior conversation doesn't invalidate subsequent receipts (the receipt only covers the call it was computed for).
- Not a replacement for approval gates. A receipt proves a call happened; it doesn't decide whether it should have.

## Current state

| Feature | Status |
|---|---|
| HMAC generation per call | Shipped |
| Receipt appended to tool result | Shipped |
| Debug log of receipts | Shipped |
| `show_in_response` | Shipped |
| System-prompt instruction to echo receipts | Shipped |
| Persistent audit database of receipts | Planned |
| Cross-session receipt verification | Not planned (see ephemeral-key design) |

## See also

- [Security → Overview](./overview.md)
- [Autonomy levels](./autonomy.md) — the policy layer that decides whether a receipt-worthy call happens
- [Reference → Config](../reference/config.md) — generated config reference
