# ZeroClaw TOTP Integration Guide

## Step 1: Copy module into ZeroClaw

```bash
cp -r src/security/totp/ /path/to/zeroclaw/src/security/totp/
```

## Step 2: Add to src/security/mod.rs

```rust
pub mod totp;
```

## Step 3: Add dependencies to Cargo.toml

```toml
sha1 = "0.10"
data-encoding = "2.6"
hkdf = "0.12"
zeroize = { version = "1", features = ["derive"] }
secrecy = "0.10"
```

## Step 4: Add [security.totp] to config.toml schema

In `src/config/schema.rs`, add `TotpConfig` to the main `Config` struct.

## Step 5: Wire into tool execution pipeline

In `src/tools/mod.rs` or `src/agent/loop_.rs`, before every tool execution:

```rust
let signed_decision = totp_gate.evaluate(user_id, &command, &context);
match signed_decision.decision {
    GateDecision::Allowed => { /* proceed */ }
    GateDecision::TotpRequired { reason } => {
        // prompt user for TOTP code
        // verify with totp::core::verify_code()
    }
    GateDecision::Blocked { reason } => {
        // reject the command
    }
    GateDecision::QueuedForApproval { reason } => {
        // add to approval_queue
    }
    // ... other variants
}
```

## Step 6: Verify in execution engine

Right before actually running the command:

```rust
if !totp_gate.verify_decision(&signed_decision, &actual_command) {
    // TOCTOU detected! Command was modified between gate and execution.
    // Reject and audit-log.
}
```

## Hook points in ZeroClaw source

- `src/security/policy.rs:82-95` — SecurityPolicy struct
- `src/security/policy.rs:689` — validate_command_execution()
- `src/approval/mod.rs:113-151` — needs_approval()
- `src/tools/mod.rs:89-238` — Tool trait + all_tools() registry
- `src/config/schema.rs:2094-2193` — AutonomyConfig
- `src/main.rs:136-145` — E-Stop state machine
