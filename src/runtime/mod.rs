pub mod native;
pub mod traits;

pub use native::NativeRuntime;
pub use traits::RuntimeAdapter;

use crate::config::RuntimeConfig;

/// Factory: create the right runtime from config
pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind.as_str() {
        "native" | "" => Ok(Box::new(NativeRuntime::new())),
        other => anyhow::bail!("Unknown runtime kind '{other}'. Augusta only supports 'native'."),
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
    fn factory_empty_defaults_to_native() {
        let cfg = RuntimeConfig {
            kind: String::new(),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "native");
    }

    #[test]
    fn factory_unknown_errors() {
        let cfg = RuntimeConfig {
            kind: "wasm-edge-unknown".into(),
            ..RuntimeConfig::default()
        };
        assert!(create_runtime(&cfg).is_err());
    }
}
