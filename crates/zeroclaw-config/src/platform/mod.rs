pub mod docker;
pub mod native;

pub use docker::DockerRuntime;
pub use native::NativeRuntime;
pub use zeroclaw_api::runtime_traits::RuntimeAdapter;

use crate::schema::RuntimeConfig;

pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind.as_str() {
        "native" => Ok(Box::new(NativeRuntime::new())),
        "docker" => Ok(Box::new(DockerRuntime::new(config.docker.clone()))),
        "cloudflare" => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
        other if other.trim().is_empty() => {
            anyhow::bail!("runtime.kind cannot be empty. Supported values: native, docker")
        }
        other => anyhow::bail!("Unknown runtime kind '{other}'. Supported values: native, docker"),
    }
}
