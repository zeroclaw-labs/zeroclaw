//! Host hardware probing for Gemma 4 model tier selection.
//!
//! Detects system RAM, GPU VRAM, CPU capabilities, and disk space to determine
//! the optimal Gemma 4 model tier (T1–T4) for on-device LLM inference via Ollama.
//!
//! This module is distinct from `src/hardware/` which handles USB peripheral
//! enumeration (STM32, RPi GPIO).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::process::Command;

/// Gemma 4 model tier, ordered by resource requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// Gemma 4 E2B (effective 2B params). ~4 GB memory. Text + image + audio.
    T1E2B,
    /// Gemma 4 E4B (effective 4B params). ~5.5–6 GB memory. Text + image + audio.
    T2E4B,
    /// Gemma 4 26B A4B MoE (active 4B). ~15 GB memory. Text + image.
    T3MoE26B,
    /// Gemma 4 31B Dense. ~17–20 GB memory. Text + image.
    T4Dense31B,
}

impl Tier {
    /// Ollama model tag for this tier.
    pub fn ollama_tag(&self) -> &'static str {
        match self {
            Tier::T1E2B => "gemma4:e2b",
            Tier::T2E4B => "gemma4:e4b",
            Tier::T3MoE26B => "gemma4:26b",
            Tier::T4Dense31B => "gemma4:31b",
        }
    }

    /// Approximate download size in GB (Q4_K_M quantization).
    pub fn download_size_gb(&self) -> f32 {
        match self {
            Tier::T1E2B => 2.0,
            Tier::T2E4B => 3.0,
            Tier::T3MoE26B => 8.0,
            Tier::T4Dense31B => 10.0,
        }
    }

    /// Approximate memory required for inference in GB.
    pub fn required_memory_gb(&self) -> f32 {
        match self {
            Tier::T1E2B => 4.0,
            Tier::T2E4B => 5.5,
            Tier::T3MoE26B => 15.0,
            Tier::T4Dense31B => 17.0,
        }
    }

    /// Whether this tier supports native audio input.
    pub fn supports_audio(&self) -> bool {
        matches!(self, Tier::T1E2B | Tier::T2E4B)
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Tier::T1E2B => "T1 Minimum (Gemma 4 E2B)",
            Tier::T2E4B => "T2 Standard (Gemma 4 E4B)",
            Tier::T3MoE26B => "T3 High-perf (Gemma 4 26B MoE)",
            Tier::T4Dense31B => "T4 Workstation (Gemma 4 31B Dense)",
        }
    }

    /// One tier below, for conservative downgrade. Returns self if already T1.
    // Each arm encodes a distinct downgrade rule; merging T1→T1 and T2→T1 would
    // hide the saturation semantic at T1.
    #[allow(clippy::match_same_arms)]
    pub fn downgrade(&self) -> Tier {
        match self {
            Tier::T1E2B => Tier::T1E2B,
            Tier::T2E4B => Tier::T1E2B,
            Tier::T3MoE26B => Tier::T2E4B,
            Tier::T4Dense31B => Tier::T3MoE26B,
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// GPU type detected on the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GpuType {
    /// NVIDIA discrete GPU with dedicated VRAM.
    Nvidia { name: String, vram_mb: u64 },
    /// AMD discrete GPU with dedicated VRAM.
    Amd { name: String, vram_mb: u64 },
    /// Apple Silicon with unified memory (shared CPU/GPU).
    AppleSilicon { chip: String },
    /// Integrated GPU only (Intel HD, etc.) — no dedicated VRAM.
    Integrated,
    /// No GPU detected or detection failed.
    None,
}

/// Snapshot of host hardware capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// Operating system (e.g. "macOS", "Linux", "Windows").
    pub os: String,
    /// CPU architecture (e.g. "aarch64", "x86_64").
    pub arch: String,
    /// Total system RAM in MB.
    pub total_ram_mb: u64,
    /// Available (free) RAM in MB at probe time.
    pub available_ram_mb: u64,
    /// CPU logical core count.
    pub cpu_cores: usize,
    /// Detected GPU type.
    pub gpu: GpuType,
    /// Available disk space in MB on the data partition.
    pub disk_free_mb: u64,
    /// Recommended tier based on detection.
    pub recommended_tier: Tier,
    /// Whether conservative downgrade was applied.
    pub downgraded: bool,
    /// ISO 8601 timestamp of this probe.
    pub probed_at: String,
}

