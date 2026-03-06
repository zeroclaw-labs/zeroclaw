pub mod docker;
pub mod native;
pub mod traits;
pub mod wasm;

pub use docker::DockerRuntime;
pub use native::NativeRuntime;
pub use traits::RuntimeAdapter;
pub use wasm::{WasmCapabilities, WasmRuntime, WasmRuntimeConfig};

use crate::config::RuntimeConfig;

impl From<crate::config::WasmSecurityConfig> for wasm::WasmSecurityConfig {
    fn from(s: crate::config::WasmSecurityConfig) -> Self {
        Self {
            require_workspace_relative_tools_dir: s.require_workspace_relative_tools_dir,
            module_hash_policy: match s.module_hash_policy {
                crate::config::WasmModuleHashPolicy::Disabled => {
                    wasm::WasmModuleHashPolicy::Disabled
                }
                crate::config::WasmModuleHashPolicy::Warn => wasm::WasmModuleHashPolicy::Warn,
                crate::config::WasmModuleHashPolicy::Enforce => {
                    wasm::WasmModuleHashPolicy::Enforce
                }
            },
            module_sha256: s.module_sha256,
            strict_host_validation: s.strict_host_validation,
            capability_escalation_mode: match s.capability_escalation_mode {
                crate::config::WasmCapabilityEscalationMode::Deny => {
                    wasm::WasmCapabilityEscalationMode::Deny
                }
                crate::config::WasmCapabilityEscalationMode::Clamp => {
                    wasm::WasmCapabilityEscalationMode::Clamp
                }
            },
            reject_symlink_tools_dir: s.reject_symlink_tools_dir,
            reject_symlink_modules: s.reject_symlink_modules,
        }
    }
}

impl From<crate::config::WasmRuntimeConfig> for WasmRuntimeConfig {
    fn from(c: crate::config::WasmRuntimeConfig) -> Self {
        Self {
            fuel_limit: c.fuel_limit,
            memory_limit_mb: c.memory_limit_mb,
            max_module_size_mb: c.max_module_size_mb,
            tools_dir: c.tools_dir,
            allowed_hosts: c.allowed_hosts,
            allow_workspace_read: c.allow_workspace_read,
            allow_workspace_write: c.allow_workspace_write,
            security: c.security.into(),
        }
    }
}

/// Factory: create the right runtime from config
pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind.as_str() {
        "native" => Ok(Box::new(NativeRuntime::new())),
        "docker" => Ok(Box::new(DockerRuntime::new(config.docker.clone()))),
        "wasm" => Ok(Box::new(WasmRuntime::new(config.wasm.clone().into()))),
        "cloudflare" => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
        other if other.trim().is_empty() => {
            anyhow::bail!("runtime.kind cannot be empty. Supported values: native, docker, wasm")
        }
        other => {
            anyhow::bail!("Unknown runtime kind '{other}'. Supported values: native, docker, wasm")
        }
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
