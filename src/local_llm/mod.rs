//! Local LLM lifecycle: Ollama daemon health, model installation, model pull
//! progress, and OS-matched runtime install for the on-device Gemma 4
//! fallback path.
//!
//! Distinct from `src/providers/ollama.rs` which handles inference (chat /
//! completion). This module covers the *setup* surface end-to-end:
//! - [`installer`] — detect host OS and install the Ollama runtime
//! - [`is_ollama_running`] — daemon health probe
//! - [`list_installed`] / [`is_installed`] — model inventory
//! - [`pull_model`] — streaming model pull with NDJSON progress
//! - [`LocalLlmConfig`] — persisted default-model choice
//!
//! The whole module assumes the caller's UI has obtained explicit user
//! consent for each automated step (hardware detect → recommend → install).

pub mod fallback_registry;
pub mod installer;
pub mod network_health;
pub mod setup;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::fs;

use crate::config::ReliabilityConfig;
use fallback_registry::{register_local_fallback, RegistrationOutcome};
use network_health::NetworkHealth;

/// Default Ollama HTTP endpoint on localhost.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

// Process-wide shared NetworkHealth so request handlers can read cached
// reachability without each constructing their own probe. Lazily spawns the
// background refresh loop the first time it is accessed from inside a tokio
// runtime.
static SHARED_HEALTH: OnceLock<Arc<NetworkHealth>> = OnceLock::new();

/// Returns the process-wide shared `NetworkHealth`. Cheap on the hot path —
/// each call is one atomic `get` plus an `Arc::clone`. The first call from
/// inside a tokio runtime spawns a background refresh loop at
/// [`network_health::DEFAULT_REFRESH_INTERVAL`]; before that initial spawn
/// the cached state is the construction-time default (online).
pub fn shared_health() -> Arc<NetworkHealth> {
    SHARED_HEALTH
        .get_or_init(|| {
            let h = NetworkHealth::new();
            if tokio::runtime::Handle::try_current().is_ok() {
                // Fire-and-forget refresh loop; binding the JoinHandle keeps
                // clippy happy (otherwise it warns about non-binding `let _`
                // on a future-returning call).
                let _refresh_handle: tokio::task::JoinHandle<()> = Arc::clone(&h)
                    .spawn_refresh_loop(network_health::DEFAULT_REFRESH_INTERVAL);
            }
            h
        })
        .clone()
}

/// Result of arming the local-LLM fallback path at runtime startup.
///
/// Returned by [`arm_local_fallback`]. Bundles the ongoing
/// [`NetworkHealth`] probe (which the caller should keep alive) with the
/// outcome of the one-shot fallback registration so it can be surfaced in
/// startup logs / UI badges.
pub struct ArmedFallback {
    /// Shared reachability cache. Hot-path callers query
    /// `health.is_online()`. Refresh task is spawned automatically when
    /// `arm_local_fallback` is called with `start_refresh: true`.
    pub health: Arc<NetworkHealth>,
    /// What the registration step did (or did not) do.
    pub registration: RegistrationOutcome,
    /// Snapshot of the local LLM model tag that was registered, if any.
    pub local_model: Option<String>,
}

/// Arm the local-LLM fallback path: probe network health, attempt to
/// register Ollama+Gemma 4 in `reliability`, and optionally spawn the
/// background reachability refresh loop.
///
/// Idempotent — safe to call multiple times; the registry helper short-
/// circuits if `ollama` is already in `fallback_providers`.
///
/// Mutates `reliability` in place when local fallback is enabled, daemon
/// is reachable, and the configured model tag is installed.
pub async fn arm_local_fallback(
    reliability: &mut ReliabilityConfig,
    base_url: &str,
    start_refresh: bool,
) -> ArmedFallback {
    let health = NetworkHealth::new();
    let _ = health.check_now().await;
    if start_refresh {
        let _join =
            Arc::clone(&health).spawn_refresh_loop(network_health::DEFAULT_REFRESH_INTERVAL);
    }

    let registration = register_local_fallback(reliability, base_url).await;
    let local_model = match &registration {
        RegistrationOutcome::Registered { local_model } => Some(local_model.clone()),
        _ => None,
    };

    ArmedFallback {
        health,
        registration,
        local_model,
    }
}

/// Identifies which provider MoA would route a fresh request to, given the
/// current configuration and runtime state. Useful for stamping observability
/// metadata onto chat responses (`X-MoA-Active-Provider` header,
/// `active_provider` JSON field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveProvider {
    /// Configured cloud provider (e.g. `"gemini"`, `"anthropic"`).
    Cloud { name: String },
    /// On-device Ollama with the local Gemma 4 tag.
    Local { model: String },
}

