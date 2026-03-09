use anyhow::Result;
use serde::{Deserialize, Serialize};

#[allow(clippy::unsafe_derive_deserialize)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_mb: u64,
    pub max_cpu_seconds: u64,
    pub max_file_descriptors: u64,
    pub max_processes: u64,
    pub max_file_size_mb: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_mb: 256,
            max_cpu_seconds: 30,
            max_file_descriptors: 256,
            max_processes: 16,
            max_file_size_mb: 10,
        }
    }
}

impl ResourceLimits {
    #[cfg(unix)]
    pub fn enforce(&self) -> Result<()> {
        use std::io;

        fn set_rlimit(resource: libc::c_int, value: u64) -> Result<()> {
            let lim = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            let ret = unsafe { libc::setrlimit(resource, &raw const lim) };
            if ret != 0 {
                return Err(io::Error::last_os_error().into());
            }
            Ok(())
        }

        set_rlimit(libc::RLIMIT_AS, self.max_memory_mb * 1024 * 1024)?;
        set_rlimit(libc::RLIMIT_CPU, self.max_cpu_seconds)?;
        set_rlimit(libc::RLIMIT_NOFILE, self.max_file_descriptors)?;
        set_rlimit(libc::RLIMIT_NPROC, self.max_processes)?;
        set_rlimit(libc::RLIMIT_FSIZE, self.max_file_size_mb * 1024 * 1024)?;
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn enforce(&self) -> Result<()> {
        tracing::warn!("resource limits not supported on this platform");
        Ok(())
    }
}

impl From<&crate::config::ResourceLimitsConfig> for ResourceLimits {
    fn from(cfg: &crate::config::ResourceLimitsConfig) -> Self {
        Self {
            max_memory_mb: u64::from(cfg.max_memory_mb),
            max_cpu_seconds: cfg.max_cpu_time_seconds,
            max_file_descriptors: 256,
            max_processes: u64::from(cfg.max_subprocesses),
            max_file_size_mb: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_memory_mb, 256);
        assert_eq!(limits.max_cpu_seconds, 30);
        assert_eq!(limits.max_file_descriptors, 256);
        assert_eq!(limits.max_processes, 16);
        assert_eq!(limits.max_file_size_mb, 10);
    }

    #[test]
    fn from_config() {
        let cfg = crate::config::ResourceLimitsConfig {
            max_memory_mb: 512,
            max_cpu_time_seconds: 60,
            max_subprocesses: 10,
            memory_monitoring: true,
        };
        let limits = ResourceLimits::from(&cfg);
        assert_eq!(limits.max_memory_mb, 512);
        assert_eq!(limits.max_cpu_seconds, 60);
        assert_eq!(limits.max_processes, 10);
    }

    #[test]
    fn serde_roundtrip() {
        let limits = ResourceLimits::default();
        let json = serde_json::to_string(&limits).unwrap();
        let parsed: ResourceLimits = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.max_memory_mb, limits.max_memory_mb);
        assert_eq!(parsed.max_cpu_seconds, limits.max_cpu_seconds);
    }
}
