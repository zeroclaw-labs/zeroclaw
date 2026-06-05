use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::providers::ProvidersConfig;
use crate::schema::{
    ClassificationRule, EmbeddingRouteConfig, ModelProviderConfig, ModelRouteConfig,
    ProxyConfig, QueryClassificationConfig,
};
use crate::secrets::SecretStore;

static STORE: OnceLock<ProviderStore> = OnceLock::new();

/// Mutex used to serialise test code that mutates + reads the global
/// provider store.  Tests that call `set_fallback_name`, `upsert_provider`,
/// etc. on the shared store should hold this lock for the duration of
/// their write-then-read sequence to prevent data races with concurrent
/// tests.
static TEST_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub struct ProviderStore {
    db_path: PathBuf,
    secret_store: SecretStore,
    env_overrides: EnvOverrides,
}

struct EnvOverrides {
    api_key: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    timeout_secs: Option<u64>,
    temperature: Option<f64>,
    extra_headers: HashMap<String, String>,
}

impl EnvOverrides {
    fn capture() -> Self {
        let api_key = std::env::var("DAEMONCLAW_API_KEY")
            .or_else(|_| std::env::var("API_KEY"))
            .ok()
            .filter(|k| !k.is_empty());

        let provider = std::env::var("DAEMONCLAW_PROVIDER")
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| {
                std::env::var("DAEMONCLAW_MODEL_PROVIDER")
                    .or_else(|_| std::env::var("MODEL_PROVIDER"))
                    .ok()
                    .filter(|p| !p.is_empty())
            });

        let model = std::env::var("DAEMONCLAW_MODEL")
            .or_else(|_| std::env::var("MODEL"))
            .ok()
            .filter(|m| !m.is_empty());

        let timeout_secs = std::env::var("DAEMONCLAW_PROVIDER_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&t| t > 0);

        let temperature = std::env::var("DAEMONCLAW_TEMPERATURE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|t| (0.0..=2.0).contains(t));

        let extra_headers = std::env::var("DAEMONCLAW_EXTRA_HEADERS")
            .ok()
            .map(|raw| parse_extra_headers(&raw))
            .unwrap_or_default();

        Self {
            api_key,
            provider,
            model,
            timeout_secs,
            temperature,
            extra_headers,
        }
    }

    #[allow(dead_code)]
    fn has_any(&self) -> bool {
        self.api_key.is_some()
            || self.provider.is_some()
            || self.model.is_some()
            || self.timeout_secs.is_some()
            || self.temperature.is_some()
            || !self.extra_headers.is_empty()
    }
}

fn parse_extra_headers(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in raw.split(',') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    map
}

impl ProviderStore {
    fn new(workspace_dir: &Path, daemonclaw_dir: &Path, encrypt_enabled: bool) -> Result<Self> {
        let state_dir = workspace_dir.join("state");
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("Failed to create state dir: {}", state_dir.display()))?;
        let db_path = state_dir.join("state.db");

        let secret_store = SecretStore::new(daemonclaw_dir, encrypt_enabled);
        let env_overrides = EnvOverrides::capture();

        let store = Self {
            db_path,
            secret_store,
            env_overrides,
        };
        store.ensure_tables()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("Failed to open state.db: {}", self.db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        Ok(conn)
    }

