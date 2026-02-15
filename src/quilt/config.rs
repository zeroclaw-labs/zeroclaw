use serde::{Deserialize, Serialize};

// ── Default constants ───────────────────────────────────────────────

const DEFAULT_API_URL: &str = "https://backend.quilt.sh";
const DEFAULT_MEMORY_LIMIT_MB: u32 = 4096;
const DEFAULT_CPU_LIMIT_PERCENT: u32 = 100;

// ── Config ──────────────────────────────────────────────────────────

/// Configuration for a Quilt-based sandbox container.
///
/// Resolution order (highest priority first):
/// 1. Agent-specific config (passed directly)
/// 2. Global defaults (from application config)
/// 3. Environment variables
/// 4. Hardcoded defaults
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxQuiltConfig {
    /// Quilt API base URL.
    /// Env: `QUILT_API_URL`, default: `https://backend.quilt.sh`
    pub api_url: String,

    /// Quilt API key (must start with `qlt_`).
    /// Env: `QUILT_API_KEY`
    pub api_key: String,

    /// Memory limit for the sandbox container in megabytes.
    /// Default: 4096
    pub memory_limit_mb: u32,

    /// CPU limit as a percentage (1-100+).
    /// Default: 100
    pub cpu_limit_percent: u32,

    /// Optional shell command to run inside the container after creation
    /// (e.g. installing dependencies, cloning a repo).
    pub setup_command: Option<String>,
}

/// Overrides that can be provided at the agent level.
/// All fields are optional; `None` means "fall through to the next level".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuiltConfigOverrides {
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    pub memory_limit_mb: Option<u32>,
    pub cpu_limit_percent: Option<u32>,
    pub setup_command: Option<String>,
}

/// Global defaults that live in the application-level config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuiltGlobalDefaults {
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    pub memory_limit_mb: Option<u32>,
    pub cpu_limit_percent: Option<u32>,
    pub setup_command: Option<String>,
}

impl SandboxQuiltConfig {
    /// Resolve the final configuration by layering:
    ///   agent overrides > global defaults > env vars > hardcoded defaults.
    ///
    /// Returns `Err` if no API key can be resolved from any source.
    pub fn resolve(
        agent: Option<&QuiltConfigOverrides>,
        global: Option<&QuiltGlobalDefaults>,
    ) -> Result<Self, anyhow::Error> {
        // api_url always has a hardcoded default, so unwrap is safe
        let api_url = Self::resolve_string(
            agent.and_then(|a| a.api_url.as_deref()),
            global.and_then(|g| g.api_url.as_deref()),
            "QUILT_API_URL",
            Some(DEFAULT_API_URL),
        )
        .expect("api_url should always resolve (hardcoded default exists)");

        let api_key = Self::resolve_string(
            agent.and_then(|a| a.api_key.as_deref()),
            global.and_then(|g| g.api_key.as_deref()),
            "QUILT_API_KEY",
            None, // no hardcoded default for the key
        );

        let api_key = api_key.ok_or_else(|| {
            anyhow::anyhow!(
                "Quilt API key not configured. Set QUILT_API_KEY or add it to your config."
            )
        })?;

        let memory_limit_mb = Self::resolve_u32(
            agent.and_then(|a| a.memory_limit_mb),
            global.and_then(|g| g.memory_limit_mb),
            "QUILT_MEMORY_LIMIT_MB",
            DEFAULT_MEMORY_LIMIT_MB,
        );

        let cpu_limit_percent = Self::resolve_u32(
            agent.and_then(|a| a.cpu_limit_percent),
            global.and_then(|g| g.cpu_limit_percent),
            "QUILT_CPU_LIMIT_PERCENT",
            DEFAULT_CPU_LIMIT_PERCENT,
        );

        let setup_command = agent
            .and_then(|a| a.setup_command.clone())
            .or_else(|| global.and_then(|g| g.setup_command.clone()))
            .or_else(|| std::env::var("QUILT_SETUP_COMMAND").ok());

        Ok(Self {
            api_url,
            api_key,
            memory_limit_mb,
            cpu_limit_percent,
            setup_command,
        })
    }

