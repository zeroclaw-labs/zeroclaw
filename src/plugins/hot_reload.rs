#[derive(Debug, Clone)]
pub struct HotReloadConfig {
    pub enabled: bool,
}

impl Default for HotReloadConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Debug, Default)]
pub struct HotReloadManager {
    config: HotReloadConfig,
}

impl HotReloadManager {
    pub fn new(config: HotReloadConfig) -> Self {
        Self { config }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_reload_disabled_by_default() {
        let manager = HotReloadManager::new(HotReloadConfig::default());
        assert!(!manager.enabled());
    }
}
