use anyhow::Result;

use super::manifest::PluginManifest;

#[derive(Debug, Default)]
pub struct PluginRuntime;

impl PluginRuntime {
    pub fn new() -> Self {
        Self
    }

    pub fn load_manifest(&self, manifest: PluginManifest) -> Result<PluginManifest> {
        if !manifest.is_valid() {
            anyhow::bail!("invalid plugin manifest")
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_rejects_invalid_manifest() {
        let runtime = PluginRuntime::new();
        assert!(runtime.load_manifest(PluginManifest::default()).is_err());
    }
}