impl HardwareProfile {
    /// Default path for persisted profile: `~/.moa/hardware_profile.json`.
    pub fn default_path() -> Result<PathBuf> {
        let home = dirs_home().context("cannot determine home directory")?;
        Ok(home.join(".moa").join("hardware_profile.json"))
    }

    /// Load a previously saved profile from disk.
    pub async fn load(path: &Path) -> Result<Self> {
        let data = fs::read_to_string(path)
            .await
            .with_context(|| format!("reading hardware profile from {}", path.display()))?;
        serde_json::from_str(&data).context("parsing hardware profile JSON")
    }

    /// Save this profile to disk (creates parent dirs if needed).
    pub async fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).await?;
        Ok(())
    }
}

/// Probe the host hardware and determine the optimal Gemma 4 tier.
///
/// If `conservative` is true, the tier is downgraded by one step when the
/// detected effective memory is within 20% of the tier boundary (accounting
/// for OS and other app memory pressure).
pub async fn probe(conservative: bool) -> Result<HardwareProfile> {
    let os = detect_os();
    let arch = std::env::consts::ARCH.to_string();
    let (total_ram_mb, available_ram_mb) = detect_ram()?;
    let cpu_cores = detect_cpu_cores();
    let gpu = detect_gpu().await;
    let disk_free_mb = detect_disk_free().await.unwrap_or(0);

    let effective_mem_gb = compute_effective_memory_gb(total_ram_mb, &gpu);
    let mut tier = tier_from_effective_memory(effective_mem_gb);
    let mut downgraded = false;

    if conservative {
        // Apply conservative downgrade: if effective memory is within 20% of
        // the tier's requirement, step down one tier.
        let headroom = effective_mem_gb / tier.required_memory_gb();
        if headroom < 1.2 {
            tier = tier.downgrade();
            downgraded = tier != Tier::T1E2B || effective_mem_gb < 4.0;
        }
    }

    let now = chrono::Utc::now().to_rfc3339();

    Ok(HardwareProfile {
        os,
        arch,
        total_ram_mb,
        available_ram_mb,
        cpu_cores,
        gpu,
        disk_free_mb,
        recommended_tier: tier,
        downgraded,
        probed_at: now,
    })
}

// ── Tier selection ──────────────────────────────────────────────────────

/// Map effective GPU/unified memory to a tier.
///
/// Boundaries are chosen so each tier has roughly 50% headroom over the model's
/// minimum requirement; the conservative downgrade in `probe()` shrinks that
/// headroom further when the device is near a tier boundary.
fn tier_from_effective_memory(effective_mem_gb: f32) -> Tier {
    if effective_mem_gb < 6.0 {
        Tier::T1E2B
    } else if effective_mem_gb < 10.0 {
        Tier::T2E4B
    } else if effective_mem_gb < 20.0 {
        Tier::T3MoE26B
    } else {
        Tier::T4Dense31B
    }
}

/// Compute effective memory available for LLM inference in GB.
///
/// Priority: dedicated GPU VRAM > Apple Silicon unified (70%) > system RAM minus OS overhead.
fn compute_effective_memory_gb(total_ram_mb: u64, gpu: &GpuType) -> f32 {
    match gpu {
        GpuType::Nvidia { vram_mb, .. } | GpuType::Amd { vram_mb, .. } => *vram_mb as f32 / 1024.0,
        GpuType::AppleSilicon { .. } => {
            // Apple Silicon can allocate ~70% of unified memory to GPU workloads.
            (total_ram_mb as f32 / 1024.0) * 0.70
        }
        GpuType::Integrated | GpuType::None => {
            // CPU-only inference: subtract ~4 GB for OS overhead.
            let total_gb = total_ram_mb as f32 / 1024.0;
            (total_gb - 4.0).max(1.0)
        }
    }
}

// ── OS / CPU / RAM detection ────────────────────────────────────────────

fn detect_os() -> String {
    match std::env::consts::OS {
        "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        "windows" => "Windows".to_string(),
        other => other.to_string(),
    }
}

fn detect_cpu_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn detect_ram() -> Result<(u64, u64)> {
    // Use sysinfo for cross-platform RAM detection.
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory() / (1024 * 1024); // bytes → MB
    let available = sys.available_memory() / (1024 * 1024);
    Ok((total, available))
}

// ── GPU detection ───────────────────────────────────────────────────────

