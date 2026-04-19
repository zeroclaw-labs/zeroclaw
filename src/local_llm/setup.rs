//! End-to-end first-time setup orchestrator for the on-device Gemma 4 path.
//!
//! Chains the existing building blocks so `moa setup local-llm` and the Tauri
//! onboarding wizard share one flow:
//!
//! 1. [`crate::host_probe::probe`] — pick a Gemma 4 tier from host capabilities
//! 2. Disk-space gate (≥ 30 GB free) per the deployment checklist
//! 3. [`crate::local_llm::installer`] — install Ollama runtime when absent
//! 4. Wait for the daemon to accept HTTP on `127.0.0.1:11434`
//! 5. [`crate::local_llm::pull_model`] — pull the recommended tag with
//!    exponential-backoff retries (2s → 4s → 8s)
//! 6. Persist [`crate::host_probe::HardwareProfile`] +
//!    [`crate::local_llm::LocalLlmConfig`] under `~/.moa/`
//!
//! Callers supply progress callbacks so each stage can drive a CLI spinner,
//! a Tauri event, or a headless log sink without this module knowing about
//! any UI.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::host_probe::{self, HardwareProfile, Tier};
use crate::local_llm::installer::{
    self, detect_install_method, install_ollama, is_ollama_installed, InstallMethod,
    InstallProgress,
};
use crate::local_llm::{
    is_ollama_running, pull_model, LocalLlmConfig, PullProgress, DEFAULT_OLLAMA_URL,
};

/// Minimum free disk space before setup is allowed to start (MB).
///
/// Chosen per the §9 deployment checklist: the largest Gemma 4 tier is ~10 GB
/// on-wire, ~17–20 GB resident, so 30 GB keeps headroom for Ollama's layer
/// cache, `.ollama/models/blobs` deduplication, and per-tier upgrade swaps.
const MIN_FREE_DISK_MB: u64 = 30 * 1024;

/// Maximum number of retry attempts for the model-pull step.
const PULL_MAX_ATTEMPTS: u32 = 3;

/// Initial backoff before the second pull attempt (doubles each retry).
const PULL_INITIAL_BACKOFF: Duration = Duration::from_secs(2);

/// How long to wait for the daemon to accept HTTP after install.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to re-poll the daemon while waiting for readiness.
const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// High-level stages a consumer UI can render.
///
/// Emitted by [`run_setup`] via the `on_stage` callback, independently of the
/// finer-grained install/pull progress streams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SetupStage {
    /// Detecting RAM / GPU / disk / OS.
    Probing,
    /// Verifying free disk against [`MIN_FREE_DISK_MB`].
    CheckingDisk,
    /// Running the OS-matched Ollama installer.
    InstallingOllama,
    /// Polling the Ollama HTTP endpoint until it is reachable.
    WaitingForDaemon,
    /// `/api/pull` stream is active for the recommended tag.
    PullingModel {
        /// 1-indexed attempt number (1 on the first try, up to
        /// [`PULL_MAX_ATTEMPTS`]).
        attempt: u32,
    },
    /// Writing `~/.moa/hardware_profile.json` and `~/.moa/local_llm.toml`.
    Persisting,
    /// Setup finished without recoverable errors.
    Done,
}

/// Inputs for [`run_setup`]. All fields are optional so callers can tune the
/// flow without reconstructing the whole struct.
#[derive(Debug, Clone)]
pub struct SetupOptions {
    /// Override the auto-picked tier (e.g. user chose E2B in the UI dropdown
    /// even though host_probe recommended E4B).
    pub override_tier: Option<Tier>,
    /// Override the Ollama base URL. Defaults to [`DEFAULT_OLLAMA_URL`].
    pub base_url: Option<String>,
    /// When `true`, apply [`Tier::downgrade`] once in `host_probe::probe`.
    pub conservative_downgrade: bool,
    /// When `true` and the daemon is not reachable, attempt to launch
    /// `ollama serve` in the background. Default `false` — in production the
    /// Ollama app / systemd unit normally keeps the daemon alive.
    pub try_start_daemon: bool,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            override_tier: None,
            base_url: None,
            conservative_downgrade: true,
            try_start_daemon: false,
        }
    }
}

