//! ESP32 runtime adapter stub.
//!
//! Provides a minimal `RuntimeAdapter` for ESP32-S3 targets.
//! Shell access, filesystem, and long-running processes are disabled.

use std::path::{Path, PathBuf};

use super::traits::RuntimeAdapter;

pub struct Esp32Runtime {
    storage_path: PathBuf,
}

impl Esp32Runtime {
    pub fn new(storage_path: PathBuf) -> Self {
        Self { storage_path }
    }
}

impl RuntimeAdapter for Esp32Runtime {
    fn name(&self) -> &str {
        "esp32"
    }

    fn has_shell_access(&self) -> bool {
        false
    }

    fn has_filesystem_access(&self) -> bool {
        // Limited filesystem via SPIFFS/LittleFS
        true
    }

    fn storage_path(&self) -> PathBuf {
        self.storage_path.clone()
    }

    fn supports_long_running(&self) -> bool {
        true // ESP32 runs continuously
    }

    fn memory_budget(&self) -> u64 {
        // ESP32-S3: 512KB SRAM, budget for agent runtime after OS overhead
        384 * 1024
    }

    fn build_shell_command(
        &self,
        _command: &str,
        _workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        anyhow::bail!("Shell access is not available on ESP32")
    }
}
