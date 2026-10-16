//! ZeroClaw A2A protocol core.
//!
//! Consolidates the A2A (Agent2Agent) protocol pieces previously spread across
//! `zeroclaw-api` (wire), `zeroclaw-tools` (client + tools), and
//! `zeroclaw-gateway` (inbound server) into one protocol crate.
//!
//! Layout mirrors the internal SDK's protocol layer:
//! - `a2a::a2a_wire` — A2A v1.0 wire model
//! - `a2a_client`    — outbound HTTP client
//! - `a2a_server`    — inbound surface: `A2aServerState` + `a2a_router`,
//!   `A2aExecutor`, `AgentCardProducer`, spec error shapes
//!
//! Streaming and task-store management are intentionally out of scope: this
//! crate holds exactly what ZeroClaw's current A2A feature set needs.
//!
//! Convenience re-exports keep the `zeroclaw_a2a::wire` / `zeroclaw_a2a::client`
//! paths used by `zeroclaw-tools` and `zeroclaw-gateway`.

pub mod a2a;
pub mod a2a_client;
pub mod a2a_server;

pub use a2a::a2a_wire as wire;
pub use a2a_client::a2a_client as client;
pub use a2a_client::config::{
    A2aClientConfig, A2aPeerConfig, A2aPeerSource, A2aSettings, LiveA2aConfig, ProxyPolicy,
    StaticA2aPeerSource, live_config,
};