    /// Compute a stable hash of the config fields that affect the container
    /// specification. Used to detect when a container needs rebuilding.
    pub fn config_hash(&self) -> String {
        use std::collections::BTreeMap;
        use std::hash::{Hash, Hasher};

        // Use a BTreeMap for deterministic ordering
        let mut map = BTreeMap::new();
        map.insert("memory_limit_mb", self.memory_limit_mb.to_string());
        map.insert("cpu_limit_percent", self.cpu_limit_percent.to_string());
        if let Some(ref cmd) = self.setup_command {
            map.insert("setup_command", cmd.clone());
        }

        let mut hasher = std::hash::DefaultHasher::new();
        for (k, v) in &map {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        let hash = hasher.finish();
        format!("{hash:016x}")
    }

    // ── Internal resolution helpers ─────────────────────────────

    fn resolve_string(
        agent_val: Option<&str>,
        global_val: Option<&str>,
        env_var: &str,
        hardcoded: Option<&str>,
    ) -> Option<String> {
        if let Some(v) = agent_val {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        if let Some(v) = global_val {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        if let Ok(v) = std::env::var(env_var) {
            if !v.is_empty() {
                return Some(v);
            }
        }
        hardcoded.map(std::string::ToString::to_string)
    }

    fn resolve_u32(
        agent_val: Option<u32>,
        global_val: Option<u32>,
        env_var: &str,
        hardcoded: u32,
    ) -> u32 {
        if let Some(v) = agent_val {
            return v;
        }
        if let Some(v) = global_val {
            return v;
        }
        if let Ok(v) = std::env::var(env_var) {
            if let Ok(parsed) = v.parse::<u32>() {
                return parsed;
            }
        }
        hardcoded
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    // Helper: ensure env vars don't leak between tests
    fn with_clean_env<F: FnOnce()>(vars: &[&str], f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        // Save originals
        let originals: Vec<_> = vars.iter().map(|v| (*v, std::env::var(v).ok())).collect();
        // Clear
        for v in vars {
            std::env::remove_var(v);
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

        // Restore env even if the test closure panics.
        for (var, value) in originals {
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    static ENV_VARS: &[&str] = &[
        "QUILT_API_URL",
        "QUILT_API_KEY",
        "QUILT_MEMORY_LIMIT_MB",
        "QUILT_CPU_LIMIT_PERCENT",
        "QUILT_SETUP_COMMAND",
    ];

    // ── Hardcoded defaults ──────────────────────────────────────

    #[test]
    fn resolve_uses_hardcoded_defaults() {
        with_clean_env(ENV_VARS, || {
            let agent = QuiltConfigOverrides {
                api_key: Some("qlt_test_key".into()),
                ..Default::default()
            };
            let cfg = SandboxQuiltConfig::resolve(Some(&agent), None).unwrap();
            assert_eq!(cfg.api_url, "https://backend.quilt.sh");
            assert_eq!(cfg.memory_limit_mb, 4096);
            assert_eq!(cfg.cpu_limit_percent, 100);
            assert!(cfg.setup_command.is_none());
        });
    }

    // ── Missing API key ─────────────────────────────────────────

    #[test]
    fn resolve_fails_without_api_key() {
        with_clean_env(ENV_VARS, || {
            let result = SandboxQuiltConfig::resolve(None, None);
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("API key"));
        });
    }

    // ── Env var fallback ────────────────────────────────────────

    #[test]
    fn resolve_reads_env_vars() {
        with_clean_env(ENV_VARS, || {
            std::env::set_var("QUILT_API_URL", "https://custom.quilt.dev");
            std::env::set_var("QUILT_API_KEY", "qlt_env_key_123");
            std::env::set_var("QUILT_MEMORY_LIMIT_MB", "2048");
            std::env::set_var("QUILT_CPU_LIMIT_PERCENT", "50");
            std::env::set_var("QUILT_SETUP_COMMAND", "apt-get update");

            let cfg = SandboxQuiltConfig::resolve(None, None).unwrap();
            assert_eq!(cfg.api_url, "https://custom.quilt.dev");
            assert_eq!(cfg.api_key, "qlt_env_key_123");
            assert_eq!(cfg.memory_limit_mb, 2048);
            assert_eq!(cfg.cpu_limit_percent, 50);
            assert_eq!(cfg.setup_command.as_deref(), Some("apt-get update"));
        });
    }

    // ── Global defaults override env ────────────────────────────

    #[test]
    fn resolve_global_overrides_env() {
        with_clean_env(ENV_VARS, || {
            std::env::set_var("QUILT_API_KEY", "qlt_env_key");
            std::env::set_var("QUILT_MEMORY_LIMIT_MB", "1024");

            let global = QuiltGlobalDefaults {
                api_key: Some("qlt_global_key".into()),
                memory_limit_mb: Some(8192),
                ..Default::default()
            };

            let cfg = SandboxQuiltConfig::resolve(None, Some(&global)).unwrap();
            assert_eq!(cfg.api_key, "qlt_global_key");
            assert_eq!(cfg.memory_limit_mb, 8192);
        });
    }

    // ── Agent overrides override global ─────────────────────────

    #[test]
    fn resolve_agent_overrides_global() {
        with_clean_env(ENV_VARS, || {
            let global = QuiltGlobalDefaults {
                api_key: Some("qlt_global_key".into()),
                memory_limit_mb: Some(8192),
                cpu_limit_percent: Some(200),
                ..Default::default()
            };
            let agent = QuiltConfigOverrides {
                api_key: Some("qlt_agent_key".into()),
                memory_limit_mb: Some(512),
                ..Default::default()
            };

            let cfg = SandboxQuiltConfig::resolve(Some(&agent), Some(&global)).unwrap();
            assert_eq!(cfg.api_key, "qlt_agent_key");
            assert_eq!(cfg.memory_limit_mb, 512);
            // Falls through to global for cpu
            assert_eq!(cfg.cpu_limit_percent, 200);
        });
    }

    // ── Full override chain ─────────────────────────────────────

    #[test]
    fn resolve_full_chain() {
        with_clean_env(ENV_VARS, || {
            std::env::set_var("QUILT_API_URL", "https://env.quilt.sh");
            std::env::set_var("QUILT_API_KEY", "qlt_env");
            std::env::set_var("QUILT_MEMORY_LIMIT_MB", "1024");
            std::env::set_var("QUILT_CPU_LIMIT_PERCENT", "25");
            std::env::set_var("QUILT_SETUP_COMMAND", "env-setup");

            let global = QuiltGlobalDefaults {
                api_url: Some("https://global.quilt.sh".into()),
                api_key: Some("qlt_global".into()),
                memory_limit_mb: Some(2048),
                cpu_limit_percent: None, // falls to env
                setup_command: Some("global-setup".into()),
            };

            let agent = QuiltConfigOverrides {
                api_url: None, // falls to global
                api_key: Some("qlt_agent".into()),
                memory_limit_mb: None, // falls to global
                cpu_limit_percent: Some(75),
                setup_command: None, // falls to global
            };

            let cfg = SandboxQuiltConfig::resolve(Some(&agent), Some(&global)).unwrap();
            assert_eq!(cfg.api_url, "https://global.quilt.sh"); // global
            assert_eq!(cfg.api_key, "qlt_agent"); // agent
            assert_eq!(cfg.memory_limit_mb, 2048); // global
            assert_eq!(cfg.cpu_limit_percent, 75); // agent
            assert_eq!(cfg.setup_command.as_deref(), Some("global-setup")); // global
        });
    }

    // ── Empty strings skipped ───────────────────────────────────

    #[test]
    fn resolve_skips_empty_strings() {
        with_clean_env(ENV_VARS, || {
            std::env::set_var("QUILT_API_KEY", "qlt_env_key");

            let agent = QuiltConfigOverrides {
                api_url: Some(String::new()), // empty, should skip
                api_key: Some(String::new()), // empty, should skip
                ..Default::default()
            };

            let cfg = SandboxQuiltConfig::resolve(Some(&agent), None).unwrap();
            assert_eq!(cfg.api_url, "https://backend.quilt.sh"); // hardcoded default
            assert_eq!(cfg.api_key, "qlt_env_key"); // env var
        });
    }

    // ── Invalid env var for u32 ─────────────────────────────────

    #[test]
    fn resolve_ignores_invalid_u32_env_vars() {
        with_clean_env(ENV_VARS, || {
            std::env::set_var("QUILT_API_KEY", "qlt_key");
            std::env::set_var("QUILT_MEMORY_LIMIT_MB", "not_a_number");
            std::env::set_var("QUILT_CPU_LIMIT_PERCENT", "abc");

            let cfg = SandboxQuiltConfig::resolve(None, None).unwrap();
            assert_eq!(cfg.memory_limit_mb, 4096); // hardcoded
            assert_eq!(cfg.cpu_limit_percent, 100); // hardcoded
        });
    }

    // ── Config hash ─────────────────────────────────────────────

    #[test]
    fn config_hash_is_deterministic() {
        let cfg = SandboxQuiltConfig {
            api_url: "https://backend.quilt.sh".into(),
            api_key: "qlt_key".into(),
            memory_limit_mb: 4096,
            cpu_limit_percent: 100,
            setup_command: Some("echo hello".into()),
        };
        let h1 = cfg.config_hash();
        let h2 = cfg.config_hash();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16, "hash should be 16 hex chars");
    }

    #[test]
    fn config_hash_changes_with_memory() {
        let cfg1 = SandboxQuiltConfig {
            api_url: "https://backend.quilt.sh".into(),
            api_key: "qlt_key".into(),
            memory_limit_mb: 4096,
            cpu_limit_percent: 100,
            setup_command: None,
        };
        let cfg2 = SandboxQuiltConfig {
            memory_limit_mb: 2048,
            ..cfg1.clone()
        };
        assert_ne!(cfg1.config_hash(), cfg2.config_hash());
    }

    #[test]
    fn config_hash_changes_with_cpu() {
        let cfg1 = SandboxQuiltConfig {
            api_url: "https://backend.quilt.sh".into(),
            api_key: "qlt_key".into(),
            memory_limit_mb: 4096,
            cpu_limit_percent: 100,
            setup_command: None,
        };
        let cfg2 = SandboxQuiltConfig {
            cpu_limit_percent: 50,
            ..cfg1.clone()
        };
        assert_ne!(cfg1.config_hash(), cfg2.config_hash());
    }

    #[test]
    fn config_hash_changes_with_setup_command() {
        let cfg1 = SandboxQuiltConfig {
            api_url: "https://backend.quilt.sh".into(),
            api_key: "qlt_key".into(),
            memory_limit_mb: 4096,
            cpu_limit_percent: 100,
            setup_command: None,
        };
        let cfg2 = SandboxQuiltConfig {
            setup_command: Some("npm install".into()),
            ..cfg1.clone()
        };
        assert_ne!(cfg1.config_hash(), cfg2.config_hash());
    }

    #[test]
    fn config_hash_ignores_api_url_and_key() {
        let cfg1 = SandboxQuiltConfig {
            api_url: "https://a.com".into(),
            api_key: "qlt_key1".into(),
            memory_limit_mb: 4096,
            cpu_limit_percent: 100,
            setup_command: None,
        };
        let cfg2 = SandboxQuiltConfig {
            api_url: "https://b.com".into(),
            api_key: "qlt_key2".into(),
            ..cfg1.clone()
        };
        assert_eq!(
            cfg1.config_hash(),
            cfg2.config_hash(),
            "API URL and key should not affect the config hash"
        );
    }

    // ── Serde roundtrip ─────────────────────────────────────────

    #[test]
    fn config_json_roundtrip() {
        let cfg = SandboxQuiltConfig {
            api_url: "https://test.quilt.sh".into(),
            api_key: "qlt_roundtrip".into(),
            memory_limit_mb: 2048,
            cpu_limit_percent: 50,
            setup_command: Some("cargo build".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: SandboxQuiltConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.api_url, cfg.api_url);
        assert_eq!(parsed.api_key, cfg.api_key);
        assert_eq!(parsed.memory_limit_mb, cfg.memory_limit_mb);
        assert_eq!(parsed.cpu_limit_percent, cfg.cpu_limit_percent);
        assert_eq!(parsed.setup_command, cfg.setup_command);
    }

    #[test]
    fn overrides_default_is_all_none() {
        let o = QuiltConfigOverrides::default();
        assert!(o.api_url.is_none());
        assert!(o.api_key.is_none());
        assert!(o.memory_limit_mb.is_none());
        assert!(o.cpu_limit_percent.is_none());
        assert!(o.setup_command.is_none());
    }

    #[test]
    fn global_defaults_default_is_all_none() {
        let g = QuiltGlobalDefaults::default();
        assert!(g.api_url.is_none());
        assert!(g.api_key.is_none());
        assert!(g.memory_limit_mb.is_none());
        assert!(g.cpu_limit_percent.is_none());
        assert!(g.setup_command.is_none());
    }
}
