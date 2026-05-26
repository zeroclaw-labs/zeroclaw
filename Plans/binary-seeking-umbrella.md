# Bot Workspace Isolation Plan

## Context

Each bot in ZeroClaw currently shares a single workspace, identity, soul, memory, and gateway port. Ricardo needs each bot isolated into its own workspace with its own soul/context, and each bot visible on the dashboard as a separate service with its own port. Currently `DelegateAgentConfig` only has provider/model/system_prompt fields — no workspace, identity, soul, port, or channels. The daemon spawns one gateway and one channel runtime for everything.

## Approach: New `BotConfig` + Per-Bot Daemon Spawning

### Phase 1: Config Schema (`src/config/schema.rs`)

Add `BotConfig` struct with optional overrides:

```rust
pub struct BotConfig {
    pub name: Option<String>,
    pub workspace_dir: Option<PathBuf>,    // default: <global>/bots/<bot_id>/
    pub identity: Option<IdentityConfig>,
    pub soul: Option<SoulConfig>,
    pub port: u16,                          // required, unique per bot
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f64>,
    pub system_prompt: Option<String>,
    pub channels: ChannelsConfig,           // bot's own channels
    pub memory: Option<MemoryConfig>,
}
```

Add to `Config`:
```rust
#[serde(default)]
pub bots: HashMap<String, BotConfig>,
```

Add `Config::resolve_bot_config(&self, bot_id: &str) -> Config` — merges BotConfig into a full Config, falling back to global defaults for `None` fields.

Validate all bot ports are unique at config load time (fail fast).

**Backward compat**: Empty `bots` map = single-bot mode (unchanged behavior).

### Phase 2: Daemon Per-Bot Spawning (`src/daemon/mod.rs`)

In `run()`, after existing global gateway/channels spawn:

```
for each (bot_id, bot_config) in config.bots:
    resolved = config.resolve_bot_config(bot_id)
    mkdir -p resolved.workspace_dir
    spawn_component_supervisor("gateway-{bot_id}", run_gateway(host, bot_port, resolved))
    spawn_component_supervisor("channels-{bot_id}", start_channels(resolved))
```

Use `Box::leak` for dynamic `&'static str` names (acceptable for long-lived daemon).

### Phase 3: Memory Isolation (verify only)

`start_channels()` already scopes memory via `config.workspace_dir`. Since each bot's resolved config has a different workspace_dir, each gets its own SQLite. Verify `create_memory_with_storage()` uses workspace-relative paths — if hardcoded, add scoping.

### Phase 4: Auto-Register in Control Store (`src/daemon/mod.rs`)

After spawning each bot, call `store.upsert_bot()` with bot's id, name, host, port, status="online", channels list. The global control store tracks all bots.

### Phase 5: Dashboard (`src/gateway/dashboard.rs`)

Enrich `rBots()`:
- Per-bot status indicator (green/red based on heartbeat recency)
- "Gateway URL" column: clickable `http://{host}:{port}`
- Workspace path column
- Identity/soul configured indicators

### Phase 6: Per-Bot Heartbeat (`src/daemon/mod.rs`)

Spawn lightweight `tokio::spawn` per bot — interval timer updating `last_heartbeat` and `uptime_secs` in control store.

## Config Example

```toml
[bots.support-agent]
name = "Support Agent"
port = 3001
system_prompt = "You are a customer support agent."

[bots.support-agent.identity]
format = "aieos"
aieos_path = "support-identity.json"

[bots.support-agent.soul]
enabled = true
soul_path = "SOUL-support.md"

[bots.support-agent.channels.telegram]
bot_token = "TOKEN_SUPPORT"
allowed_users = ["*"]

[bots.sales-agent]
name = "Sales Agent"
port = 3002

[bots.sales-agent.channels.discord]
bot_token = "TOKEN_SALES"
guild_id = "123456"
```

## Critical Files

| File | Changes |
|------|---------|
| `src/config/schema.rs` | Add `BotConfig`, `bots` field on `Config`, `resolve_bot_config()` |
| `src/daemon/mod.rs` | Per-bot spawn loop, workspace creation, auto-registration, heartbeat |
| `src/gateway/dashboard.rs` | Enrich `rBots()` with per-bot URLs, status, workspace |
| `src/control/store.rs` | Possibly add `workspace_dir` column to Bot (already has port/host) |
| `src/channels/mod.rs` | Verify workspace scoping in `start_channels()` (likely no changes) |

## Sequencing

1. Phase 1 (Config) — additive, no runtime impact
2. Phase 2 (Daemon) — depends on Phase 1
3. Phase 3 (Memory verify) — parallel with Phase 2
4. Phase 4 (Registration) — depends on Phase 2
5. Phase 5 (Dashboard) — depends on Phase 4
6. Phase 6 (Heartbeat) — depends on Phase 4

## Risks

- **Port conflicts**: Validate uniqueness at config load, fail fast
- **Shared control store**: WAL mode handles concurrent heartbeat writes
- **Config explosion**: All BotConfig fields optional with global fallback

## Verification

1. `cargo build` — clean compile
2. `cargo clippy -D warnings` — zero warnings
3. `cargo test` — all existing tests pass
4. Manual: configure 2 bots on different ports, verify both gateways respond
5. Manual: dashboard shows both bots with correct ports/status
6. Manual: each bot has its own workspace dir with separate memory.db