impl ActiveProvider {
    /// Short label suitable for an HTTP header value.
    pub fn label(&self) -> String {
        match self {
            ActiveProvider::Cloud { name } => name.clone(),
            ActiveProvider::Local { model } => format!("ollama:{model}"),
        }
    }

    /// Whether routing landed on the on-device path.
    pub fn is_local(&self) -> bool {
        matches!(self, ActiveProvider::Local { .. })
    }
}

/// Compute the active provider given configuration, API key presence, and
/// runtime network state. Encapsulates the patent §3.1 routing rules:
///
/// 1. `offline_force_local` set → always local (privacy-strict path)
/// 2. Network offline + local fallback armed → local
/// 3. No API key for primary cloud provider + local fallback armed → local
/// 4. Otherwise → primary cloud provider
pub fn decide_active_provider(
    primary_cloud: &str,
    has_cloud_api_key: bool,
    network_online: bool,
    reliability: &ReliabilityConfig,
) -> ActiveProvider {
    let local_armed = reliability.local_llm_fallback
        && reliability.fallback_providers.iter().any(|p| p == "ollama");

    if reliability.offline_force_local {
        return ActiveProvider::Local {
            model: reliability.local_llm_model.clone(),
        };
    }

    if !network_online && local_armed {
        return ActiveProvider::Local {
            model: reliability.local_llm_model.clone(),
        };
    }

    if !has_cloud_api_key && local_armed {
        return ActiveProvider::Local {
            model: reliability.local_llm_model.clone(),
        };
    }

    ActiveProvider::Cloud {
        name: primary_cloud.to_string(),
    }
}

/// One incremental progress event emitted while pulling a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullProgress {
    /// Free-form Ollama status string (e.g. "pulling manifest",
    /// "downloading", "verifying sha256 digest", "success").
    pub status: String,
    /// Layer digest currently being processed, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Total layer size in bytes, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// Bytes transferred so far for this layer, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_bytes: Option<u64>,
}

impl PullProgress {
    /// Fractional progress in `[0.0, 1.0]` for the current layer, if both
    /// `total_bytes` and `completed_bytes` are present.
    pub fn fraction(&self) -> Option<f32> {
        match (self.completed_bytes, self.total_bytes) {
            (Some(done), Some(total)) if total > 0 => {
                Some((done as f32 / total as f32).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    /// Whether this event indicates the pull completed successfully.
    pub fn is_success(&self) -> bool {
        self.status.eq_ignore_ascii_case("success")
    }
}

/// Summary of a model already installed on the local Ollama instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModel {
    /// Full tag, e.g. `gemma4:e4b`.
    pub name: String,
    /// On-disk size in bytes.
    pub size_bytes: u64,
    /// ISO 8601 modification timestamp reported by Ollama.
    #[serde(default)]
    pub modified_at: String,
}

/// Persisted choice of the default local model used by MoA's on-device
/// fallback path. Lives at `~/.moa/local_llm.toml` by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalLlmConfig {
    /// Ollama tag to use as the default local LLM (e.g. `gemma4:e4b`).
    pub default_model: String,
    /// ISO 8601 timestamp recording when this config was written.
    pub installed_at: String,
    /// Best-effort on-disk size in GB at install time.
    pub size_gb: f32,
}

impl LocalLlmConfig {
    /// Default config path: `~/.moa/local_llm.toml`.
    pub fn default_path() -> Result<PathBuf> {
        let home = home_dir().context("cannot determine home directory")?;
        Ok(home.join(".moa").join("local_llm.toml"))
    }

    /// Load a previously saved config from disk.
    pub async fn load(path: &Path) -> Result<Self> {
        let data = fs::read_to_string(path)
            .await
            .with_context(|| format!("reading local_llm config from {}", path.display()))?;
        toml::from_str(&data).context("parsing local_llm config TOML")
    }

    /// Save this config to disk (creates parent dirs as needed).
    pub async fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let toml_str = toml::to_string_pretty(self)?;
        fs::write(path, toml_str).await?;
        Ok(())
    }
}

// ── Daemon health ───────────────────────────────────────────────────────

/// Returns true when the Ollama daemon at `base_url` responds within 2s.
/// `base_url` should be the scheme+host+port without trailing slash, e.g.
/// `http://127.0.0.1:11434`.
pub async fn is_ollama_running(base_url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{base_url}/api/tags"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ── Installed model inventory ───────────────────────────────────────────

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagsModel>,
}

#[derive(Deserialize)]
struct TagsModel {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    modified_at: String,
}

/// List models currently installed on the local Ollama daemon.
pub async fn list_installed(base_url: &str) -> Result<Vec<InstalledModel>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base_url}/api/tags"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .context("calling /api/tags — is the Ollama daemon running?")?
        .error_for_status()
        .context("non-2xx response from /api/tags")?;
    let parsed: TagsResponse = resp
        .json()
        .await
        .context("parsing /api/tags JSON response")?;
    Ok(parsed
        .models
        .into_iter()
        .map(|m| InstalledModel {
            name: m.name,
            size_bytes: m.size,
            modified_at: m.modified_at,
        })
        .collect())
}