/// Outcome returned to the caller on success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupReport {
    /// Hardware snapshot that drove tier selection.
    pub profile: HardwareProfile,
    /// Tier actually installed (may differ from the recommendation if the
    /// caller passed [`SetupOptions::override_tier`]).
    pub installed_tier: Tier,
    /// Ollama tag that was pulled / verified.
    pub model_tag: String,
    /// Whether Ollama had to be installed during this run.
    pub ollama_installed_now: bool,
    /// Number of pull attempts until the first success.
    pub pull_attempts: u32,
    /// Path where the hardware profile was written.
    pub hardware_profile_path: PathBuf,
    /// Path where the local-LLM config was written.
    pub local_llm_config_path: PathBuf,
}

/// Progress callbacks. Keep them `FnMut` so callers can accumulate into their
/// own buffers without an extra `Arc<Mutex<_>>` layer.
pub struct SetupCallbacks<'a> {
    /// High-level stage transitions.
    pub on_stage: &'a mut (dyn FnMut(SetupStage) + Send),
    /// Per-line installer progress (downloading, running, verifying, done).
    pub on_install_progress: &'a mut (dyn FnMut(InstallProgress) + Send),
    /// Per-event model pull progress (NDJSON from `/api/pull`).
    pub on_pull_progress: &'a mut (dyn FnMut(PullProgress) + Send),
}

/// Run the full first-time setup flow.
///
/// The caller (CLI, Tauri onboarding wizard, or headless installer) is
/// responsible for having already obtained user consent to run the OS
/// installer and download several GB over the network.
///
/// # Errors
/// * disk < [`MIN_FREE_DISK_MB`]
/// * Ollama install fails or daemon never comes up
/// * model pull fails after [`PULL_MAX_ATTEMPTS`] retries
pub async fn run_setup(
    opts: SetupOptions,
    callbacks: &mut SetupCallbacks<'_>,
) -> Result<SetupReport> {
    let base_url = opts
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_OLLAMA_URL)
        .to_string();

    // 1. Probe host hardware.
    (callbacks.on_stage)(SetupStage::Probing);
    let profile = host_probe::probe(opts.conservative_downgrade)
        .await
        .context("host hardware probe failed")?;

    // Pick the tier. Respect an explicit override from the UI, otherwise use
    // the probe's recommendation, subject to the mobile T1/T2 cap.
    let tier = select_tier(&profile, opts.override_tier);
    let tag = tier.ollama_tag().to_string();

    // 2. Disk gate.
    (callbacks.on_stage)(SetupStage::CheckingDisk);
    check_disk(&profile)?;

    // 3. Install Ollama if absent.
    let mut ollama_installed_now = false;
    if !is_ollama_installed().await {
        (callbacks.on_stage)(SetupStage::InstallingOllama);
        let method = detect_install_method().await;
        ensure_automated_install_available(&method)?;
        install_ollama(&method, |p| (callbacks.on_install_progress)(p))
            .await
            .context("Ollama runtime installation failed")?;
        ollama_installed_now = true;
    }

    // 4. Wait for the daemon.
    (callbacks.on_stage)(SetupStage::WaitingForDaemon);
    wait_for_daemon(&base_url, opts.try_start_daemon).await?;

    // 5. Pull the tier tag with exponential backoff.
    let pull_attempts = pull_with_retries(&base_url, &tag, callbacks).await?;

    // 6. Persist profile + local-LLM config.
    (callbacks.on_stage)(SetupStage::Persisting);
    let hardware_profile_path = HardwareProfile::default_path()?;
    profile.save(&hardware_profile_path).await.with_context(|| {
        format!(
            "writing hardware profile to {}",
            hardware_profile_path.display()
        )
    })?;

    let local_llm_config = LocalLlmConfig {
        default_model: tag.clone(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        size_gb: tier.download_size_gb(),
    };
    let local_llm_config_path = LocalLlmConfig::default_path()?;
    local_llm_config
        .save(&local_llm_config_path)
        .await
        .with_context(|| {
            format!(
                "writing local-LLM config to {}",
                local_llm_config_path.display()
            )
        })?;

    (callbacks.on_stage)(SetupStage::Done);
    Ok(SetupReport {
        profile,
        installed_tier: tier,
        model_tag: tag,
        ollama_installed_now,
        pull_attempts,
        hardware_profile_path,
        local_llm_config_path,
    })
}

