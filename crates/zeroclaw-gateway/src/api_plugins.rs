//! Authenticated, read-only plugin package catalog API.
//!
//! The catalog is materialized for each request from canonical config, the
//! host-admitted installed manifests, and the cached registry index. The
//! gateway does not retain a second catalog or plugin lifecycle state.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use serde::Serialize;

use super::AppState;

/// Host-admitted metadata for an installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InstalledPluginPackage {
    pub version: String,
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
}

/// Metadata selected from the cached registry by the canonical unpinned
/// install resolution rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AvailablePluginPackage {
    pub version: String,
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    /// Exact `name@version` identity for display as inert data. Registry URLs
    /// are deliberately excluded because custom URLs may contain credentials.
    pub install_source: String,
}

/// One package in the request-time catalog.
///
/// Installed and registry records remain separate because their versions and
/// metadata can legitimately differ. Package name is the only merged identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PluginCatalogEntry {
    pub name: String,
    pub installed: Option<InstalledPluginPackage>,
    pub available: Option<AvailablePluginPackage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PluginCatalogIssueSource {
    Installed,
    Registry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PluginCatalogIssueCode {
    DiscoveryFailed,
    CacheReadFailed,
}

/// Stable source failure returned without filesystem paths or diagnostics.
/// Detailed failures remain in gateway logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PluginCatalogIssue {
    pub source: PluginCatalogIssueSource,
    pub code: PluginCatalogIssueCode,
}

/// Response from `GET /api/plugins`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PluginsResponse {
    /// Canonical `[plugins].enabled` config value. This is configuration intent,
    /// not proof that any plugin instance is healthy.
    pub plugins_enabled: bool,
    /// Whether this binary was compiled with WASM plugin catalog support.
    pub wasm_plugins_available: bool,
    /// Canonical configured plugin directory, before path expansion.
    pub plugins_dir: String,
    /// One row per package name across installed and cached-registry sources.
    pub plugins: Vec<PluginCatalogEntry>,
    /// Source failures, kept distinct from a valid empty catalog.
    pub issues: Vec<PluginCatalogIssue>,
}

/// `GET /api/plugins` — return the package catalog without mutating config,
/// registry state, or the plugin directory.
pub async fn list_plugins(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|authorization| authorization.strip_prefix("Bearer "))
            .unwrap_or_default();
        if !state.pairing.is_authenticated(token) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    let config = state.config.read();
    let plugins_enabled = config.plugins.enabled;
    let plugins_dir = config.plugins.plugins_dir.clone();

    #[cfg(not(feature = "plugins-wasm"))]
    {
        drop(config);
        Json(build_response(plugins_enabled, plugins_dir)).into_response()
    }

    #[cfg(feature = "plugins-wasm")]
    {
        let plugin_path = config.plugins.resolved_plugins_dir();
        let signature_mode = config.plugins.security.signature_mode.clone();
        let trusted_publisher_keys = config.plugins.security.trusted_publisher_keys.clone();
        let data_dir = config.data_dir.clone();
        drop(config);

        Json(build_response(
            plugins_enabled,
            plugins_dir,
            plugin_path,
            signature_mode,
            trusted_publisher_keys,
            data_dir,
        ))
        .into_response()
    }
}

#[cfg(not(feature = "plugins-wasm"))]
fn build_response(plugins_enabled: bool, plugins_dir: String) -> PluginsResponse {
    PluginsResponse {
        plugins_enabled,
        wasm_plugins_available: false,
        plugins_dir,
        plugins: Vec::new(),
        issues: Vec::new(),
    }
}

