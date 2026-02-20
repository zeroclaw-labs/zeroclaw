// Integration tests for JS plugin lifecycle

use crate::js::{
    manifest::PluginManifest,
    runtime::PluginId,
    sandbox::{PluginSandbox, SandboxConfig},
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Create a minimal valid plugin manifest
fn create_minimal_manifest(dir: &PathBuf) -> PathBuf {
    let manifest_path = dir.join("plugin.toml");
    let manifest_content = r#"
[plugin]
name = "test-plugin"
version = "1.0.0"
description = "A test plugin"
author = "Test Author"
license = "MIT"

[runtime]
entry = "index.js"

[permissions]
network = []
file_read = []
file_write = []
env_vars = []
"#;
    fs::write(&manifest_path, manifest_content).unwrap();
    manifest_path
}

/// Create a simple JavaScript plugin entry point
fn create_simple_plugin(dir: &PathBuf) -> PathBuf {
    let plugin_path = dir.join("index.js");
    let plugin_content = r#"
// Simple test plugin
function greet(name) {
    return "Hello, " + name + "!";
}

// Export for ZeroClaw
if (typeof __zc_tool_greet !== 'undefined') {
    __zc_tool_greet = greet;
}
"#;
    fs::write(&plugin_path, plugin_content).unwrap();
    plugin_path
}

/// Test fixture: A complete plugin directory structure
pub struct PluginFixture {
    /// Temporary directory
    pub temp_dir: TempDir,

    /// Plugin directory path
    pub plugin_dir: PathBuf,

    /// Manifest path
    pub manifest_path: PathBuf,

    /// Entry point path
    pub entry_path: PathBuf,
}

impl PluginFixture {
    /// Create a new plugin fixture with minimal files
    pub fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        let plugin_dir = temp_dir.path().to_path_buf();

        let manifest_path = create_minimal_manifest(&plugin_dir);
        let entry_path = create_simple_plugin(&plugin_dir);

        Self {
            temp_dir,
            plugin_dir,
            manifest_path,
            entry_path,
        }
    }

    /// Get the plugin ID
    pub fn plugin_id(&self) -> PluginId {
        PluginId("test-plugin".to_string())
    }

    /// Load and parse the manifest
    pub fn load_manifest(&self) -> PluginManifest {
        PluginManifest::from_file(&self.manifest_path).unwrap()
    }
}

impl Default for PluginFixture {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn test_fixture_creates_valid_files() {
        let fixture = PluginFixture::new();

        // Verify files exist
        assert!(fixture.manifest_path.exists());
        assert!(fixture.entry_path.exists());

        // Verify manifest is valid
        let manifest = fixture.load_manifest();
        assert_eq!(manifest.plugin.name, "test-plugin");
        assert_eq!(manifest.plugin.version, "1.0.0");
    }