/// Returns true when a model matching `tag` (with or without `:latest`) is
/// already installed.
pub async fn is_installed(base_url: &str, tag: &str) -> Result<bool> {
    let installed = list_installed(base_url).await?;
    Ok(installed.iter().any(|m| matches_tag(&m.name, tag)))
}

/// Returns true when `installed_name` refers to the same model as the
/// user-supplied `requested_tag`, accounting for the implicit `:latest`
/// suffix that Ollama applies when no tag is given.
pub fn matches_tag(installed_name: &str, requested_tag: &str) -> bool {
    if installed_name == requested_tag {
        return true;
    }
    let normalize = |s: &str| -> String {
        if s.contains(':') {
            s.to_string()
        } else {
            format!("{s}:latest")
        }
    };
    normalize(installed_name) == normalize(requested_tag)
}

// ── Model pull with NDJSON progress ─────────────────────────────────────

#[derive(Serialize)]
struct PullRequest<'a> {
    model: &'a str,
    stream: bool,
}

/// Pull `tag` from the Ollama registry with streaming progress callbacks.
///
/// `on_progress` is invoked for each NDJSON event. The function returns
/// `Ok(())` on the final `success` event or `Err` on any reported error.
///
/// If the model is already installed, returns `Ok(())` immediately without
/// network activity.
pub async fn pull_model<F>(base_url: &str, tag: &str, mut on_progress: F) -> Result<()>
where
    F: FnMut(PullProgress) + Send,
{
    if is_installed(base_url, tag).await.unwrap_or(false) {
        on_progress(PullProgress {
            status: "already installed".to_string(),
            digest: None,
            total_bytes: None,
            completed_bytes: None,
        });
        on_progress(PullProgress {
            status: "success".to_string(),
            digest: None,
            total_bytes: None,
            completed_bytes: None,
        });
        return Ok(());
    }

    let client = reqwest::Client::builder()
        // No overall timeout — model pulls can take many minutes on slow
        // links. Per-event activity is implicit through chunk reads.
        .build()
        .context("building reqwest client for pull")?;

    let req = PullRequest {
        model: tag,
        stream: true,
    };
    let resp = client
        .post(format!("{base_url}/api/pull"))
        .json(&req)
        .send()
        .await
        .context("POST /api/pull failed — is the Ollama daemon running?")?
        .error_for_status()
        .context("non-2xx response starting /api/pull")?;

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut saw_success = false;

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("reading pull NDJSON chunk")?;
        buf.extend_from_slice(&bytes);

        // Drain complete lines from buf.
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let trimmed = std::str::from_utf8(&line)
                .context("non-UTF8 NDJSON line")?
                .trim();
            if trimmed.is_empty() {
                continue;
            }
            let event = parse_pull_event(trimmed)?;
            if event.is_success() {
                saw_success = true;
            }
            on_progress(event);
        }
    }
    // Handle any final partial line without trailing newline.
    if !buf.is_empty() {
        let trimmed = std::str::from_utf8(&buf)
            .context("non-UTF8 NDJSON tail")?
            .trim();
        if !trimmed.is_empty() {
            let event = parse_pull_event(trimmed)?;
            if event.is_success() {
                saw_success = true;
            }
            on_progress(event);
        }
    }

    if saw_success {
        Ok(())
    } else {
        anyhow::bail!("Ollama pull stream ended without 'success' event")
    }
}

/// Parse a single NDJSON event from `/api/pull`. Either a normal progress
/// event or an error envelope `{"error": "..."}`.
fn parse_pull_event(line: &str) -> Result<PullProgress> {
    // Try error envelope first.
    if let Ok(err) = serde_json::from_str::<PullErrorEnvelope>(line) {
        if !err.error.is_empty() {
            anyhow::bail!("Ollama pull error: {}", err.error);
        }
    }
    let raw: PullEventRaw =
        serde_json::from_str(line).with_context(|| format!("parsing NDJSON event: {line}"))?;
    Ok(PullProgress {
        status: raw.status.unwrap_or_default(),
        digest: raw.digest,
        total_bytes: raw.total,
        completed_bytes: raw.completed,
    })
}

