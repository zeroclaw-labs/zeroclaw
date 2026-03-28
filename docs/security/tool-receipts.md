# Tool Execution Receipts

HMAC-SHA256 tool execution receipts for hallucination detection.

## Overview

When enabled, every tool execution produces a cryptographic receipt that proves
the tool actually ran. The LLM cannot forge valid receipts because it never
sees the ephemeral session key.

Based on: Basu, A. (2026). "Tool Receipts, Not Zero-Knowledge Proofs:
Practical Hallucination Detection for AI Agents." arXiv:2603.10060

## How it works

1. At session start, an ephemeral 256-bit key is generated via the system CSPRNG.
2. When a tool executes successfully, ZeroClaw computes an HMAC-SHA256 over:
   - Tool name
   - Serialized arguments (JSON)
   - Tool output (after credential scrubbing)
   - Current Unix timestamp
3. The receipt is formatted as `zc-receipt-{timestamp}-{base64url_hash}` and
   appended to the tool result seen by the LLM.
4. The LLM is instructed to include receipts verbatim when referencing tool
   results; a missing or invalid receipt indicates a fabricated tool call.

## Configuration

Add to your `zeroclaw.toml`:

```toml
[agent.tool_receipts]
enabled = true             # Generate HMAC receipts for tool executions
show_in_response = false   # Append receipts to user-visible responses
```

### Fields

| Field              | Type | Default | Description                                      |
|--------------------|------|---------|--------------------------------------------------|
| `enabled`          | bool | false   | Enable HMAC receipt generation                   |
| `show_in_response` | bool | false   | Append receipts to the delivered channel message  |

## Security properties

- **Unforgeability**: The LLM never sees the ephemeral key, so it cannot
  produce a valid receipt for a tool call it did not make.
- **Ephemeral keys**: A new key is generated each session. Compromising one
  session's key does not affect others.
- **Non-interference**: Receipts are stripped by the leak detector's entropy
  scanner so they are never redacted as leaked credentials.

## Verification

Receipts can be verified programmatically using the `ReceiptGenerator::verify`
method with the same ephemeral key. This is useful for audit logging and
automated hallucination detection pipelines.

## Limitations

- Receipts prove execution happened, not that the output is semantically correct.
- The ephemeral key exists only in memory; it is lost on process restart.
- Receipt verification requires access to the session's ephemeral key.