/// Detect GPU type and VRAM. Uses command-line tools to avoid heavy native
/// dependencies (nvml-wrapper, wgpu). Falls back gracefully on each platform.
async fn detect_gpu() -> GpuType {
    // 1. Check Apple Silicon first (macOS + aarch64).
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        if let Some(chip) = detect_apple_silicon_chip().await {
            return GpuType::AppleSilicon { chip };
        }
    }

    // 2. Try NVIDIA via nvidia-smi (cross-platform).
    if let Some(gpu) = detect_nvidia_gpu().await {
        return gpu;
    }

    // 3. Try AMD on Linux via sysfs.
    #[cfg(target_os = "linux")]
    if let Some(gpu) = detect_amd_gpu_linux().await {
        return gpu;
    }

    GpuType::None
}

async fn detect_apple_silicon_chip() -> Option<String> {
    let output = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .await
        .ok()?;
    if output.status.success() {
        let brand = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if brand.contains("Apple") {
            return Some(brand);
        }
    }
    None
}

async fn detect_nvidia_gpu() -> Option<GpuType> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?.trim();
    let mut parts = line.splitn(2, ',');
    let name = parts.next()?.trim().to_string();
    let vram_str = parts.next()?.trim();
    let vram_mb: u64 = vram_str.parse().ok()?;
    Some(GpuType::Nvidia { name, vram_mb })
}

#[cfg(target_os = "linux")]
async fn detect_amd_gpu_linux() -> Option<GpuType> {
    use tokio::fs as afs;
    // Scan /sys/class/drm/card*/device/mem_info_vram_total for AMD GPUs.
    let mut entries = afs::read_dir("/sys/class/drm").await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let vram_path = entry.path().join("device/mem_info_vram_total");
        if let Ok(content) = afs::read_to_string(&vram_path).await {
            if let Ok(vram_bytes) = content.trim().parse::<u64>() {
                let vram_mb = vram_bytes / (1024 * 1024);
                if vram_mb > 0 {
                    // Try to read device name.
                    let dev_name = afs::read_to_string(entry.path().join("device/product_name"))
                        .await
                        .unwrap_or_else(|_| "AMD GPU".to_string())
                        .trim()
                        .to_string();
                    return Some(GpuType::Amd {
                        name: dev_name,
                        vram_mb,
                    });
                }
            }
        }
    }
    None
}

// ── Disk space detection ────────────────────────────────────────────────

async fn detect_disk_free() -> Result<u64> {
    let home = dirs_home().context("cannot determine home directory")?;
    disk_free_mb(&home).await
}

#[cfg(unix)]
async fn disk_free_mb(path: &Path) -> Result<u64> {
    let output = Command::new("df")
        .args(["-m", &path.to_string_lossy()])
        .output()
        .await
        .context("running df")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // df -m output: Filesystem 1M-blocks Used Available ...
    // Parse the Available column from the second line.
    let line = stdout
        .lines()
        .nth(1)
        .context("unexpected df output format")?;
    let available = line
        .split_whitespace()
        .nth(3)
        .context("cannot parse df available column")?;
    available.parse::<u64>().context("parsing df available MB")
}