#[derive(Deserialize)]
struct PullEventRaw {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    completed: Option<u64>,
}

#[derive(Deserialize)]
struct PullErrorEnvelope {
    #[serde(default)]
    error: String,
}

// ── Helpers ─────────────────────────────────────────────────────────────

// Use the shared helper in src/util.rs (PR #1 host_probe and PR #8 cosyvoice2
// used to define their own near-identical copies).
use crate::util::home_dir;

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn armed_reliability() -> ReliabilityConfig {
        let mut c = ReliabilityConfig::default();
        c.local_llm_fallback = true;
        c.local_llm_model = "gemma4:e4b".to_string();
        c.fallback_providers.push("ollama".to_string());
        c
    }

    #[tokio::test]
    async fn shared_health_returns_same_instance() {
        let h1 = shared_health();
        let h2 = shared_health();
        assert!(Arc::ptr_eq(&h1, &h2), "shared_health must memoize");
        // Default state is online (so first request still tries cloud).
        assert!(h1.is_online());
    }

    #[test]
    fn decide_active_provider_offline_force_local_wins() {
        let mut c = armed_reliability();
        c.offline_force_local = true;
        let decision = decide_active_provider("gemini", true, true, &c);
        assert!(decision.is_local());
        assert_eq!(decision.label(), "ollama:gemma4:e4b");
    }

    #[test]
    fn decide_active_provider_offline_routes_local_when_armed() {
        let c = armed_reliability();
        let decision = decide_active_provider("gemini", true, false, &c);
        assert!(decision.is_local());
    }

    #[test]
    fn decide_active_provider_offline_falls_back_to_cloud_when_not_armed() {
        // local_llm_fallback enabled but ollama not in fallback_providers
        let mut c = ReliabilityConfig::default();
        c.local_llm_fallback = true;
        let decision = decide_active_provider("gemini", true, false, &c);
        // Without armed local, the decision is still cloud — caller will
        // discover the failure via the existing ReliableProvider retry chain.
        match decision {
            ActiveProvider::Cloud { name } => assert_eq!(name, "gemini"),
            _ => panic!("expected cloud"),
        }
    }

    #[test]
    fn decide_active_provider_no_api_key_routes_local() {
        let c = armed_reliability();
        let decision = decide_active_provider("gemini", false, true, &c);
        assert!(decision.is_local());
    }

    #[test]
    fn decide_active_provider_happy_path_picks_cloud() {
        let c = armed_reliability();
        let decision = decide_active_provider("gemini", true, true, &c);
        match decision {
            ActiveProvider::Cloud { name } => assert_eq!(name, "gemini"),
            _ => panic!("expected cloud, got {decision:?}"),
        }
    }

    #[test]
    fn active_provider_label_is_round_trippable() {
        let cloud = ActiveProvider::Cloud {
            name: "anthropic".to_string(),
        };
        assert_eq!(cloud.label(), "anthropic");
        assert!(!cloud.is_local());
        let local = ActiveProvider::Local {
            model: "gemma4:e4b".to_string(),
        };
        assert_eq!(local.label(), "ollama:gemma4:e4b");
        assert!(local.is_local());
    }

    #[test]
    fn matches_tag_exact() {
        assert!(matches_tag("gemma4:e4b", "gemma4:e4b"));
        assert!(!matches_tag("gemma4:e4b", "gemma4:e2b"));
    }

    #[test]
    fn matches_tag_implicit_latest() {
        // User asks for "gemma4" → matches "gemma4:latest"
        assert!(matches_tag("gemma4:latest", "gemma4"));
        assert!(matches_tag("gemma4", "gemma4:latest"));
        // Different explicit tag should not match latest.
        assert!(!matches_tag("gemma4:e4b", "gemma4:latest"));
    }

    #[test]
    fn pull_progress_fraction() {
        let p = PullProgress {
            status: "downloading".to_string(),
            digest: Some("sha256:abc".to_string()),
            total_bytes: Some(1000),
            completed_bytes: Some(250),
        };
        assert!((p.fraction().unwrap() - 0.25).abs() < 1e-6);

        let p_zero = PullProgress {
            status: "downloading".to_string(),
            digest: None,
            total_bytes: Some(0),
            completed_bytes: Some(0),
        };
        assert_eq!(p_zero.fraction(), None);

        let p_partial = PullProgress {
            status: "pulling manifest".to_string(),
            digest: None,
            total_bytes: None,
            completed_bytes: None,
        };
        assert_eq!(p_partial.fraction(), None);
    }

    #[test]
    fn pull_progress_success_detection() {
        let success = PullProgress {
            status: "success".to_string(),
            digest: None,
            total_bytes: None,
            completed_bytes: None,
        };
        assert!(success.is_success());

        let mid = PullProgress {
            status: "downloading".to_string(),
            digest: None,
            total_bytes: Some(100),
            completed_bytes: Some(50),
        };
        assert!(!mid.is_success());
    }

    #[test]
    fn parse_pull_event_progress() {
        let line =
            r#"{"status":"downloading","digest":"sha256:abc","total":2048,"completed":1024}"#;
        let p = parse_pull_event(line).unwrap();
        assert_eq!(p.status, "downloading");
        assert_eq!(p.digest.as_deref(), Some("sha256:abc"));
        assert_eq!(p.total_bytes, Some(2048));
        assert_eq!(p.completed_bytes, Some(1024));
        assert!((p.fraction().unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn parse_pull_event_status_only() {
        let line = r#"{"status":"pulling manifest"}"#;
        let p = parse_pull_event(line).unwrap();
        assert_eq!(p.status, "pulling manifest");
        assert!(p.digest.is_none());
        assert!(p.total_bytes.is_none());
    }

    #[test]
    fn parse_pull_event_success() {
        let line = r#"{"status":"success"}"#;
        let p = parse_pull_event(line).unwrap();
        assert!(p.is_success());
    }

    #[test]
    fn parse_pull_event_error_envelope() {
        let line = r#"{"error":"model not found"}"#;
        let err = parse_pull_event(line).expect_err("error envelope must fail");
        let msg = format!("{err}");
        assert!(msg.contains("model not found"));
    }

    #[tokio::test]
    async fn config_roundtrip() {
        let cfg = LocalLlmConfig {
            default_model: "gemma4:e4b".to_string(),
            installed_at: "2026-04-16T03:30:00Z".to_string(),
            size_gb: 3.0,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local_llm.toml");
        cfg.save(&path).await.unwrap();
        let loaded = LocalLlmConfig::load(&path).await.unwrap();
        assert_eq!(loaded.default_model, "gemma4:e4b");
        assert!((loaded.size_gb - 3.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn ollama_daemon_health_returns_bool() {
        // Unreachable port — must return false fast (within timeout).
        let alive = is_ollama_running("http://127.0.0.1:1").await;
        assert!(!alive);
    }

    /// Manual integration test against a live Ollama daemon. Verifies that
    /// `list_installed` returns at least one model and that `is_installed`
    /// matches an existing tag. Requires `ollama serve` running.
    /// Run with:
    ///     cargo test --lib local_llm::tests::live_list_installed -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_list_installed() {
        if !is_ollama_running(DEFAULT_OLLAMA_URL).await {
            eprintln!("skipping: Ollama daemon not reachable at {DEFAULT_OLLAMA_URL}");
            return;
        }
        let models = list_installed(DEFAULT_OLLAMA_URL).await.unwrap();
        println!("\nInstalled models ({}):", models.len());
        for m in &models {
            let gb = m.size_bytes as f32 / (1024.0 * 1024.0 * 1024.0);
            println!("  {:30}  {:>6.2} GB  {}", m.name, gb, m.modified_at);
        }
        if let Some(first) = models.first() {
            assert!(is_installed(DEFAULT_OLLAMA_URL, &first.name).await.unwrap());
        }
    }

    /// Manual integration test that pulls (or re-checks) `gemma4:e4b`.
    /// If the model is already installed, returns instantly. Run with:
    ///     cargo test --lib local_llm::tests::live_pull_gemma4_e4b -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_pull_gemma4_e4b() {
        if !is_ollama_running(DEFAULT_OLLAMA_URL).await {
            eprintln!("skipping: Ollama daemon not reachable");
            return;
        }
        let mut last_status = String::new();
        let result = pull_model(DEFAULT_OLLAMA_URL, "gemma4:e4b", |p| {
            // Print one line per status change to keep output readable.
            if p.status != last_status {
                println!(
                    "[{}] digest={} {}",
                    p.status,
                    p.digest.as_deref().unwrap_or("-"),
                    p.fraction()
                        .map(|f| format!("{:>5.1}%", f * 100.0))
                        .unwrap_or_else(|| "—".to_string())
                );
                last_status = p.status.clone();
            }
        })
        .await;
        result.expect("pull should succeed");
    }
}
