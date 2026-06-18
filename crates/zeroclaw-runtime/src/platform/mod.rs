pub use zeroclaw_config::platform::*;

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{RuntimeConfig, RuntimeKind};

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
}
