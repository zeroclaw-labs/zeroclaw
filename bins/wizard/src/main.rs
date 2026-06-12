// bins/wizard/src/main.rs — osAgent wizard binary (Phase 1.2 placeholder)
//
// LOAD-BEARING SAFETY PROPERTY: the wizard binary must contain ZERO MCP code.
// This is enforced structurally by Cargo.toml (no dependency edge to
// osagent-tools-mcp) and by a 4-layer CI gate (`wizard-no-mcp-gate`) that
// fails any build whose ELF contains MCP symbols.
//
// See the comment block at the top of bins/wizard/Cargo.toml for the full
// rationale.
//
// This is intentionally a hollow placeholder at Phase 1.2. The point of this
// phase is the workspace shape, not the runtime behaviour. M3 fills this
// binary with the real wizard functionality:
//
//   - osagent-runtime daemon loop
//   - osagent-vault (idempotency keys, customer-prefix path enforcement)
//   - osagent-2person-approval (dashboard ack + chat ack from distinct identities)
//   - osagent-bootstrap-secret (sealed plaintext fallback when Vault unreachable)
//   - osagent-subagent (markdown frontmatter, pool cost, depth=1, signed provenance)
//   - osagent-channels (Telegram + Slack + Mattermost + Matrix + WhatsApp-Cloud + Signal + dashboard WS)
//   - osagent-tools (excluding MCP — that crate is not even in this binary's dep tree)

fn main() {
    println!(
        "osAgent: {} v{} (wizard; Phase 1.2 placeholder, M3 fills runtime)",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );
    println!("forked-from: zeroclaw-labs/zeroclaw v0.7.5");
    println!("status: hollow — does nothing yet; binary exists to ratify the workspace shape.");
    println!("safety: this binary contains zero MCP code (structurally excluded; CI-enforced).");
}
