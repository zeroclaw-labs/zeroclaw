//! Read-only catalogs the dashboard shows: `integrations/list`,
//! `tools/cli-discover`, `plugins/list` and `a2a/identity`.
//!
//! Each body here is also what the matching HTTP route serializes, so the
//! route and the RPC method cannot drift.

use serde::Serialize;
use serde_json::Value;
use zeroclaw_api::jsonrpc::{JsonRpcError, error_codes};
use zeroclaw_config::schema::Config;

use crate::integrations::IntegrationEntry;

/// One integration as it goes over the wire.
#[must_use]
pub fn integration_entry_json(entry: &IntegrationEntry) -> Value {
    serde_json::json!({
        "name": &entry.name,
        "description": &entry.description,
        "category": entry.category,
        "category_label": entry.category.label(),
        "status": entry.status,
        // Canonical config map key (provider family key / ChannelsConfig map
        // key) for deep links; null when the entry has no config section.
        "key": &entry.key,
    })
}

/// The `integrations/list` body: every known integration and its status
/// under `config`.
#[must_use]
pub fn integrations_body(config: &Config) -> Value {
    let integrations: Vec<Value> = crate::integrations::registry::all_integrations(config)
        .iter()
        .map(integration_entry_json)
        .collect();
    serde_json::json!({ "integrations": integrations })
}

/// One tool spec as the dashboard lists it: name, description and parameter
/// schema, plus its output schema and parameter domains when it has them.
#[must_use]
pub fn tool_spec_json(spec: &zeroclaw_api::tool::ToolSpec) -> Value {
    let mut tool = serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.parameters,
    });
    if let Some(output) = &spec.output {
        tool["output"] = output.clone();
    }
    if !spec.param_domains.is_empty() {
        tool["param_domains"] = serde_json::json!(spec.param_domains);
    }
    tool
}

/// The `tools/list` body for a set of specs.
#[must_use]
pub fn tools_body(specs: &[zeroclaw_api::tool::ToolSpec]) -> Value {
    let tools: Vec<Value> = specs.iter().map(tool_spec_json).collect();
    serde_json::json!({ "tools": tools })
}

/// The `tools/cli-discover` body: CLI tools found on the daemon's `PATH`.
///
/// Discovery spawns child processes and blocks, so it runs on a blocking
/// worker. If that worker panics the list is empty and the reason is logged,
/// rather than failing the request.
pub async fn cli_tools_body() -> Value {
    let tools = match tokio::task::spawn_blocking(|| {
        zeroclaw_tools::cli_discovery::discover_cli_tools(&[], &[])
    })
    .await
    {
        Ok(tools) => tools,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "cli-tools discovery task failed; returning empty list"
            );
            Vec::new()
        }
    };
    serde_json::json!({ "cli_tools": tools })
}

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

/// Why the plugin catalog could not be built for this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCatalogUnavailable {
    /// Another catalog scan is running; the caller may retry.
    Busy,
    /// The discovery worker failed; the reason is logged.
    Failed,
}

#[cfg(feature = "plugins-wasm")]
static CATALOG_DISCOVERY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// The `plugins/list` body: the package catalog from canonical config, the
/// host-admitted installed manifests and the cached registry index, without
/// mutating any of them.
///
/// Host admission reads and verifies each component synchronously, so the
/// scan runs on a blocking worker, and at most one runs at a time across both
/// surfaces, including after a requesting client disconnects.
pub async fn plugins_body(config: &Config) -> Result<PluginsResponse, PluginCatalogUnavailable> {
    #[cfg(not(feature = "plugins-wasm"))]
    {
        Ok(build_response(
            config.plugins.enabled,
            config.plugins.plugins_dir.clone(),
        ))
    }

    #[cfg(feature = "plugins-wasm")]
    {
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        let plugin_path = config.plugins.resolved_plugins_dir();
        let signature_mode = config.plugins.security.signature_mode.clone();
        let trusted_publisher_keys = config.plugins.security.trusted_publisher_keys.clone();
        let data_dir = config.data_dir.clone();
        let permit = CATALOG_DISCOVERY
            .try_acquire()
            .map_err(|_| PluginCatalogUnavailable::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            build_response(
                plugins_enabled,
                plugins_dir,
                plugin_path,
                signature_mode,
                trusted_publisher_keys,
                data_dir,
            )
        })
        .await
        .map_err(|error| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": error.to_string(),
                        "error_key": "plugin_catalog_discovery_task_failed",
                    })),
                "plugin catalog discovery task failed"
            );
            PluginCatalogUnavailable::Failed
        })
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

/// The `a2a/identity` body: the per-alias agent card for `agent`, or the
/// discovery catalog card without one.
///
/// The card is built from config. A gateway started with a host or port
/// override advertises that override on its own well-known routes; the core
/// does not learn the gateway's listener address, so it uses the configured
/// one.
pub fn a2a_identity(config: &Config, agent: Option<&str>) -> Result<Value, JsonRpcError> {
    if !config.a2a.server.enabled {
        return Err(JsonRpcError {
            code: error_codes::INVALID_REQUEST,
            message: "the A2A server is not enabled ([a2a.server] enabled = false)".into(),
            data: None,
        });
    }
    let card = match agent {
        Some(alias) => {
            crate::a2a_card::build_agent_card(config, alias).ok_or_else(|| JsonRpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("agent {alias:?} is not published over A2A"),
                data: None,
            })?
        }
        None => crate::a2a_card::build_catalog_card(config),
    };
    serde_json::to_value(card).map_err(|e| JsonRpcError {
        code: error_codes::INTERNAL_ERROR,
        message: format!("failed to serialize the agent card: {e}"),
        data: None,
    })
}

#[cfg(test)]
mod plugin_catalog_tests {
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