/// Enforce the `T1|T2` mobile cap and honour caller overrides.
///
/// Kept public-in-module so the CLI can render a preview ("would install …")
/// without running the rest of the flow.
fn select_tier(profile: &HardwareProfile, override_tier: Option<Tier>) -> Tier {
    let base = override_tier.unwrap_or(profile.recommended_tier);
    if is_mobile_os(&profile.os) && matches!(base, Tier::T3MoE26B | Tier::T4Dense31B) {
        // Mobile constraint (§2.2): never run >= 26B on phones. Downgrade twice
        // — T4 → T3 → T2 — to land on the largest mobile-legal tier.
        return Tier::T2E4B;
    }
    base
}

fn is_mobile_os(os: &str) -> bool {
    matches!(os, "iOS" | "Android")
}

fn check_disk(profile: &HardwareProfile) -> Result<()> {
    if profile.disk_free_mb == 0 {
        // detect_disk_free returned 0 only on probe failure. Don't hard-fail;
        // the user can still proceed at their own risk.
        return Ok(());
    }
    if profile.disk_free_mb < MIN_FREE_DISK_MB {
        bail!(
            "not enough free disk: {} MB available, {} MB required (§9 deployment checklist)",
            profile.disk_free_mb,
            MIN_FREE_DISK_MB
        );
    }
    Ok(())
}

fn ensure_automated_install_available(method: &InstallMethod) -> Result<()> {
    if let InstallMethod::Manual { instructions } = method {
        bail!(
            "automated Ollama install is not supported on this host; \
             instructions for the user: {instructions}"
        );
    }
    Ok(())
}

async fn wait_for_daemon(base_url: &str, try_start_daemon: bool) -> Result<()> {
    if is_ollama_running(base_url).await {
        return Ok(());
    }
    if try_start_daemon {
        // Best-effort background spawn; ignore errors because a missing binary
        // is already covered by the `is_ollama_installed` check upstream.
        let _ = tokio::process::Command::new("ollama")
            .arg("serve")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    let deadline = tokio::time::Instant::now() + DAEMON_READY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if is_ollama_running(base_url).await {
            return Ok(());
        }
        tokio::time::sleep(DAEMON_POLL_INTERVAL).await;
    }
    bail!(
        "Ollama daemon did not become reachable at {base_url} within {:?}",
        DAEMON_READY_TIMEOUT
    )
}

