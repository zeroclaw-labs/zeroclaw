pub use zeroclaw_config::policy::*;
// `SandboxPolicy` and its resolver live in `zeroclaw-config` so both the call site
// that passes a resolved policy to `create_sandbox` (this crate) and the app-layer
// path guard (`SecurityPolicy::from_profiles`, zeroclaw-config) derive from the
// identical resolution instead of reading two different resolutions of the same
// config.
pub use zeroclaw_config::sandbox_policy::SandboxPolicy;
