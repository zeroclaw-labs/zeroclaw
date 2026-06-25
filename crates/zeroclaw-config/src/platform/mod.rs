pub mod docker;
pub mod native;

pub use docker::DockerRuntime;
pub use native::NativeRuntime;
pub use zeroclaw_api::runtime_traits::RuntimeAdapter;

use crate::schema::{RuntimeConfig, RuntimeKind};

pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind {
        RuntimeKind::Native => {
            let shell = config.shell.clone().unwrap_or_else(|| "sh".into());
            Ok(Box::new(NativeRuntime::with_shell(shell)))
        }
        RuntimeKind::Docker => Ok(Box::new(DockerRuntime::new(config.docker.clone()))),
        RuntimeKind::Cloudflare => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RuntimeConfig, RuntimeKind};

    #[test]
    fn factory_native() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "native");
        assert!(rt.has_shell_access());
    }

    #[test]
    fn factory_docker() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Docker,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "docker");
        assert!(rt.has_shell_access());
    }

    #[test]
    fn factory_cloudflare_errors() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Cloudflare,
            ..RuntimeConfig::default()
        };
        match create_runtime(&cfg) {
            Err(err) => assert!(err.to_string().contains("not implemented")),
            Ok(_) => panic!("cloudflare runtime should error"),
        }
    }

    #[test]
    fn unknown_runtime_kind_loads_as_native() {
        let parsed: RuntimeConfig = toml::from_str("kind = \"wasm-edge-unknown\"").unwrap();
        assert_eq!(parsed.kind, RuntimeKind::Native);
        let empty: RuntimeConfig = toml::from_str("kind = \"\"").unwrap();
        assert_eq!(empty.kind, RuntimeKind::Native);
    }

    #[test]
    fn factory_native_with_custom_shell() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some("bash".into()),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "native");
        let cmd = rt
            .build_shell_command("echo hi", &std::env::temp_dir())
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(
            debug.contains("bash"),
            "custom shell 'bash' should appear in command, got: {debug}"
        );
    }

    #[test]
    fn factory_native_default_shell_is_sh() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: None,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        let cmd = rt
            .build_shell_command("echo hi", &std::env::temp_dir())
            .unwrap();
        let debug = format!("{cmd:?}");
        #[cfg(not(target_os = "windows"))]
        assert!(
            debug.contains("\"sh\""),
            "default shell should be 'sh', got: {debug}"
        );
    }
}