    #[test]
    fn test_plugin_manifest_validation() {
        let fixture = PluginFixture::new();
        let manifest = fixture.load_manifest();

        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_plugin_permissions_default() {
        let fixture = PluginFixture::new();
        let manifest = fixture.load_manifest();

        assert_eq!(manifest.permissions.network.len(), 0);
        assert_eq!(manifest.permissions.file_read.len(), 0);
        assert!(manifest.permissions.file_write.is_empty());
        assert_eq!(manifest.permissions.env_vars.len(), 0);
    }

    #[tokio::test]
    #[cfg(feature = "js-runtime")]
    async fn test_sandbox_can_load_plugin() {
        let fixture = PluginFixture::new();
        let config = SandboxConfig::default();

        let sandbox = PluginSandbox::new(config).unwrap();

        // Read the plugin source
        let source = fs::read_to_string(&fixture.entry_path).unwrap();

        // Load the plugin
        let handle = sandbox
            .load_plugin(fixture.plugin_id().0.as_str(), &source, Some("index.js"))
            .await;

        assert!(handle.is_ok());
    }

    #[tokio::test]
    #[cfg(feature = "js-runtime")]
    async fn test_sandbox_execute_simple_code() {
        let fixture = PluginFixture::new();
        let config = SandboxConfig::default();

        let sandbox = PluginSandbox::new(config).unwrap();
        let source = fs::read_to_string(&fixture.entry_path).unwrap();

        let handle = sandbox
            .load_plugin(fixture.plugin_id().0.as_str(), &source, Some("index.js"))
            .await
            .unwrap();

        // Execute simple JavaScript
        let result = handle.execute("1 + 1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[cfg(feature = "js-runtime")]
    async fn test_plugin_with_custom_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_dir = temp_dir.path().to_path_buf();

        // Create manifest with custom permissions
        let manifest_path = plugin_dir.join("plugin.toml");
        let manifest_content = r#"
[plugin]
name = "custom-perm-plugin"
version = "1.0.0"
description = "Plugin with custom permissions"
author = "Test"
license = "MIT"

[runtime]
entry = "index.js"

[permissions]
network = ["api.example.com"]
file_read = ["./data/**"]
file_write = ["./output/**"]
env_vars = ["API_KEY"]
"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        // Create entry point
        let entry_path = plugin_dir.join("index.js");
        fs::write(&entry_path, "// test plugin").unwrap();

        let manifest = PluginManifest::from_file(&manifest_path).unwrap();

        assert_eq!(manifest.permissions.network.len(), 1);
        assert_eq!(manifest.permissions.network[0], "api.example.com");
        assert_eq!(manifest.permissions.file_read.len(), 1);
        assert_eq!(manifest.permissions.file_write[0], "./output/**");
        assert_eq!(manifest.permissions.env_vars.len(), 1);
    }

    #[test]
    fn test_plugin_manifest_with_tools() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_dir = temp_dir.path().to_path_buf();

        // Create manifest with tools
        let manifest_path = plugin_dir.join("plugin.toml");
        let manifest_content = r#"
[plugin]
name = "tool-plugin"
version = "1.0.0"
description = "Plugin with tools"
author = "Test"
license = "MIT"

[runtime]
entry = "index.js"

[permissions]
network = []
file_read = []
file_write = []
env_vars = []

[[tools.definitions]]
name = "search"
description = "Search the web"

[tools.definitions.parameters]
type = "object"
properties.query = { type = "string" }
required = ["query"]
"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        let manifest = PluginManifest::from_file(&manifest_path).unwrap();

        assert_eq!(manifest.tools.definitions.len(), 1);
        assert_eq!(manifest.tools.definitions[0].name, "search");
        assert_eq!(manifest.tools.definitions[0].description, "Search the web");
    }

    #[test]
    fn test_plugin_manifest_with_skills() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_dir = temp_dir.path().to_path_buf();

        // Create manifest with skills
        let manifest_path = plugin_dir.join("plugin.toml");
        let manifest_content = r#"
[plugin]
name = "skill-plugin"
version = "1.0.0"
description = "Plugin with skills"
author = "Test"
license = "MIT"

[runtime]
entry = "index.js"

[permissions]
network = []
file_read = []
file_write = []
env_vars = []

[[skills.definitions]]
name = "greeting"
description = "Greet the user"
patterns = ["hello", "hi *"]
examples = ["hello", "hi there"]
"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        let manifest = PluginManifest::from_file(&manifest_path).unwrap();

        assert_eq!(manifest.skills.definitions.len(), 1);
        assert_eq!(manifest.skills.definitions[0].name, "greeting");
        assert_eq!(
            manifest.skills.definitions[0]
                .patterns
                .as_ref()
                .unwrap()
                .len(),
            2
        );
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;

    #[test]
    fn test_tool_result_serialization() {
        let result = json!({
            "success": true,
            "output": "test output",
            "error": null
        });

        assert_eq!(result["success"], true);
        assert_eq!(result["output"], "test output");
    }

    #[test]
    fn test_skill_result_serialization() {
        let result = json!({
            "success": true,
            "response": "test response",
            "actions": [],
            "error": null
        });

        assert_eq!(result["success"], true);
        assert_eq!(result["response"], "test response");
        assert!(result["actions"].as_array().unwrap().is_empty());
    }
}