/// Drive `pull_model` with exponential backoff. Returns the 1-indexed attempt
/// number that eventually succeeded.
async fn pull_with_retries(
    base_url: &str,
    tag: &str,
    callbacks: &mut SetupCallbacks<'_>,
) -> Result<u32> {
    let mut last_err: Option<anyhow::Error> = None;
    let mut backoff = PULL_INITIAL_BACKOFF;

    for attempt in 1..=PULL_MAX_ATTEMPTS {
        (callbacks.on_stage)(SetupStage::PullingModel { attempt });
        let result = pull_model(base_url, tag, |p| (callbacks.on_pull_progress)(p)).await;
        match result {
            Ok(()) => return Ok(attempt),
            Err(e) => {
                last_err = Some(e);
                if attempt < PULL_MAX_ATTEMPTS {
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("pull failed with no underlying error captured"))
        .context(format!(
            "model pull `{tag}` failed after {PULL_MAX_ATTEMPTS} attempts"
        )))
}

// Re-export for callers who build `SetupCallbacks` with default no-ops.
pub use installer::InstallProgress as InstallerProgress;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_probe::GpuType;

    fn profile_with(os: &str, disk_mb: u64, tier: Tier) -> HardwareProfile {
        HardwareProfile {
            os: os.to_string(),
            arch: "aarch64".to_string(),
            total_ram_mb: 16 * 1024,
            available_ram_mb: 8 * 1024,
            cpu_cores: 8,
            gpu: GpuType::AppleSilicon {
                chip: "Apple M2".to_string(),
            },
            disk_free_mb: disk_mb,
            recommended_tier: tier,
            downgraded: false,
            probed_at: "2026-04-19T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn mobile_cap_downgrades_t4_to_t2() {
        let p = profile_with("iOS", 200_000, Tier::T4Dense31B);
        assert_eq!(select_tier(&p, None), Tier::T2E4B);
        let p = profile_with("Android", 200_000, Tier::T3MoE26B);
        assert_eq!(select_tier(&p, None), Tier::T2E4B);
    }

    #[test]
    fn desktop_keeps_recommended_tier() {
        let p = profile_with("macOS", 200_000, Tier::T3MoE26B);
        assert_eq!(select_tier(&p, None), Tier::T3MoE26B);
        let p = profile_with("Linux", 200_000, Tier::T4Dense31B);
        assert_eq!(select_tier(&p, None), Tier::T4Dense31B);
    }

    #[test]
    fn override_wins_on_desktop_but_still_capped_on_mobile() {
        let desktop = profile_with("Windows", 200_000, Tier::T2E4B);
        assert_eq!(
            select_tier(&desktop, Some(Tier::T4Dense31B)),
            Tier::T4Dense31B
        );
        let mobile = profile_with("iOS", 200_000, Tier::T1E2B);
        // User asked for 31B on a phone — still capped.
        assert_eq!(select_tier(&mobile, Some(Tier::T4Dense31B)), Tier::T2E4B);
    }

    #[test]
    fn disk_gate_rejects_below_threshold() {
        let p = profile_with("Linux", MIN_FREE_DISK_MB - 1, Tier::T2E4B);
        let err = check_disk(&p).expect_err("must reject insufficient disk");
        assert!(
            format!("{err:#}").contains("not enough free disk"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn disk_gate_allows_zero_probe_result() {
        // disk_free_mb == 0 means probe failed; don't block setup.
        let p = profile_with("Linux", 0, Tier::T2E4B);
        check_disk(&p).expect("zero should be tolerated");
    }

    #[test]
    fn disk_gate_allows_threshold_and_above() {
        let p = profile_with("Linux", MIN_FREE_DISK_MB, Tier::T2E4B);
        check_disk(&p).expect("exactly at threshold should pass");
        let p2 = profile_with("Linux", MIN_FREE_DISK_MB * 2, Tier::T4Dense31B);
        check_disk(&p2).expect("well above threshold should pass");
    }

    #[test]
    fn manual_install_method_is_rejected() {
        let m = InstallMethod::Manual {
            instructions: "visit ollama.com".to_string(),
        };
        ensure_automated_install_available(&m).expect_err("manual must error");
    }

    #[test]
    fn automated_install_methods_are_accepted() {
        ensure_automated_install_available(&InstallMethod::BrewMacOS).unwrap();
        ensure_automated_install_available(&InstallMethod::OfficialScriptUnix).unwrap();
        ensure_automated_install_available(&InstallMethod::WindowsMsi).unwrap();
    }

    #[tokio::test]
    async fn wait_for_daemon_times_out_on_closed_port() {
        // Port 1 is reliably unreachable.
        let start = std::time::Instant::now();
        let err = wait_for_daemon("http://127.0.0.1:1", false)
            .await
            .expect_err("closed port must time out");
        assert!(format!("{err:#}").contains("did not become reachable"));
        // Sanity: we waited roughly the configured timeout (allow +/- 5s slack
        // for slow CI). Lower bound is what matters — we must not return fast.
        assert!(start.elapsed() >= DAEMON_READY_TIMEOUT.saturating_sub(Duration::from_secs(1)));
    }
}
