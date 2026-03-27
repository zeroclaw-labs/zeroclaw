pub mod docker;
pub mod native;
pub mod traits;
pub mod wasm;

pub use docker::DockerRuntime;
pub use native::NativeRuntime;
pub use traits::RuntimeAdapter;

use crate::config::RuntimeConfig;

fn supported_runtime_kinds() -> &'static str {
    #[cfg(feature = "runtime-wasm")]
    {
        "native, docker, wasm"
    }
    #[cfg(not(feature = "runtime-wasm"))]
    {
        "native, docker"
    }
}

/// Factory: create the right runtime from config
pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind.as_str() {
        "native" => Ok(Box::new(NativeRuntime::new())),
        "docker" => Ok(Box::new(DockerRuntime::new(config.docker.clone()))),
        "wasm" => {
            #[cfg(feature = "runtime-wasm")]
            {
                Ok(Box::new(WasmRuntime::new(config.wasm.clone())))
            }
            #[cfg(not(feature = "runtime-wasm"))]
            {
                anyhow::bail!(
                    "runtime.kind='wasm' requires the 'runtime-wasm' cargo feature. Supported values in this build: {}",
                    supported_runtime_kinds()
                )
            }
        }
        "cloudflare" => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
        other if other.trim().is_empty() => {
            anyhow::bail!(
                "runtime.kind cannot be empty. Supported values: {}",
                supported_runtime_kinds()
            )
        }
        other => anyhow::bail!(
            "Unknown runtime kind '{other}'. Supported values: {}",
            supported_runtime_kinds()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_native() {
        let cfg = RuntimeConfig {
            kind: "native".into(),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "native");
        assert!(rt.has_shell_access());
    }

    #[test]
    fn factory_docker() {
        let cfg = RuntimeConfig {
            kind: "docker".into(),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "docker");
        assert!(rt.has_shell_access());
    }

    #[cfg(feature = "runtime-wasm")]
    #[test]
    fn factory_wasm() {
        let cfg = RuntimeConfig {
            kind: "wasm".into(),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "wasm");
        assert!(!rt.has_shell_access());
    }

    #[cfg(not(feature = "runtime-wasm"))]
    #[test]
    fn factory_wasm_errors_without_feature() {
        let cfg = RuntimeConfig {
            kind: "wasm".into(),
            ..RuntimeConfig::default()
        };
        match create_runtime(&cfg) {
            Err(err) => assert!(err.to_string().contains("runtime-wasm")),
            Ok(_) => panic!("wasm runtime should require feature"),
        }
    }

    #[test]
    fn factory_cloudflare_errors() {
        let cfg = RuntimeConfig {
            kind: "cloudflare".into(),
            ..RuntimeConfig::default()
        };
        match create_runtime(&cfg) {
            Err(err) => assert!(err.to_string().contains("not implemented")),
            Ok(_) => panic!("cloudflare runtime should error"),
        }
    }

    #[test]
    fn factory_unknown_errors() {
        let cfg = RuntimeConfig {
            kind: "wasm-edge-unknown".into(),
            ..RuntimeConfig::default()
        };
        match create_runtime(&cfg) {
            Err(err) => assert!(err.to_string().contains("Unknown runtime kind")),
            Ok(_) => panic!("unknown runtime should error"),
        }
    }

    #[test]
    fn factory_empty_errors() {
        let cfg = RuntimeConfig {
            kind: String::new(),
            ..RuntimeConfig::default()
        };
        match create_runtime(&cfg) {
            Err(err) => assert!(err.to_string().contains("cannot be empty")),
            Ok(_) => panic!("empty runtime should error"),
        }
    }
}
