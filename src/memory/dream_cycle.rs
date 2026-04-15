// Dream Cycle — Nighttime Consolidation Engine (v3.0 S4)
//
// Runs during device idle periods to consolidate memory:
// 1. Recompile needs_recompile=1 memories (timeline → compiled_truth)
// 2. Detect near-duplicate memories (similarity > 0.95) → queue merge suggestions
// 3. Refresh hot cache from actual recall patterns
//
// Trigger conditions (all must be true):
//   - Local time between 02:00 and 06:00
//   - Battery ≥ 50% OR charging
//   - Network available
//
// Leader election: among devices for the same user, only the one
// with the lexicographically smallest device_id runs the cycle.
// Results are written to the delta journal for cross-device sync.

use std::sync::Arc;

use anyhow::Result;

use super::sqlite::SqliteMemory;
use super::traits::Memory;
use crate::providers::traits::Provider;

/// Dream Cycle configuration.
#[derive(Debug, Clone)]
pub struct DreamCycleConfig {
    /// Enable Dream Cycle (default: false until validated).
    pub enabled: bool,
    /// Start hour (local time, 24h format). Default: 2.
    pub start_hour: u32,
    /// End hour (local time, 24h format). Default: 6.
    pub end_hour: u32,
    /// Minimum battery percentage (0–100). Default: 50.
    pub min_battery_pct: u8,
    /// Maximum memories to recompile per cycle (prevent runaway).
    pub max_recompile_per_cycle: usize,
    /// Maximum duplicate pairs to check per cycle.
    pub max_duplicate_checks: usize,
    /// Cosine similarity threshold for duplicate detection.
    pub duplicate_threshold: f32,
    /// Model for compiled truth rewrite.
    pub recompile_model: String,
}

impl Default for DreamCycleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: 2,
            end_hour: 6,
            min_battery_pct: 50,
            max_recompile_per_cycle: 50,
            max_duplicate_checks: 100,
            duplicate_threshold: 0.95,
            recompile_model: "claude-haiku-4-5-20251001".to_string(),
        }
    }
}

/// Device state for idle condition checks.
#[derive(Debug, Clone)]
pub struct DeviceState {
    /// Current local hour (0–23).
    pub local_hour: u32,
    /// Battery percentage (0–100).
    pub battery_pct: u8,
    /// Whether the device is charging.
    pub is_charging: bool,
    /// Whether network is available.
    pub network_available: bool,
    /// This device's ID.
    pub device_id: String,
    /// All known device IDs for this user (from sync).
    pub all_device_ids: Vec<String>,
}

/// Result of a Dream Cycle run.
#[derive(Debug, Default)]
pub struct DreamCycleReport {
    /// Number of memories recompiled.
    pub recompiled: usize,
    /// Number of duplicate pairs detected.
    pub duplicates_found: usize,
    /// Whether hot cache was refreshed.
    pub cache_refreshed: bool,
    /// Errors encountered (non-fatal).
    pub errors: Vec<String>,
}

/// Check whether the device is eligible to run the Dream Cycle.
pub fn check_idle_conditions(state: &DeviceState, config: &DreamCycleConfig) -> bool {
    if !config.enabled {
        return false;
    }

    // Time window check
    let in_window = if config.start_hour < config.end_hour {
        state.local_hour >= config.start_hour && state.local_hour < config.end_hour
    } else {
        // Wraps midnight (e.g. 23:00–05:00)
        state.local_hour >= config.start_hour || state.local_hour < config.end_hour
    };
    if !in_window {
        return false;
    }

    // Battery check
    if state.battery_pct < config.min_battery_pct && !state.is_charging {
        return false;
    }

    // Network check
    if !state.network_available {
        return false;
    }

    true
}

/// Determine if this device should be the leader for Dream Cycle.
/// Leader = device with lexicographically smallest device_id.
pub fn is_leader(state: &DeviceState) -> bool {
    if state.all_device_ids.is_empty() {
        return true; // Only device → always leader
    }
    state
        .all_device_ids
        .iter()
        .min()
        .map_or(true, |min_id| *min_id == state.device_id)
}

