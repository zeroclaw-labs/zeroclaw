// bins/engineer/src/main.rs — osAgent engineer binary (Phase 1.2 placeholder)
//
// This is intentionally a hollow placeholder per the M1 "hollow-wizard" pattern
// documented in .planning/research/FEATURES.md. The point of Phase 1.2 is the
// Cargo.toml topology + workspace shape, not the runtime behaviour. M2 fills
// this binary with real engineer functionality:
//
//   - osagent-runtime daemon loop
//   - osagent-bridge tool (native AMQP-mTLS to operator)
//   - osagent-exchange channel (PLAN/MISSION/REPORT files)
//   - osagent-channels (Telegram + Slack + Mattermost + Matrix + WhatsApp-Cloud + Signal)
//   - osagent-tools-mcp (the engineer-only MCP server registry — wizard does NOT depend on this crate)
//   - osagent-lifecycle, osagent-audit, osagent-codeword, ...
//
// The Cargo.toml above is the human-readable manifest of what the engineer
// binary is allowed to compile in. Audit it directly to see what changed.

fn main() {
    println!(
        "osAgent: {} v{} (engineer; Phase 1.2 placeholder, M2 fills runtime)",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );
    println!("forked-from: zeroclaw-labs/zeroclaw v0.7.5");
    println!("status: hollow — does nothing yet; binary exists to ratify the workspace shape.");
}