#[cfg(windows)]
async fn disk_free_mb(path: &Path) -> Result<u64> {
    // On Windows, use the drive letter from the path.
    let drive = path
        .components()
        .next()
        .context("no drive component")?
        .as_os_str()
        .to_string_lossy()
        .to_string();
    let output = Command::new("wmic")
        .args([
            "logicaldisk",
            "where",
            &format!("DeviceID='{drive}'"),
            "get",
            "FreeSpace",
            "/format:value",
        ])
        .output()
        .await
        .context("running wmic")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(val) = line.strip_prefix("FreeSpace=") {
            let bytes: u64 = val.trim().parse().context("parsing FreeSpace")?;
            return Ok(bytes / (1024 * 1024));
        }
    }
    anyhow::bail!("could not parse wmic FreeSpace output")
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn dirs_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_ordering() {
        assert!(Tier::T1E2B < Tier::T2E4B);
        assert!(Tier::T2E4B < Tier::T3MoE26B);
        assert!(Tier::T3MoE26B < Tier::T4Dense31B);
    }

    #[test]
    fn tier_from_memory_boundaries() {
        assert_eq!(tier_from_effective_memory(3.5), Tier::T1E2B);
        assert_eq!(tier_from_effective_memory(5.0), Tier::T1E2B);
        assert_eq!(tier_from_effective_memory(6.0), Tier::T2E4B);
        assert_eq!(tier_from_effective_memory(9.9), Tier::T2E4B);
        assert_eq!(tier_from_effective_memory(10.0), Tier::T3MoE26B);
        assert_eq!(tier_from_effective_memory(19.9), Tier::T3MoE26B);
        assert_eq!(tier_from_effective_memory(20.0), Tier::T4Dense31B);
        assert_eq!(tier_from_effective_memory(48.0), Tier::T4Dense31B);
    }

    #[test]
    fn effective_memory_apple_silicon() {
        let gpu = GpuType::AppleSilicon {
            chip: "Apple M2".to_string(),
        };
        // 16 GB unified → 16 * 0.70 = 11.2 GB effective
        let eff = compute_effective_memory_gb(16 * 1024, &gpu);
        assert!((eff - 11.2).abs() < 0.5);
    }

    #[test]
    fn effective_memory_nvidia() {
        let gpu = GpuType::Nvidia {
            name: "RTX 4090".to_string(),
            vram_mb: 24576,
        };
        let eff = compute_effective_memory_gb(32 * 1024, &gpu);
        assert!((eff - 24.0).abs() < 0.5);
    }

    #[test]
    fn effective_memory_cpu_only() {
        let gpu = GpuType::None;
        // 8 GB - 4 GB overhead = 4 GB
        let eff = compute_effective_memory_gb(8 * 1024, &gpu);
        assert!((eff - 4.0).abs() < 0.5);
    }

    #[test]
    fn downgrade_chain() {
        assert_eq!(Tier::T4Dense31B.downgrade(), Tier::T3MoE26B);
        assert_eq!(Tier::T3MoE26B.downgrade(), Tier::T2E4B);
        assert_eq!(Tier::T2E4B.downgrade(), Tier::T1E2B);
        assert_eq!(Tier::T1E2B.downgrade(), Tier::T1E2B);
    }

    #[test]
    fn ollama_tags() {
        assert_eq!(Tier::T1E2B.ollama_tag(), "gemma4:e2b");
        assert_eq!(Tier::T2E4B.ollama_tag(), "gemma4:e4b");
        assert_eq!(Tier::T3MoE26B.ollama_tag(), "gemma4:26b");
        assert_eq!(Tier::T4Dense31B.ollama_tag(), "gemma4:31b");
    }

    #[test]
    fn audio_support_by_tier() {
        assert!(Tier::T1E2B.supports_audio());
        assert!(Tier::T2E4B.supports_audio());
        assert!(!Tier::T3MoE26B.supports_audio());
        assert!(!Tier::T4Dense31B.supports_audio());
    }

    #[tokio::test]
    async fn probe_runs_on_current_host() {
        let profile = probe(true).await.expect("probe should succeed");
        assert!(profile.total_ram_mb > 0);
        assert!(profile.cpu_cores > 0);
        // Tier should be valid regardless of hardware.
        assert!(profile.recommended_tier >= Tier::T1E2B);
    }

    /// Manual smoke test — dumps the full probe result for the current host.
    /// Run with:
    ///     cargo test --lib host_probe::tests::dump_current_host_profile -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn dump_current_host_profile() {
        let profile = probe(true).await.expect("probe should succeed");
        let json = serde_json::to_string_pretty(&profile).unwrap();
        println!("\n{json}\n");
        println!("─────────────────────────────────────────────");
        println!(
            "Recommended Ollama model: {}",
            profile.recommended_tier.ollama_tag()
        );
        println!(
            "Approx. download size:    {:.1} GB",
            profile.recommended_tier.download_size_gb()
        );
        println!(
            "Approx. memory required:  {:.1} GB",
            profile.recommended_tier.required_memory_gb()
        );
        println!(
            "Native audio supported:   {}",
            profile.recommended_tier.supports_audio()
        );
        if profile.downgraded {
            println!("Note: tier was downgraded one step (within 20% of boundary).");
        }
        println!("─────────────────────────────────────────────\n");
    }

    #[tokio::test]
    async fn profile_roundtrip() {
        let profile = HardwareProfile {
            os: "macOS".to_string(),
            arch: "aarch64".to_string(),
            total_ram_mb: 16384,
            available_ram_mb: 8192,
            cpu_cores: 8,
            gpu: GpuType::AppleSilicon {
                chip: "Apple M2".to_string(),
            },
            disk_free_mb: 100_000,
            recommended_tier: Tier::T3MoE26B,
            downgraded: false,
            probed_at: "2026-04-16T00:00:00Z".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_profile.json");
        profile.save(&path).await.unwrap();
        let loaded = HardwareProfile::load(&path).await.unwrap();
        assert_eq!(loaded.total_ram_mb, 16384);
        assert_eq!(loaded.recommended_tier, Tier::T3MoE26B);
    }
}