#[cfg(feature = "plugins-wasm")]
fn build_response(
    plugins_enabled: bool,
    plugins_dir: String,
    plugin_path: std::path::PathBuf,
    signature_mode: String,
    trusted_publisher_keys: Vec<String>,
    data_dir: std::path::PathBuf,
) -> PluginsResponse {
    use zeroclaw_plugins::catalog::package_catalog;

    let mut issues = Vec::new();
    let installed = match plugin_path.try_exists() {
        Ok(true) => {
            let mode = zeroclaw_plugins::host::PluginHost::resolve_signature_mode(&signature_mode);
            match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
                &plugin_path,
                mode,
                trusted_publisher_keys,
            ) {
                Ok(host) => host.list_plugins(),
                Err(error) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail,)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error": error.to_string(),
                                "error_key": "plugin_catalog_discovery_failed",
                            })),
                        "plugin catalog discovery failed"
                    );
                    issues.push(PluginCatalogIssue {
                        source: PluginCatalogIssueSource::Installed,
                        code: PluginCatalogIssueCode::DiscoveryFailed,
                    });
                    Vec::new()
                }
            }
        }
        Ok(false) => Vec::new(),
        Err(error) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": error.to_string(),
                        "error_key": "plugin_catalog_directory_metadata_failed",
                    })),
                "plugin catalog directory metadata could not be read"
            );
            issues.push(PluginCatalogIssue {
                source: PluginCatalogIssueSource::Installed,
                code: PluginCatalogIssueCode::DiscoveryFailed,
            });
            Vec::new()
        }
    };

    let registry = match zeroclaw_plugins::registry::read_cached_registry_index(&data_dir) {
        Ok(index) => index,
        Err(error) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": error.to_string(),
                        "error_key": "plugin_catalog_registry_cache_read_failed",
                    })),
                "plugin catalog registry cache could not be read"
            );
            issues.push(PluginCatalogIssue {
                source: PluginCatalogIssueSource::Registry,
                code: PluginCatalogIssueCode::CacheReadFailed,
            });
            None
        }
    };

    let plugins = package_catalog(&installed, registry.as_ref())
        .into_iter()
        .map(PluginCatalogEntry::from)
        .collect();

    PluginsResponse {
        plugins_enabled,
        wasm_plugins_available: true,
        plugins_dir,
        plugins,
        issues,
    }
}

#[cfg(feature = "plugins-wasm")]
impl From<zeroclaw_plugins::catalog::PluginCatalogEntry<'_>> for PluginCatalogEntry {
    fn from(entry: zeroclaw_plugins::catalog::PluginCatalogEntry<'_>) -> Self {
        Self {
            name: entry.name().to_string(),
            installed: entry.installed().map(InstalledPluginPackage::from),
            available: entry.available().map(AvailablePluginPackage::from),
        }
    }
}

#[cfg(feature = "plugins-wasm")]
impl From<&zeroclaw_plugins::PluginInfo> for InstalledPluginPackage {
    fn from(plugin: &zeroclaw_plugins::PluginInfo) -> Self {
        Self {
            version: plugin.version.clone(),
            description: plugin.description.clone(),
            capabilities: serialized_wire_names(&plugin.capabilities),
            permissions: serialized_wire_names(&plugin.permissions),
        }
    }
}

#[cfg(feature = "plugins-wasm")]
impl From<&zeroclaw_plugins::registry::PluginRegistryEntry> for AvailablePluginPackage {
    fn from(plugin: &zeroclaw_plugins::registry::PluginRegistryEntry) -> Self {
        Self {
            version: plugin.version.clone(),
            description: plugin.description.clone(),
            capabilities: plugin.capabilities.clone(),
            install_source: zeroclaw_plugins::registry::install_source(plugin),
        }
    }
}