    fn migrate_table_names(&self) -> Result<()> {
        let conn = self.connect()?;
        let existing: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'oni_%'"
            )?;
            stmt.query_map([], |r| r.get(0))?
                .filter_map(|r| r.ok())
                .collect()
        };
        if existing.is_empty() {
            return Ok(());
        }
        let renames = [
            ("oni_providers", "providers"),
            ("oni_default_provider", "default_provider"),
            ("oni_model_routes", "model_routes"),
            ("oni_embedding_routes", "embedding_routes"),
            ("oni_proxy", "proxy_settings"),
            ("oni_classification_rules", "classification_rules"),
            ("oni_classification_enabled", "classification_enabled"),
        ];
        for (old, new) in &renames {
            if existing.iter().any(|t| t == *old) {
                conn.execute(&format!("ALTER TABLE \"{old}\" RENAME TO \"{new}\""), [])?;
            }
        }
        tracing::info!("Renamed {} legacy oni_* tables to generic names", existing.len());
        Ok(())
    }

    fn ensure_tables(&self) -> Result<()> {
        self.migrate_table_names()?;
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS providers (
                 name        TEXT PRIMARY KEY,
                 config_json TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS default_provider (
                 id   INTEGER PRIMARY KEY CHECK (id = 1),
                 name TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS model_routes (
                 hint     TEXT PRIMARY KEY,
                 provider TEXT NOT NULL,
                 model    TEXT NOT NULL,
                 api_key  TEXT
             );

             CREATE TABLE IF NOT EXISTS embedding_routes (
                 hint       TEXT PRIMARY KEY,
                 provider   TEXT NOT NULL,
                 model      TEXT NOT NULL,
                 dimensions INTEGER,
                 api_key    TEXT
             );

             CREATE TABLE IF NOT EXISTS proxy_settings (
                 id          INTEGER PRIMARY KEY CHECK (id = 1),
                 config_json TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS classification_rules (
                 hint       TEXT PRIMARY KEY,
                 keywords   TEXT NOT NULL DEFAULT '[]',
                 patterns   TEXT NOT NULL DEFAULT '[]',
                 min_length INTEGER,
                 max_length INTEGER,
                 priority   INTEGER NOT NULL DEFAULT 0
             );

             CREATE TABLE IF NOT EXISTS classification_enabled (
                 id      INTEGER PRIMARY KEY CHECK (id = 1),
                 enabled INTEGER NOT NULL DEFAULT 0
             );",
        )
        .context("Failed to create provider store tables")?;
        Ok(())
    }

    // ── Migration from config.toml ────────────────────────────────

    pub fn migrate_from_config(
        &self,
        providers: &ProvidersConfig,
        proxy: &ProxyConfig,
        classification: &QueryClassificationConfig,
    ) -> Result<bool> {
        let conn = self.connect()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM providers",
            [],
            |r| r.get(0),
        )?;
        let has_default: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM default_provider",
            [],
            |r| r.get(0),
        )?;

        if count > 0 || has_default {
            return Ok(false);
        }

        for (name, config) in &providers.models {
            let json = serde_json::to_string(config)
                .with_context(|| format!("Failed to serialize provider '{name}'"))?;
            conn.execute(
                "INSERT OR IGNORE INTO providers (name, config_json) VALUES (?1, ?2)",
                params![name, json],
            )?;
        }

        if let Some(ref fallback) = providers.fallback {
            conn.execute(
                "INSERT OR REPLACE INTO default_provider (id, name) VALUES (1, ?1)",
                params![fallback],
            )?;
        }

        for route in &providers.model_routes {
            conn.execute(
                "INSERT OR IGNORE INTO model_routes (hint, provider, model, api_key) VALUES (?1, ?2, ?3, ?4)",
                params![route.hint, route.provider, route.model, route.api_key],
            )?;
        }

        for route in &providers.embedding_routes {
            conn.execute(
                "INSERT OR IGNORE INTO embedding_routes (hint, provider, model, dimensions, api_key) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![route.hint, route.provider, route.model, route.dimensions.map(|d| d as i64), route.api_key],
            )?;
        }

        let proxy_json = serde_json::to_string(proxy)
            .context("Failed to serialize proxy config")?;
        conn.execute(
            "INSERT OR REPLACE INTO proxy_settings (id, config_json) VALUES (1, ?1)",
            params![proxy_json],
        )?;

        for rule in &classification.rules {
            let keywords_json = serde_json::to_string(&rule.keywords)?;
            let patterns_json = serde_json::to_string(&rule.patterns)?;
            conn.execute(
                "INSERT OR IGNORE INTO classification_rules (hint, keywords, patterns, min_length, max_length, priority) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![rule.hint, keywords_json, patterns_json, rule.min_length.map(|v| v as i64), rule.max_length.map(|v| v as i64), rule.priority],
            )?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO classification_enabled (id, enabled) VALUES (1, ?1)",
            params![classification.enabled as i32],
        )?;

        let migrated_count = providers.models.len();
        if migrated_count > 0 {
            tracing::info!(
                "Migrated {migrated_count} provider(s) from config.toml to state.db"
            );
        }

        Ok(true)
    }

    // ── Readers ───────────────────────────────────────────────────

    pub fn fallback_name(&self) -> Option<String> {
        if let Some(ref name) = self.env_overrides.provider {
            return Some(name.clone());
        }
        let conn = self.connect().ok()?;
        conn.query_row(
            "SELECT name FROM default_provider WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .ok()
    }

    pub fn fallback_provider(&self) -> Option<ModelProviderConfig> {
        let name = self.fallback_name()?;
        let mut config = self.get_provider(&name)?;
        self.apply_env_overrides_to(&name, &mut config);
        Some(config)
    }

    pub fn get_provider(&self, name: &str) -> Option<ModelProviderConfig> {
        let conn = self.connect().ok()?;
        let json: String = conn
            .query_row(
                "SELECT config_json FROM providers WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .ok()?;
        let mut config: ModelProviderConfig = serde_json::from_str(&json).ok()?;
        if let Some(ref encrypted) = config.api_key {
            if let Ok(decrypted) = self.secret_store.decrypt(encrypted) {
                config.api_key = Some(decrypted);
            }
        }
        Some(config)
    }

    pub fn get_provider_raw(&self, name: &str) -> Option<ModelProviderConfig> {
        let conn = self.connect().ok()?;
        let json: String = conn
            .query_row(
                "SELECT config_json FROM providers WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn all_providers(&self) -> HashMap<String, ModelProviderConfig> {
        let conn = match self.connect() {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let mut stmt = match conn.prepare("SELECT name, config_json FROM providers") {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok();
        let mut map = HashMap::new();
        if let Some(rows) = rows {
            for row in rows.flatten() {
                if let Ok(mut config) = serde_json::from_str::<ModelProviderConfig>(&row.1) {
                    if let Some(ref encrypted) = config.api_key {
                        if let Ok(decrypted) = self.secret_store.decrypt(encrypted) {
                            config.api_key = Some(decrypted);
                        }
                    }
                    map.insert(row.0, config);
                }
            }
        }
        map
    }

    pub fn model_routes(&self) -> Vec<ModelRouteConfig> {
        let conn = match self.connect() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn
            .prepare("SELECT hint, provider, model, api_key FROM model_routes ORDER BY hint")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| {
            Ok(ModelRouteConfig {
                hint: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                api_key: r.get(3)?,
            })
        })
        .ok()
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn embedding_routes(&self) -> Vec<EmbeddingRouteConfig> {
        let conn = match self.connect() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut stmt = match conn.prepare(
            "SELECT hint, provider, model, dimensions, api_key FROM embedding_routes ORDER BY hint",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| {
            Ok(EmbeddingRouteConfig {
                hint: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                dimensions: r.get::<_, Option<i64>>(3)?.map(|d| d as usize),
                api_key: r.get(4)?,
            })
        })
        .ok()
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn proxy(&self) -> ProxyConfig {
        let conn = match self.connect() {
            Ok(c) => c,
            Err(_) => return ProxyConfig::default(),
        };
        let json: Option<String> = conn
            .query_row(
                "SELECT config_json FROM proxy_settings WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .ok();
        json.and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    fn apply_env_overrides_to(&self, provider_name: &str, config: &mut ModelProviderConfig) {
        let is_fallback = self
            .fallback_name_from_db()
            .as_deref()
            .or(self.env_overrides.provider.as_deref())
            .is_some_and(|n| n == provider_name);

        if !is_fallback {
            return;
        }

        if let Some(ref key) = self.env_overrides.api_key {
            config.api_key = Some(key.clone());
        }

        if let Some(ref key) = provider_specific_api_key_env(provider_name) {
            config.api_key = Some(key.clone());
        }

        if let Some(ref model) = self.env_overrides.model {
            config.model = Some(model.clone());
        }
        if let Some(timeout) = self.env_overrides.timeout_secs {
            config.timeout_secs = Some(timeout);
        }
        if let Some(temp) = self.env_overrides.temperature {
            config.temperature = Some(temp);
        }
        if !self.env_overrides.extra_headers.is_empty() {
            for (k, v) in &self.env_overrides.extra_headers {
                config.extra_headers.insert(k.clone(), v.clone());
            }
        }
    }

    fn fallback_name_from_db(&self) -> Option<String> {
        let conn = self.connect().ok()?;
        conn.query_row(
            "SELECT name FROM default_provider WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .ok()
    }

    // ── Writers ───────────────────────────────────────────────────

    pub fn set_fallback_name(&self, name: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT OR REPLACE INTO default_provider (id, name) VALUES (1, ?1)",
            params![name],
        )?;
        Ok(())
    }

    pub fn upsert_provider(&self, name: &str, config: &ModelProviderConfig) -> Result<()> {
        let mut to_store = config.clone();
        if let Some(ref key) = to_store.api_key {
            if !key.is_empty() && !SecretStore::is_encrypted(key) {
                to_store.api_key = Some(self.secret_store.encrypt(key)?);
            }
        }
        let json = serde_json::to_string(&to_store)
            .with_context(|| format!("Failed to serialize provider '{name}'"))?;
        let conn = self.connect()?;
        conn.execute(
            "INSERT OR REPLACE INTO providers (name, config_json) VALUES (?1, ?2)",
            params![name, json],
        )?;
        Ok(())
    }

    pub fn set_model_routes(&self, routes: &[ModelRouteConfig]) -> Result<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM model_routes", [])?;
        for route in routes {
            conn.execute(
                "INSERT INTO model_routes (hint, provider, model, api_key) VALUES (?1, ?2, ?3, ?4)",
                params![route.hint, route.provider, route.model, route.api_key],
            )?;
        }
        Ok(())
    }

    pub fn remove_model_route(&self, hint: &str) -> Result<usize> {
        let conn = self.connect()?;
        let removed = conn.execute(
            "DELETE FROM model_routes WHERE hint = ?1",
            params![hint],
        )?;
        Ok(removed)
    }

    pub fn set_embedding_routes(&self, routes: &[EmbeddingRouteConfig]) -> Result<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM embedding_routes", [])?;
        for route in routes {
            conn.execute(
                "INSERT INTO embedding_routes (hint, provider, model, dimensions, api_key) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![route.hint, route.provider, route.model, route.dimensions.map(|d| d as i64), route.api_key],
            )?;
        }
        Ok(())
    }

    pub fn set_proxy(&self, proxy: &ProxyConfig) -> Result<()> {
        let json = serde_json::to_string(proxy).context("Failed to serialize proxy config")?;
        let conn = self.connect()?;
        conn.execute(
            "INSERT OR REPLACE INTO proxy_settings (id, config_json) VALUES (1, ?1)",
            params![json],
        )?;
        Ok(())
    }

    pub fn classification_config(&self) -> QueryClassificationConfig {
        let conn = match self.connect() {
            Ok(c) => c,
            Err(_) => return QueryClassificationConfig::default(),
        };

        let enabled: bool = conn
            .query_row(
                "SELECT enabled != 0 FROM classification_enabled WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(false);

        let mut stmt = match conn.prepare(
            "SELECT hint, keywords, patterns, min_length, max_length, priority FROM classification_rules ORDER BY priority DESC, hint",
        ) {
            Ok(s) => s,
            Err(_) => return QueryClassificationConfig { enabled, rules: Vec::new() },
        };

        let rules = stmt
            .query_map([], |r| {
                let keywords_json: String = r.get(1)?;
                let patterns_json: String = r.get(2)?;
                let keywords: Vec<String> =
                    serde_json::from_str(&keywords_json).unwrap_or_default();
                let patterns: Vec<String> =
                    serde_json::from_str(&patterns_json).unwrap_or_default();
                Ok(ClassificationRule {
                    hint: r.get(0)?,
                    keywords,
                    patterns,
                    min_length: r.get::<_, Option<i64>>(3)?.map(|v| v as usize),
                    max_length: r.get::<_, Option<i64>>(4)?.map(|v| v as usize),
                    priority: r.get(5)?,
                })
            })
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();

        QueryClassificationConfig { enabled, rules }
    }

    pub fn set_classification_config(&self, config: &QueryClassificationConfig) -> Result<()> {
        let conn = self.connect()?;
        conn.execute("DELETE FROM classification_rules", [])?;
        for rule in &config.rules {
            let keywords_json = serde_json::to_string(&rule.keywords)?;
            let patterns_json = serde_json::to_string(&rule.patterns)?;
            conn.execute(
                "INSERT INTO classification_rules (hint, keywords, patterns, min_length, max_length, priority) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![rule.hint, keywords_json, patterns_json, rule.min_length.map(|v| v as i64), rule.max_length.map(|v| v as i64), rule.priority],
            )?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO classification_enabled (id, enabled) VALUES (1, ?1)",
            params![config.enabled as i32],
        )?;
        Ok(())
    }

    pub fn secret_store(&self) -> &SecretStore {
        &self.secret_store
    }
}

fn provider_specific_api_key_env(provider_name: &str) -> Option<String> {
    let lower = provider_name.to_ascii_lowercase();
    if crate::provider_aliases::is_glm_alias(&lower) {
        std::env::var("GLM_API_KEY").ok().filter(|k| !k.is_empty())
    } else if crate::provider_aliases::is_zai_alias(&lower) {
        std::env::var("ZAI_API_KEY").ok().filter(|k| !k.is_empty())
    } else {
        None
    }
}

// ── Global access ─────────────────────────────────────────────────

pub fn init_provider_store(
    workspace_dir: &Path,
    daemonclaw_dir: &Path,
    encrypt_enabled: bool,
) -> Result<()> {
    let store = ProviderStore::new(workspace_dir, daemonclaw_dir, encrypt_enabled)?;
    // OnceLock::set returns Err if already initialized — that's fine,
    // the first writer wins.  Silently ignore the duplicate.
    let _ = STORE.set(store);
    Ok(())
}

pub fn provider_store() -> &'static ProviderStore {
    STORE
        .get()
        .expect("Provider store not initialized — call init_provider_store first")
}

pub fn try_provider_store() -> Option<&'static ProviderStore> {
    STORE.get()
}

/// Ensure the global provider store is initialized.
///
/// If the store has not been initialised yet, creates one backed by a
/// temporary directory (kept alive for the process lifetime).
/// If the store is already initialised, this is a no-op.
///
/// This is meant for test code in downstream crates that needs the store
/// present before exercising tool / doctor / wizard helpers.
pub fn ensure_provider_store_for_tests() {
    if STORE.get().is_some() {
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "daemonclaw-test-store-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create test provider store dir");
    init_provider_store(&path, &path, false)
        .expect("test provider store init");
}

/// Acquire a lock that serialises test access to the shared provider store.
///
/// Tests that mutate the global store (set_fallback_name, upsert_provider,
/// set_model_routes, etc.) and then assert on the result should hold this
/// guard for the entire write-then-read span.
pub fn test_store_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn get_fallback_name() -> Option<String> {
    STORE.get().and_then(|s| s.fallback_name())
}

pub fn get_fallback_provider() -> Option<ModelProviderConfig> {
    STORE.get().and_then(|s| s.fallback_provider())
}

pub fn get_providers() -> HashMap<String, ModelProviderConfig> {
    STORE
        .get()
        .map(|s| s.all_providers())
        .unwrap_or_default()
}

pub fn get_model_routes() -> Vec<ModelRouteConfig> {
    STORE
        .get()
        .map(|s| s.model_routes())
        .unwrap_or_default()
}

pub fn get_embedding_routes() -> Vec<EmbeddingRouteConfig> {
    STORE
        .get()
        .map(|s| s.embedding_routes())
        .unwrap_or_default()
}

pub fn get_proxy() -> ProxyConfig {
    STORE.get().map(|s| s.proxy()).unwrap_or_default()
}

pub fn get_classification_config() -> QueryClassificationConfig {
    STORE
        .get()
        .map(|s| s.classification_config())
        .unwrap_or_default()
}

// ── Test helper ───────────────────────────────────────────────────

#[cfg(test)]
pub fn init_provider_store_for_test(
    workspace_dir: &Path,
    providers: &ProvidersConfig,
    proxy: &ProxyConfig,
) -> Result<()> {
    let store = ProviderStore::new(workspace_dir, workspace_dir, false)?;
    store.migrate_from_config(providers, proxy, &QueryClassificationConfig::default())?;
    let _ = STORE.set(store);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_created_on_init() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();
        let conn = store.connect().unwrap();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('providers','default_provider','model_routes','embedding_routes','proxy_settings','classification_rules','classification_enabled') ORDER BY name",
                )
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert!(tables.contains(&"providers".to_string()));
        assert!(tables.contains(&"default_provider".to_string()));
        assert!(tables.contains(&"model_routes".to_string()));
        assert!(tables.contains(&"embedding_routes".to_string()));
        assert!(tables.contains(&"proxy_settings".to_string()));
        assert!(tables.contains(&"classification_rules".to_string()));
        assert!(tables.contains(&"classification_enabled".to_string()));
    }

    #[test]
    fn migration_seeds_providers() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();

        let mut providers = ProvidersConfig::default();
        providers.fallback = Some("zai".to_string());
        providers.models.insert(
            "zai".to_string(),
            ModelProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                model: Some("glm-5.1".to_string()),
                temperature: Some(0.7),
                ..Default::default()
            },
        );
        providers.model_routes.push(ModelRouteConfig {
            hint: "coding".to_string(),
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key: None,
        });

        let proxy = ProxyConfig::default();
        let migrated = store.migrate_from_config(&providers, &proxy, &QueryClassificationConfig::default()).unwrap();
        assert!(migrated);

        assert_eq!(store.fallback_name(), Some("zai".to_string()));
        let fb = store.fallback_provider().unwrap();
        assert_eq!(fb.api_key.as_deref(), Some("sk-test-key"));
        assert_eq!(fb.model.as_deref(), Some("glm-5.1"));

        let routes = store.model_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].hint, "coding");
    }

    #[test]
    fn migration_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();

        let mut providers = ProvidersConfig::default();
        providers.fallback = Some("zai".to_string());
        providers.models.insert(
            "zai".to_string(),
            ModelProviderConfig {
                model: Some("v1".to_string()),
                ..Default::default()
            },
        );

        assert!(store.migrate_from_config(&providers, &ProxyConfig::default(), &QueryClassificationConfig::default()).unwrap());

        providers.models.get_mut("zai").unwrap().model = Some("v2".to_string());
        assert!(!store.migrate_from_config(&providers, &ProxyConfig::default(), &QueryClassificationConfig::default()).unwrap());

        let fb = store.fallback_provider().unwrap();
        assert_eq!(fb.model.as_deref(), Some("v1"), "Second migration must not overwrite");
    }

    #[test]
    fn upsert_provider_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();

        let config = ModelProviderConfig {
            api_key: Some("sk-new".to_string()),
            model: Some("test-model".to_string()),
            temperature: Some(0.5),
            ..Default::default()
        };
        store.upsert_provider("test", &config).unwrap();
        store.set_fallback_name("test").unwrap();

        let read = store.get_provider("test").unwrap();
        assert_eq!(read.api_key.as_deref(), Some("sk-new"));
        assert_eq!(read.model.as_deref(), Some("test-model"));
    }

    #[test]
    fn encrypted_key_stored_and_decrypted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), true).unwrap();

        let config = ModelProviderConfig {
            api_key: Some("sk-secret-123".to_string()),
            ..Default::default()
        };
        store.upsert_provider("enc_test", &config).unwrap();

        let raw = store.get_provider_raw("enc_test").unwrap();
        assert!(
            SecretStore::is_encrypted(raw.api_key.as_deref().unwrap()),
            "API key must be stored encrypted"
        );

        let decrypted = store.get_provider("enc_test").unwrap();
        assert_eq!(decrypted.api_key.as_deref(), Some("sk-secret-123"));
    }

    #[test]
    fn model_routes_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();

        let routes = vec![
            ModelRouteConfig {
                hint: "coding".to_string(),
                provider: "openai".to_string(),
                model: "gpt-4".to_string(),
                api_key: None,
            },
            ModelRouteConfig {
                hint: "reasoning".to_string(),
                provider: "anthropic".to_string(),
                model: "claude-3".to_string(),
                api_key: Some("sk-route".to_string()),
            },
        ];
        store.set_model_routes(&routes).unwrap();

        let read = store.model_routes();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].hint, "coding");
        assert_eq!(read[1].hint, "reasoning");
        assert_eq!(read[1].api_key.as_deref(), Some("sk-route"));
    }

    #[test]
    fn proxy_roundtrip() {
        use crate::schema::ProxyScope;
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();

        let proxy = ProxyConfig {
            enabled: true,
            http_proxy: Some("http://proxy:8080".to_string()),
            scope: ProxyScope::Zeroclaw,
            ..Default::default()
        };
        store.set_proxy(&proxy).unwrap();

        let read = store.proxy();
        assert!(read.enabled);
        assert_eq!(read.http_proxy.as_deref(), Some("http://proxy:8080"));
    }

    #[test]
    fn set_default_change_visible_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(tmp.path(), tmp.path(), false).unwrap();

        store
            .upsert_provider(
                "a",
                &ModelProviderConfig {
                    model: Some("model-a".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .upsert_provider(
                "b",
                &ModelProviderConfig {
                    model: Some("model-b".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        store.set_fallback_name("a").unwrap();
        assert_eq!(
            store.fallback_provider().unwrap().model.as_deref(),
            Some("model-a")
        );

        store.set_fallback_name("b").unwrap();
        assert_eq!(
            store.fallback_provider().unwrap().model.as_deref(),
            Some("model-b")
        );
    }
}