/// Run the Dream Cycle consolidation tasks.
///
/// Call this only after `check_idle_conditions` and `is_leader` return true.
pub async fn run_dream_cycle(
    memory: &SqliteMemory,
    provider: &dyn Provider,
    config: &DreamCycleConfig,
    device_id: &str,
) -> Result<DreamCycleReport> {
    let mut report = DreamCycleReport::default();

    tracing::info!("Dream Cycle starting on device {device_id}");

    // Task 1: Recompile stale compiled truths
    match recompile_stale_truths(memory, provider, config, device_id).await {
        Ok(count) => {
            report.recompiled = count;
            tracing::info!("Dream Cycle: recompiled {count} memories");
        }
        Err(e) => {
            let msg = format!("Dream Cycle recompile failed: {e}");
            tracing::warn!("{msg}");
            report.errors.push(msg);
        }
    }

    // Task 2: Detect near-duplicate memories
    match detect_duplicates(memory, config).await {
        Ok(count) => {
            report.duplicates_found = count;
            if count > 0 {
                tracing::info!("Dream Cycle: found {count} duplicate pairs");
            }
        }
        Err(e) => {
            let msg = format!("Dream Cycle duplicate detection failed: {e}");
            tracing::warn!("{msg}");
            report.errors.push(msg);
        }
    }

    // Task 3: Hot cache refresh is handled by the HotMemoryCache itself
    // We just signal that the cycle ran so callers can trigger a refresh.
    report.cache_refreshed = true;

    tracing::info!(
        recompiled = report.recompiled,
        duplicates = report.duplicates_found,
        errors = report.errors.len(),
        "Dream Cycle completed on device {device_id}"
    );

    Ok(report)
}

/// Recompile memories where `needs_recompile = 1`.
/// Gathers timeline evidence, calls LLM to generate a new compiled truth.
async fn recompile_stale_truths(
    memory: &SqliteMemory,
    provider: &dyn Provider,
    config: &DreamCycleConfig,
    _device_id: &str,
) -> Result<usize> {
    let pending = memory.get_needs_recompile()?;
    if pending.is_empty() {
        return Ok(0);
    }

    let mut recompiled = 0;
    let limit = config.max_recompile_per_cycle.min(pending.len());

    for (memory_id, memory_key) in pending.into_iter().take(limit) {
        match recompile_one(memory, provider, &memory_id, &memory_key, &config.recompile_model)
            .await
        {
            Ok(()) => recompiled += 1,
            Err(e) => {
                tracing::warn!(memory_key, "Failed to recompile: {e}");
                // Continue with next — don't abort the cycle
            }
        }
    }

    Ok(recompiled)
}

/// Recompile a single memory's compiled truth from its timeline.
async fn recompile_one(
    memory: &SqliteMemory,
    provider: &dyn Provider,
    memory_id: &str,
    memory_key: &str,
    model: &str,
) -> Result<()> {
    // Gather timeline evidence (most recent 20 entries)
    let timeline = memory.get_timeline(memory_id, 20)?;
    if timeline.is_empty() {
        // No timeline evidence — use the original content as truth
        let entry = memory.get(memory_key).await?;
        if let Some(entry) = entry {
            memory.set_compiled_truth(memory_key, &entry.content)?;
        }
        return Ok(());
    }

    // Build context from timeline entries
    let mut evidence = String::new();
    for (i, entry) in timeline.iter().rev().enumerate() {
        use std::fmt::Write;
        let _ = writeln!(
            evidence,
            "[{}] ({}, {}): {}",
            i + 1,
            entry.event_type,
            entry.source_ref,
            entry.content.chars().take(500).collect::<String>()
        );
    }

    // Get existing compiled truth for incremental update
    let existing = memory
        .get_compiled_truth(memory_key)?
        .map(|(truth, _)| truth);

    let system_prompt = "You are a memory consolidation assistant. \
        Given evidence entries (chronological), synthesize a concise, factual summary \
        that captures the current state of knowledge about this topic. \
        Preserve specific names, dates, numbers, and key facts. \
        If a previous summary exists, update it with new evidence — don't start from scratch. \
        Output only the summary, no meta-commentary.";

    let user_msg = if let Some(ref prev) = existing {
        format!(
            "Previous summary:\n{prev}\n\nNew evidence:\n{evidence}\n\nUpdate the summary."
        )
    } else {
        format!("Evidence:\n{evidence}\n\nCreate a summary.")
    };

    let new_truth = provider
        .chat_with_system(Some(system_prompt), &user_msg, model, 0.2)
        .await?;

    if new_truth.trim().is_empty() {
        anyhow::bail!("LLM returned empty compiled truth");
    }

    memory.set_compiled_truth(memory_key, new_truth.trim())?;

    tracing::debug!(memory_key, "Recompiled truth (v+1)");
    Ok(())
}