#[cfg(feature = "plugins-wasm")]
fn serialized_wire_names<T: Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| {
            serde_json::to_value(value)
                .ok()
                .and_then(|wire| wire.as_str().map(str::to_owned))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_reports_compile_time_wasm_availability() {
        #[cfg(not(feature = "plugins-wasm"))]
        let response = build_response(false, "plugins".to_string());

        #[cfg(feature = "plugins-wasm")]
        let response = {
            let temp = tempfile::tempdir().expect("temporary empty catalog");
            build_response(
                false,
                "plugins".to_string(),
                temp.path().join("missing-plugin-directory"),
                "disabled".to_string(),
                Vec::new(),
                temp.path().join("missing-data-directory"),
            )
        };

        assert_eq!(
            response.wasm_plugins_available,
            cfg!(feature = "plugins-wasm")
        );
        assert!(response.plugins.is_empty());
        assert!(response.issues.is_empty());
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn package_rows_preserve_installed_and_registry_versions_separately() {
        use zeroclaw_plugins::registry::{
            PluginRegistryEntry, PluginRegistryIndex, write_cached_registry_index,
        };

        let temp = tempfile::tempdir().expect("temporary plugin catalog");
        let plugin_dir = temp.path().join("plugins/calendar");
        std::fs::create_dir_all(&plugin_dir).expect("plugin directory");
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").expect("component fixture");
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            concat!(
                "name = \"calendar\"\n",
                "version = \"0.1.0\"\n",
                "description = \"installed description\"\n",
                "wasm_path = \"plugin.wasm\"\n",
                "capabilities = [\"tool\"]\n",
                "permissions = [\"file_read\"]\n",
            ),
        )
        .expect("manifest fixture");
        let index = PluginRegistryIndex {
            plugins: vec![PluginRegistryEntry {
                name: "calendar".to_string(),
                version: "0.2.0".to_string(),
                description: Some("registry description".to_string()),
                author: None,
                capabilities: vec!["tool".to_string(), "skill".to_string()],
                url: "https://example.invalid/calendar.zip".to_string(),
                sha256: None,
            }],
            registry_url: None,
        };
        write_cached_registry_index(temp.path(), "https://example.invalid/index.json", &index)
            .expect("registry cache");

        let response = build_response(
            false,
            temp.path().join("plugins").display().to_string(),
            temp.path().join("plugins"),
            "disabled".to_string(),
            Vec::new(),
            temp.path().to_path_buf(),
        );

        assert_eq!(response.plugins.len(), 1);
        let package = &response.plugins[0];
        assert_eq!(package.name, "calendar");
        assert_eq!(
            package
                .installed
                .as_ref()
                .map(|source| source.version.as_str()),
            Some("0.1.0")
        );
        assert_eq!(
            package
                .available
                .as_ref()
                .map(|source| source.version.as_str()),
            Some("0.2.0")
        );
        assert_eq!(
            package
                .available
                .as_ref()
                .map(|source| source.install_source.as_str()),
            Some("calendar@0.2.0")
        );
        assert!(!response.plugins_enabled);
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn registry_cache_failure_is_distinct_from_an_empty_catalog() {
        let temp = tempfile::tempdir().expect("temporary registry cache");
        let cache_dir = temp.path().join("plugin-registry");
        std::fs::create_dir_all(&cache_dir).expect("registry cache directory");
        std::fs::write(cache_dir.join("registry.json"), "not json").expect("invalid cache");

        let response = build_response(
            true,
            "plugins".to_string(),
            temp.path().join("missing-plugins"),
            "disabled".to_string(),
            Vec::new(),
            temp.path().to_path_buf(),
        );

        assert!(response.plugins.is_empty());
        assert_eq!(
            response.issues,
            vec![PluginCatalogIssue {
                source: PluginCatalogIssueSource::Registry,
                code: PluginCatalogIssueCode::CacheReadFailed,
            }]
        );
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn registry_credentials_are_not_projected_into_the_response() {
        use zeroclaw_plugins::registry::{
            PluginRegistryEntry, PluginRegistryIndex, write_cached_registry_index,
        };

        let temp = tempfile::tempdir().expect("temporary registry cache");
        let registry_url =
            "https://registry-user:registry-secret@example.invalid/index.json?token=private";
        let index = PluginRegistryIndex {
            plugins: vec![PluginRegistryEntry {
                name: "mail".to_string(),
                version: "1.2.3".to_string(),
                description: Some("Mail integration".to_string()),
                author: None,
                capabilities: vec!["channel".to_string()],
                url: "https://download-user:download-secret@example.invalid/mail.zip".to_string(),
                sha256: None,
            }],
            registry_url: None,
        };
        write_cached_registry_index(temp.path(), registry_url, &index).expect("registry cache");

        let response = build_response(
            true,
            "plugins".to_string(),
            temp.path().join("missing-plugins"),
            "disabled".to_string(),
            Vec::new(),
            temp.path().to_path_buf(),
        );
        let json = serde_json::to_string(&response).expect("catalog response JSON");

        assert!(json.contains("mail@1.2.3"));
        assert!(!json.contains("registry-secret"));
        assert!(!json.contains("download-secret"));
        assert!(!json.contains("token=private"));
        assert!(!json.contains("registry_url"));
        assert!(!json.contains("url"));
    }
}
