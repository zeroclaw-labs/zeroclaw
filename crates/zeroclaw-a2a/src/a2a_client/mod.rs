//! A2A outbound client (protocol layer).

// The implementation file is named `a2a_client.rs` to mirror the pre-extraction
// path (`zeroclaw_tools::a2a_client`); the module is folded into this `a2a_client`
// module so the external path stays `zeroclaw_a2a::a2a_client`. The same-named
// child module is intentional.
#[allow(clippy::module_inception)]
pub mod a2a_client;
pub mod config;

pub use a2a_client::*;
pub use config::{
    A2aClientConfig, A2aPeerConfig, A2aPeerSource, A2aSettings, LiveA2aConfig, ProxyPolicy,
    StaticA2aPeerSource, live_config,
};