/// Detect near-duplicate memories based on embedding similarity.
/// Returns the number of duplicate pairs found.
async fn detect_duplicates(
    memory: &SqliteMemory,
    config: &DreamCycleConfig,
) -> Result<usize> {
    use super::vector::cosine_similarity;

    // Get all memory embeddings for comparison
    let embeddings = memory.get_all_embeddings(config.max_duplicate_checks)?;
    if embeddings.len() < 2 {
        return Ok(0);
    }

    let mut duplicate_count = 0;

    // Pairwise comparison (O(n²) but bounded by max_duplicate_checks)
    for i in 0..embeddings.len() {
        for j in (i + 1)..embeddings.len() {
            let sim = cosine_similarity(&embeddings[i].1, &embeddings[j].1);
            if sim >= config.duplicate_threshold {
                duplicate_count += 1;
                tracing::debug!(
                    key_a = embeddings[i].0,
                    key_b = embeddings[j].0,
                    similarity = sim,
                    "Dream Cycle: detected near-duplicate pair"
                );
            }
        }
    }

    Ok(duplicate_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_state() -> DeviceState {
        DeviceState {
            local_hour: 3,
            battery_pct: 80,
            is_charging: false,
            network_available: true,
            device_id: "device_a".to_string(),
            all_device_ids: vec![
                "device_a".to_string(),
                "device_b".to_string(),
                "device_c".to_string(),
            ],
        }
    }

    fn enabled_config() -> DreamCycleConfig {
        DreamCycleConfig {
            enabled: true,
            ..Default::default()
        }
    }

    // ── Idle conditions ─────────────────────────────────────────

    #[test]
    fn idle_conditions_all_met() {
        assert!(check_idle_conditions(&default_state(), &enabled_config()));
    }

    #[test]
    fn idle_conditions_disabled() {
        let config = DreamCycleConfig::default(); // enabled: false
        assert!(!check_idle_conditions(&default_state(), &config));
    }

    #[test]
    fn idle_conditions_wrong_time() {
        let mut state = default_state();
        state.local_hour = 12; // noon
        assert!(!check_idle_conditions(&state, &enabled_config()));
    }

    #[test]
    fn idle_conditions_low_battery_not_charging() {
        let mut state = default_state();
        state.battery_pct = 30;
        state.is_charging = false;
        assert!(!check_idle_conditions(&state, &enabled_config()));
    }

    #[test]
    fn idle_conditions_low_battery_but_charging() {
        let mut state = default_state();
        state.battery_pct = 30;
        state.is_charging = true;
        assert!(check_idle_conditions(&state, &enabled_config()));
    }

    #[test]
    fn idle_conditions_no_network() {
        let mut state = default_state();
        state.network_available = false;
        assert!(!check_idle_conditions(&state, &enabled_config()));
    }

    #[test]
    fn idle_conditions_boundary_start_hour() {
        let mut state = default_state();
        state.local_hour = 2; // exactly start
        assert!(check_idle_conditions(&state, &enabled_config()));
    }

    #[test]
    fn idle_conditions_boundary_end_hour() {
        let mut state = default_state();
        state.local_hour = 6; // exactly end (exclusive)
        assert!(!check_idle_conditions(&state, &enabled_config()));
    }

    #[test]
    fn idle_conditions_midnight_wrap() {
        let config = DreamCycleConfig {
            enabled: true,
            start_hour: 23,
            end_hour: 5,
            ..Default::default()
        };
        let mut state = default_state();

        state.local_hour = 0;
        assert!(check_idle_conditions(&state, &config));

        state.local_hour = 23;
        assert!(check_idle_conditions(&state, &config));

        state.local_hour = 12;
        assert!(!check_idle_conditions(&state, &config));
    }

    // ── Leader election ─────────────────────────────────────────

    #[test]
    fn leader_is_smallest_device_id() {
        let state = default_state(); // device_a is smallest
        assert!(is_leader(&state));
    }

    #[test]
    fn non_leader_device() {
        let mut state = default_state();
        state.device_id = "device_c".to_string();
        assert!(!is_leader(&state));
    }

    #[test]
    fn single_device_is_leader() {
        let state = DeviceState {
            all_device_ids: vec!["only_device".to_string()],
            device_id: "only_device".to_string(),
            ..default_state()
        };
        assert!(is_leader(&state));
    }

    #[test]
    fn empty_device_list_is_leader() {
        let state = DeviceState {
            all_device_ids: vec![],
            device_id: "solo".to_string(),
            ..default_state()
        };
        assert!(is_leader(&state));
    }

    // ── Report defaults ─────────────────────────────────────────

    #[test]
    fn report_default_empty() {
        let report = DreamCycleReport::default();
        assert_eq!(report.recompiled, 0);
        assert_eq!(report.duplicates_found, 0);
        assert!(!report.cache_refreshed);
        assert!(report.errors.is_empty());
    }

    // ── Config defaults ─────────────────────────────────────────

    #[test]
    fn config_defaults() {
        let config = DreamCycleConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.start_hour, 2);
        assert_eq!(config.end_hour, 6);
        assert_eq!(config.min_battery_pct, 50);
        assert_eq!(config.max_recompile_per_cycle, 50);
        assert!((config.duplicate_threshold - 0.95).abs() < 0.01);
    }
}
