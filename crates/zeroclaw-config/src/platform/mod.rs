pub mod docker;
pub mod native;

pub use docker::DockerRuntime;
pub use native::NativeRuntime;
pub use zeroclaw_api::runtime_traits::RuntimeAdapter;

use crate::schema::{RuntimeConfig, RuntimeKind};

pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind {
        RuntimeKind::Native => Ok(Box::new(NativeRuntime::new())),
        RuntimeKind::Docker => Ok(Box::new(DockerRuntime::new(config.docker.clone()))),
        RuntimeKind::Cloudflare => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
    }
}
