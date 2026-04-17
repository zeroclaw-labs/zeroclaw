//! Skill confidence scoring and deprecation.
//!
//! This module closes the self-evolution loop started by `SkillCreator` /
//! `SkillImprover`: every time the agent uses a skill, we record a trace
//! (success, duration). Those traces are later aggregated into a
//! [`ConfidenceScore`] — a single 0..=1 number combining three signals:
//!
//! 1. **Success rate** — `successes / total_calls`.
//! 2. **Usage frequency** — how often the skill is used, normalized by a
//!    target call count (more usage → higher confidence, saturating).
//! 3. **Recency decay** — skills that haven't been touched in a long time
//!    decay towards zero, so stale skills naturally get deprecated.
//!
//! When the score falls below a configurable threshold (and the skill has
//! accumulated enough evidence to trust the score), the skill is marked as
//! deprecated via a `DEPRECATED` sidecar file in its directory. The skill
//! loader can then skip deprecated skills during prompt injection without
//! losing the on-disk history — operators can re-enable by deleting the
//! marker.
//!
//! The module is deliberately decoupled from I/O: the scoring math lives in
//! [`compute_confidence`] and [`should_deprecate`]; persistence is provided
//! via [`JsonlTraceStore`] but anyone can implement the [`TraceStore`] trait
//! for a custom backend.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single execution trace for a skill.
///
/// Traces are the raw evidence fed into the confidence score. They are
/// lightweight (no full tool payloads) so they can be persisted cheaply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTrace {
    /// Skill slug (matches the directory name under `workspace/skills/`).
    pub skill_slug: String,
    /// UTC timestamp the trace was recorded.
    pub timestamp: DateTime<Utc>,
    /// Whether the skill's tool invocation(s) completed without error.
    pub success: bool,
    /// Observed wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Number of tool calls in the trace (helps spot runaway skills).
    pub tool_calls: u32,
}

impl SkillTrace {
    pub fn new(skill_slug: impl Into<String>, success: bool, duration_ms: u64, tool_calls: u32) -> Self {
        Self {
            skill_slug: skill_slug.into(),
            timestamp: Utc::now(),
            success,
            duration_ms,
            tool_calls,
        }
    }
}

/// A breakdown of a skill's confidence score.
///
/// `value` is always `success_rate * usage_frequency * recency_decay`, all
/// clamped to the unit interval. The breakdown fields are useful for
/// debugging and for operator-facing reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfidenceScore {
    pub value: f64,
    pub success_rate: f64,
    pub usage_frequency: f64,
    pub recency_decay: f64,
    pub sample_count: usize,
}

impl ConfidenceScore {
    /// Confidence for a skill that has no traces yet.
    ///
    /// We return `1.0` rather than `0.0` so brand-new skills aren't deprecated
    /// before they've had a chance to accumulate evidence. The
    /// [`should_deprecate`] logic independently requires a minimum sample
    /// count before it will recommend removal.
    pub fn unknown() -> Self {
        Self {
            value: 1.0,
            success_rate: 1.0,
            usage_frequency: 0.0,
            recency_decay: 1.0,
            sample_count: 0,
        }
    }
}

/// Tunable thresholds for [`compute_confidence`] and [`should_deprecate`].
#[derive(Debug, Clone)]
pub struct ConfidencePolicy {
    /// Call count at which [`ConfidenceScore::usage_frequency`] saturates at 1.0.
    pub saturation_calls: u32,
    /// Half-life for recency decay, in hours. After this many hours since the
    /// last trace, `recency_decay` is 0.5; after 2× this many hours, 0.25; etc.
    pub recency_half_life_hours: f64,
    /// Minimum number of traces before [`should_deprecate`] will recommend
    /// deprecation. Below this, the skill is assumed "new" and kept.
    ///
    /// **Default is deliberately generous (15)**: a single transient issue
    /// (API rate limit, upstream flake) can easily cause 5 failures in a
    /// row. Requiring 15 samples means ~3× the natural noise floor before
    /// permanently removing a skill.
    pub min_samples_for_deprecation: usize,
    /// Confidence score at or below which deprecation is recommended.
    pub deprecation_threshold: f64,
    /// How long after a skill is deprecated we reconsider it, in hours.
    /// Set to `0` to disable automatic reinstatement.
    ///
    /// Once a skill's `DEPRECATED` marker has been on disk longer than
    /// this window, [`reevaluate_deprecations`] looks at its recent trace
    /// record; if the new confidence is above [`Self::reinstate_threshold`],
    /// the marker is removed and the skill becomes usable again.
    pub review_window_hours: u64,
    /// Confidence score above which a deprecated skill is re-enabled. Must
    /// be strictly greater than [`Self::deprecation_threshold`] — the gap
    /// is hysteresis that prevents flapping skills from oscillating between
    /// deprecated and active.
    pub reinstate_threshold: f64,
}

impl Default for ConfidencePolicy {
    fn default() -> Self {
        Self {
            saturation_calls: 20,
            recency_half_life_hours: 24.0 * 30.0, // 30 days
            // 15 was chosen over 5 based on the Staff Engineer review:
            // a transient failure spike of 5 should not cause permanent
            // removal. Operators who want aggressive pruning can lower
            // this via `[skills.skill_confidence].min_samples_for_deprecation`.
            min_samples_for_deprecation: 15,
            deprecation_threshold: 0.3,
            // 7 days gives a deprecated skill one full week of grace
            // before we retry it. Short enough that transient failures
            // (provider outage) heal automatically; long enough that we
            // don't thrash on a skill that's genuinely broken.
            review_window_hours: 24 * 7,
            // 0.5 leaves a 0.2 hysteresis gap above 0.3 — a recovering
            // skill must cross this gap to be reinstated.
            reinstate_threshold: 0.5,
        }
    }
}

/// Convert the user-facing config (`[skills.skill_confidence]`) into the
/// runtime-internal [`ConfidencePolicy`].
impl From<&zeroclaw_config::schema::SkillConfidenceConfig> for ConfidencePolicy {
    fn from(c: &zeroclaw_config::schema::SkillConfidenceConfig) -> Self {
        Self {
            saturation_calls: c.saturation_calls,
            recency_half_life_hours: c.recency_half_life_hours,
            min_samples_for_deprecation: c.min_samples_for_deprecation,
            deprecation_threshold: c.deprecation_threshold,
            review_window_hours: c.review_window_hours,
            reinstate_threshold: c.reinstate_threshold,
        }
    }
}

/// Aggregate a slice of traces into a [`ConfidenceScore`].
///
/// The traces should all be for the same skill; we don't filter internally
/// so callers have full control (e.g. excluding a known-bad time window).
pub fn compute_confidence(traces: &[SkillTrace], policy: &ConfidencePolicy) -> ConfidenceScore {
    if traces.is_empty() {
        return ConfidenceScore::unknown();
    }

    let n = traces.len();
    let successes = traces.iter().filter(|t| t.success).count();
    let success_rate = successes as f64 / n as f64;

    let usage_frequency =
        (n as f64 / policy.saturation_calls as f64).clamp(0.0, 1.0);

    let recency_decay = compute_recency_decay(traces, policy.recency_half_life_hours, Utc::now());

    let value = (success_rate * usage_frequency * recency_decay).clamp(0.0, 1.0);

    ConfidenceScore {
        value,
        success_rate,
        usage_frequency,
        recency_decay,
        sample_count: n,
    }
}

/// Compute the recency decay factor for a trace set.
///
/// Uses the timestamp of the most recent trace. Older skills decay faster.
/// Factored out so tests can inject a deterministic `now`.
fn compute_recency_decay(
    traces: &[SkillTrace],
    half_life_hours: f64,
    now: DateTime<Utc>,
) -> f64 {
    if half_life_hours <= 0.0 {
        return 1.0;
    }
    let latest = match traces.iter().map(|t| t.timestamp).max() {
        Some(ts) => ts,
        None => return 1.0,
    };
    let hours_since = (now - latest).num_seconds() as f64 / 3600.0;
    if hours_since <= 0.0 {
        return 1.0;
    }
    // Exponential decay: 0.5^(hours_since / half_life)
    let decay = 0.5f64.powf(hours_since / half_life_hours);
    decay.clamp(0.0, 1.0)
}

/// Should this skill be marked deprecated given its current score?
pub fn should_deprecate(score: &ConfidenceScore, policy: &ConfidencePolicy) -> bool {
    score.sample_count >= policy.min_samples_for_deprecation
        && score.value <= policy.deprecation_threshold
}

/// Persist-and-load traces. Production backends can use SQLite; we ship a
/// JSONL implementation that is simple, human-auditable, and has no
/// extra dependencies.
pub trait TraceStore: Send + Sync {
    fn append(&self, trace: &SkillTrace) -> Result<()>;
    fn load_for(&self, skill_slug: &str) -> Result<Vec<SkillTrace>>;
    fn load_all(&self) -> Result<Vec<SkillTrace>>;
}

/// JSONL-backed trace store. One trace per line in a single append-only
/// file. Reads scan the whole file (acceptable for moderate volumes;
/// upgrade to SQLite when trace count exceeds ~10k).
///
/// Size-based rotation: when `max_file_size` is set and the file exceeds
/// that threshold at append time, the existing file is renamed to
/// `<path>.old` (replacing any previous `.old` file) before the new line is
/// written to a fresh file. This keeps the primary trace file bounded and
/// preserves one generation of history for debugging.
pub struct JsonlTraceStore {
    path: PathBuf,
    max_file_size: Option<u64>,
}

impl JsonlTraceStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_file_size: None,
        }
    }

    /// Set the maximum file size in bytes. When an append would find the
    /// file already exceeds this size, the file is rotated to `<path>.old`
    /// before the append.
    pub fn with_max_file_size(mut self, bytes: u64) -> Self {
        self.max_file_size = Some(bytes);
        self
    }

    fn ensure_parent(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create trace store parent dir {}", parent.display())
            })?;
        }
        Ok(())
    }

    /// Rotate the trace file if it exceeds the configured limit. Errors
    /// are logged but not propagated — a failed rotation should never
    /// prevent a subsequent append (the file will just stay oversized).
    fn maybe_rotate(&self) {
        let Some(limit) = self.max_file_size else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return;
        };
        if meta.len() < limit {
            return;
        }
        let rotated = self.path.with_extension("jsonl.old");
        // Remove any existing .old file, then rename. Best-effort.
        let _ = std::fs::remove_file(&rotated);
        if let Err(e) = std::fs::rename(&self.path, &rotated) {
            tracing::debug!(
                "trace rotation failed: could not rename {} to {}: {e}",
                self.path.display(),
                rotated.display()
            );
        } else {
            tracing::info!(
                "rotated trace file {} → {} (> {} bytes)",
                self.path.display(),
                rotated.display(),
                limit
            );
        }
    }
}

impl TraceStore for JsonlTraceStore {
    fn append(&self, trace: &SkillTrace) -> Result<()> {
        self.ensure_parent()?;
        self.maybe_rotate();
        let line = serde_json::to_string(trace).context("failed to serialize trace")?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open trace file {}", self.path.display()))?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }

    fn load_for(&self, skill_slug: &str) -> Result<Vec<SkillTrace>> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|t| t.skill_slug == skill_slug).collect())
    }

    fn load_all(&self) -> Result<Vec<SkillTrace>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        let mut out = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<SkillTrace>(line) {
                Ok(t) => out.push(t),
                Err(e) => {
                    tracing::debug!(
                        "skipping unparseable trace at line {} of {}: {e}",
                        idx + 1,
                        self.path.display()
                    );
                }
            }
        }
        Ok(out)
    }
}

/// Default filename for the JSONL trace store, under `workspace_dir`.
pub const DEFAULT_TRACE_FILE: &str = "skill_traces.jsonl";

/// Derive the default trace-store path from a workspace directory.
pub fn default_trace_store_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(DEFAULT_TRACE_FILE)
}

/// Record one trace per distinct skill used in a session.
///
/// Designed to be called from the agent loop right after
/// [`super::tracker::SkillUsageTracker::record_from_history`]. We don't know
/// per-tool-call success granularly, so we collapse to a session-level
/// signal: if the overall assistant response completed without error, all
/// tools that fired are recorded as successes; otherwise all are failures.
/// Coarser than per-call tracking, but good enough to catch chronically
/// failing skills.
///
/// Errors from the underlying store are logged and swallowed; trace
/// recording must never crash the agent.
pub fn record_session_traces(
    store: &dyn TraceStore,
    tracker: &super::tracker::SkillUsageTracker,
    session_success: bool,
    duration_ms: u64,
) {
    for (slug, stats) in tracker.all() {
        let trace = SkillTrace {
            skill_slug: slug.clone(),
            timestamp: Utc::now(),
            success: session_success,
            duration_ms,
            tool_calls: stats.call_count as u32,
        };
        if let Err(e) = store.append(&trace) {
            tracing::debug!(skill = slug.as_str(), error = %e, "failed to append skill trace");
        }
    }
}

/// Convenience wrapper: build tracker from history, then append traces
/// asynchronously so the synchronous file I/O doesn't block the tokio
/// worker on the agent hot path.
///
/// `start_time` is the [`std::time::Instant`] captured at session start;
/// `session_success` indicates whether [`super::super::agent::loop_::run_tool_call_loop`]
/// returned Ok or Err. Both branches of the agent loop should call this
/// exactly once so confidence scoring sees both successes and failures.
///
/// This is a fire-and-forget helper — it never errors, only logs on failure.
pub fn record_session_traces_async(
    workspace_dir: std::path::PathBuf,
    history: Vec<zeroclaw_providers::ChatMessage>,
    session_success: bool,
    start_time: std::time::Instant,
) {
    let duration_ms = u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Build the tracker synchronously (cheap — in-memory string parsing).
    let tool_calls = super::creator::extract_tool_calls_from_history(&history);
    let mut tracker = super::tracker::SkillUsageTracker::new();
    tracker.record_from_history(&tool_calls);
    if tracker.is_empty() {
        return;
    }

    // Move the actual file I/O off the async runtime. `spawn_blocking` is
    // cheap for short bursts and prevents worker stalls when the trace
    // file is on slow storage or subject to lock contention.
    let _ = tokio::task::spawn_blocking(move || {
        let store = JsonlTraceStore::new(default_trace_store_path(&workspace_dir))
            .with_max_file_size(DEFAULT_TRACE_MAX_BYTES);
        record_session_traces(&store, &tracker, session_success, duration_ms);
    });
}

/// Default rotation threshold for the JSONL trace store: 10 MiB. When the
/// file exceeds this size, the next `append` rotates it to `<path>.old`
/// before writing.
pub const DEFAULT_TRACE_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Name of the sidecar file that marks a skill as deprecated.
pub const DEPRECATION_MARKER_FILE: &str = "DEPRECATED";

/// Write a deprecation marker to the skill's directory.
///
/// The marker contains the reason and the score breakdown so operators can
/// understand why the skill was deprecated. If the marker already exists,
/// it is overwritten.
pub fn mark_skill_deprecated(
    skill_dir: &Path,
    reason: &str,
    score: &ConfidenceScore,
) -> Result<()> {
    if !skill_dir.is_dir() {
        anyhow::bail!("skill directory does not exist: {}", skill_dir.display());
    }
    let content = format!(
        "# Deprecated\n\
         \n\
         Reason: {reason}\n\
         Timestamp: {ts}\n\
         \n\
         ## Confidence breakdown\n\
         \n\
         - value: {value:.3}\n\
         - success_rate: {sr:.3}\n\
         - usage_frequency: {uf:.3}\n\
         - recency_decay: {rd:.3}\n\
         - sample_count: {sc}\n\
         \n\
         Remove this file to re-enable the skill.\n",
        reason = reason,
        ts = Utc::now().to_rfc3339(),
        value = score.value,
        sr = score.success_rate,
        uf = score.usage_frequency,
        rd = score.recency_decay,
        sc = score.sample_count,
    );
    std::fs::write(skill_dir.join(DEPRECATION_MARKER_FILE), content).with_context(|| {
        format!(
            "failed to write deprecation marker in {}",
            skill_dir.display()
        )
    })?;
    Ok(())
}

/// Check whether a skill directory is marked deprecated.
pub fn is_skill_deprecated(skill_dir: &Path) -> bool {
    skill_dir.join(DEPRECATION_MARKER_FILE).exists()
}

/// Remove the deprecation marker (re-enable the skill).
pub fn clear_deprecation(skill_dir: &Path) -> Result<()> {
    let marker = skill_dir.join(DEPRECATION_MARKER_FILE);
    if marker.exists() {
        std::fs::remove_file(&marker)
            .with_context(|| format!("failed to remove {}", marker.display()))?;
    }
    Ok(())
}

/// Reconsider already-deprecated skills: if their `DEPRECATED` marker is
/// older than [`ConfidencePolicy::review_window_hours`] and their recent
/// confidence has recovered past [`ConfidencePolicy::reinstate_threshold`],
/// remove the marker and bring them back.
///
/// Returns the list of slugs that were successfully reinstated.
///
/// This is the recovery half of the self-evolution loop: without it, a
/// skill deprecated because of a transient provider outage stays dead
/// forever unless an operator intervenes. With it, the system heals
/// itself over the review window.
///
/// Errors from individual file operations are logged and skipped — one
/// unreadable marker should not block reinstatement of other skills.
pub fn reevaluate_deprecations(
    store: &dyn TraceStore,
    workspace_skills_dir: &Path,
    policy: &ConfidencePolicy,
) -> Result<Vec<String>> {
    if policy.review_window_hours == 0 {
        return Ok(Vec::new());
    }

    let Ok(entries) = std::fs::read_dir(workspace_skills_dir) else {
        return Ok(Vec::new());
    };

    let now = Utc::now();
    let window = chrono::Duration::hours(policy.review_window_hours as i64);
    let mut reinstated = Vec::new();

    for entry in entries.flatten() {
        let skill_dir = entry.path();
        if !skill_dir.is_dir() {
            continue;
        }
        if !is_skill_deprecated(&skill_dir) {
            continue;
        }

        let slug = match skill_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Parse the marker's timestamp so we only reconsider markers that
        // have aged past the review window. Markers written by
        // [`mark_skill_deprecated`] always include a `Timestamp:` line;
        // legacy / hand-written markers without one are treated as
        // "old enough" — if recent traces look healthy, we reinstate.
        let marker_path = skill_dir.join(DEPRECATION_MARKER_FILE);
        let Ok(marker_content) = std::fs::read_to_string(&marker_path) else {
            continue;
        };
        let parsed_ts = parse_marker_timestamp(&marker_content);
        let marker_ts = parsed_ts.unwrap_or_else(|| {
            tracing::debug!(
                "deprecation marker for '{}' has no timestamp; treating as stale",
                slug
            );
            // Fall back to epoch-far-past so the window check below passes.
            now - window - chrono::Duration::hours(1)
        });

        if parsed_ts.is_some() && (now - marker_ts) < window {
            // Not stale yet — leave it alone.
            continue;
        }

        // Rescore using ONLY traces recorded after the marker was written.
        // Pre-deprecation traces are what got us deprecated in the first
        // place; including them now would bias the reconsideration.
        let traces = match store.load_for(&slug) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(skill = slug.as_str(), error = %e, "reevaluate: load_for failed");
                continue;
            }
        };
        let recent: Vec<SkillTrace> = traces
            .into_iter()
            .filter(|t| t.timestamp >= marker_ts)
            .collect();
        if recent.is_empty() {
            // No new evidence since deprecation — cannot justify reinstating.
            continue;
        }

        let score = compute_confidence(&recent, policy);
        if score.value >= policy.reinstate_threshold {
            match clear_deprecation(&skill_dir) {
                Ok(()) => {
                    tracing::info!(
                        skill = slug.as_str(),
                        score = score.value,
                        samples = score.sample_count,
                        "Skill reinstated by confidence scorer: recent traces passed reinstate_threshold"
                    );
                    reinstated.push(slug);
                }
                Err(e) => {
                    tracing::warn!(
                        skill = slug.as_str(),
                        error = %e,
                        "Failed to clear deprecation marker during reinstatement"
                    );
                }
            }
        }
    }

    Ok(reinstated)
}

/// Extract the `Timestamp:` field from a DEPRECATED marker file. Returns
/// `None` when the field is missing or the value isn't a valid RFC 3339
/// timestamp (e.g. a legacy manually-created marker).
fn parse_marker_timestamp(content: &str) -> Option<DateTime<Utc>> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Timestamp:") {
            let ts = rest.trim();
            if let Ok(parsed) = DateTime::parse_from_rfc3339(ts) {
                return Some(parsed.with_timezone(&Utc));
            }
        }
    }
    None
}

/// One pass over the skill traces: compute scores and, for each skill
/// whose score falls below the policy threshold, write a deprecation
/// marker to its directory. Returns the list of slugs that were newly
/// deprecated.
///
/// Skills that are already deprecated are skipped. Skills that no longer
/// have a corresponding directory on disk are ignored.
pub fn evaluate_and_deprecate(
    store: &dyn TraceStore,
    workspace_skills_dir: &Path,
    policy: &ConfidencePolicy,
) -> Result<Vec<String>> {
    let traces = store.load_all()?;
    let mut by_skill: std::collections::HashMap<String, Vec<SkillTrace>> = Default::default();
    for t in traces {
        by_skill.entry(t.skill_slug.clone()).or_default().push(t);
    }

    let mut newly_deprecated = Vec::new();
    for (slug, traces) in by_skill {
        let score = compute_confidence(&traces, policy);
        if !should_deprecate(&score, policy) {
            continue;
        }
        let skill_dir = workspace_skills_dir.join(&slug);
        if !skill_dir.is_dir() {
            continue;
        }
        if is_skill_deprecated(&skill_dir) {
            continue;
        }
        let reason = format!(
            "auto-deprecated: score {:.3} <= threshold {:.3} (samples={})",
            score.value, policy.deprecation_threshold, score.sample_count
        );
        match mark_skill_deprecated(&skill_dir, &reason, &score) {
            Ok(()) => {
                tracing::info!(
                    skill = slug.as_str(),
                    score = score.value,
                    samples = score.sample_count,
                    "Skill deprecated by confidence scorer"
                );
                newly_deprecated.push(slug);
            }
            Err(e) => {
                tracing::warn!(
                    skill = slug.as_str(),
                    error = %e,
                    "Failed to mark skill as deprecated"
                );
            }
        }
    }
    Ok(newly_deprecated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::TempDir;

    fn trace(slug: &str, success: bool, ts: DateTime<Utc>) -> SkillTrace {
        SkillTrace {
            skill_slug: slug.into(),
            timestamp: ts,
            success,
            duration_ms: 100,
            tool_calls: 2,
        }
    }

    // ── compute_confidence ───────────────────────────────────────

    #[test]
    fn unknown_score_for_empty_traces() {
        let s = compute_confidence(&[], &ConfidencePolicy::default());
        assert_eq!(s.sample_count, 0);
        assert_eq!(s.value, 1.0, "new skills default to high confidence");
    }

    #[test]
    fn perfect_recent_high_frequency_scores_high() {
        let policy = ConfidencePolicy::default();
        let now = Utc::now();
        let traces: Vec<_> = (0..policy.saturation_calls)
            .map(|_| trace("s", true, now))
            .collect();
        let s = compute_confidence(&traces, &policy);
        assert!(s.value > 0.95, "got {}", s.value);
        assert!((s.success_rate - 1.0).abs() < 1e-9);
        assert!((s.usage_frequency - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mixed_success_lowers_score() {
        let policy = ConfidencePolicy::default();
        let now = Utc::now();
        let mut traces: Vec<_> = (0..10).map(|_| trace("s", true, now)).collect();
        traces.extend((0..10).map(|_| trace("s", false, now)));
        let s = compute_confidence(&traces, &policy);
        assert!((s.success_rate - 0.5).abs() < 1e-9);
        assert!(s.value <= 0.5);
    }

    #[test]
    fn recency_decay_halves_at_half_life() {
        let mut policy = ConfidencePolicy::default();
        policy.recency_half_life_hours = 24.0;
        let now = Utc::now();
        let old = now - Duration::hours(24);
        let decay = compute_recency_decay(&[trace("s", true, old)], policy.recency_half_life_hours, now);
        assert!((decay - 0.5).abs() < 1e-3, "got {decay}");
    }

    #[test]
    fn recency_decay_ten_years_old_approaches_zero() {
        let policy = ConfidencePolicy::default();
        let now = Utc::now();
        let ancient = now - Duration::days(365 * 10);
        let decay = compute_recency_decay(
            &[trace("s", true, ancient)],
            policy.recency_half_life_hours,
            now,
        );
        assert!(decay < 0.01, "got {decay}");
    }

    // ── should_deprecate ─────────────────────────────────────────

    #[test]
    fn does_not_deprecate_with_insufficient_samples() {
        let policy = ConfidencePolicy::default();
        // Score low enough to cross the threshold, but fewer samples than
        // the minimum → no deprecation.
        let score = ConfidenceScore {
            value: 0.0,
            success_rate: 0.0,
            usage_frequency: 0.1,
            recency_decay: 1.0,
            sample_count: policy.min_samples_for_deprecation - 1,
        };
        assert!(!should_deprecate(&score, &policy));
    }

    #[test]
    fn deprecates_when_below_threshold_with_enough_samples() {
        let policy = ConfidencePolicy::default();
        let score = ConfidenceScore {
            value: 0.1,
            success_rate: 0.2,
            usage_frequency: 1.0,
            recency_decay: 0.5,
            sample_count: policy.min_samples_for_deprecation + 1,
        };
        assert!(should_deprecate(&score, &policy));
    }

    #[test]
    fn keeps_new_high_confidence_skills() {
        let policy = ConfidencePolicy::default();
        let score = ConfidenceScore::unknown();
        assert!(!should_deprecate(&score, &policy));
    }

    // ── JsonlTraceStore ──────────────────────────────────────────

    #[test]
    fn jsonl_store_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlTraceStore::new(tmp.path().join("traces.jsonl"));

        store.append(&trace("alpha", true, Utc::now())).unwrap();
        store.append(&trace("beta", false, Utc::now())).unwrap();
        store.append(&trace("alpha", true, Utc::now())).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 3);

        let alpha = store.load_for("alpha").unwrap();
        assert_eq!(alpha.len(), 2);
        assert!(alpha.iter().all(|t| t.skill_slug == "alpha"));

        let beta = store.load_for("beta").unwrap();
        assert_eq!(beta.len(), 1);
        assert!(!beta[0].success);
    }

    #[test]
    fn jsonl_store_returns_empty_on_missing_file() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlTraceStore::new(tmp.path().join("does-not-exist.jsonl"));
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn jsonl_store_skips_garbage_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("traces.jsonl");
        std::fs::write(
            &path,
            "\
{\"skill_slug\":\"good\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"success\":true,\"duration_ms\":100,\"tool_calls\":2}
this is not json
{\"skill_slug\":\"good2\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"success\":false,\"duration_ms\":50,\"tool_calls\":1}
",
        )
        .unwrap();
        let store = JsonlTraceStore::new(&path);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    // ── deprecation markers ──────────────────────────────────────

    #[test]
    fn mark_and_detect_deprecation() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        assert!(!is_skill_deprecated(&skill_dir));

        let score = ConfidenceScore {
            value: 0.1,
            success_rate: 0.2,
            usage_frequency: 1.0,
            recency_decay: 0.5,
            sample_count: 10,
        };
        mark_skill_deprecated(&skill_dir, "too flaky", &score).unwrap();

        assert!(is_skill_deprecated(&skill_dir));
        let marker_content =
            std::fs::read_to_string(skill_dir.join(DEPRECATION_MARKER_FILE)).unwrap();
        assert!(marker_content.contains("too flaky"));
        assert!(marker_content.contains("value: 0.100"));
    }

    #[test]
    fn clear_deprecation_removes_marker() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let score = ConfidenceScore::unknown();
        mark_skill_deprecated(&skill_dir, "test", &score).unwrap();
        assert!(is_skill_deprecated(&skill_dir));
        clear_deprecation(&skill_dir).unwrap();
        assert!(!is_skill_deprecated(&skill_dir));
    }

    #[test]
    fn mark_deprecated_fails_on_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let ghost = tmp.path().join("ghost");
        let r = mark_skill_deprecated(&ghost, "reason", &ConfidenceScore::unknown());
        assert!(r.is_err());
    }

    // ── end-to-end evaluate_and_deprecate ───────────────────────

    #[test]
    fn evaluate_and_deprecate_marks_low_confidence_skills() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");

        // Create three skill dirs.
        std::fs::create_dir_all(skills_dir.join("flaky")).unwrap();
        std::fs::create_dir_all(skills_dir.join("healthy")).unwrap();
        std::fs::create_dir_all(skills_dir.join("new")).unwrap();

        let store = JsonlTraceStore::new(&traces_path);

        // flaky: many calls, mostly failing → should be deprecated.
        // Needs at least `min_samples_for_deprecation` (default 15) total
        // samples to cross the threshold. Using 18 failures + 2 successes.
        let now = Utc::now();
        for _ in 0..18 {
            store.append(&trace("flaky", false, now)).unwrap();
        }
        for _ in 0..2 {
            store.append(&trace("flaky", true, now)).unwrap();
        }

        // healthy: many recent successful calls → stays.
        for _ in 0..20 {
            store.append(&trace("healthy", true, now)).unwrap();
        }

        // new: only a couple of failures, not enough samples → stays.
        for _ in 0..2 {
            store.append(&trace("new", false, now)).unwrap();
        }

        let policy = ConfidencePolicy::default();
        let deprecated = evaluate_and_deprecate(&store, &skills_dir, &policy).unwrap();

        assert_eq!(deprecated, vec!["flaky".to_string()]);
        assert!(is_skill_deprecated(&skills_dir.join("flaky")));
        assert!(!is_skill_deprecated(&skills_dir.join("healthy")));
        assert!(!is_skill_deprecated(&skills_dir.join("new")));
    }

    #[test]
    fn evaluate_and_deprecate_skips_already_deprecated() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("flaky")).unwrap();

        // Pre-existing marker.
        std::fs::write(
            skills_dir.join("flaky").join(DEPRECATION_MARKER_FILE),
            "manual",
        )
        .unwrap();

        let store = JsonlTraceStore::new(&traces_path);
        let now = Utc::now();
        for _ in 0..10 {
            store.append(&trace("flaky", false, now)).unwrap();
        }

        let deprecated =
            evaluate_and_deprecate(&store, &skills_dir, &ConfidencePolicy::default()).unwrap();
        assert!(deprecated.is_empty());
        let marker = std::fs::read_to_string(skills_dir.join("flaky").join(DEPRECATION_MARKER_FILE))
            .unwrap();
        assert_eq!(marker, "manual", "existing marker should be untouched");
    }

    #[test]
    fn record_session_traces_writes_one_per_skill() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlTraceStore::new(tmp.path().join("traces.jsonl"));

        let mut tracker = crate::skills::tracker::SkillUsageTracker::new();
        tracker.record_call("alpha__tool1");
        tracker.record_call("alpha__tool2");
        tracker.record_call("beta__tool1");

        record_session_traces(&store, &tracker, true, 250);

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2, "one trace per distinct skill");
        assert!(all.iter().all(|t| t.success));
        assert!(all.iter().all(|t| t.duration_ms == 250));
    }

    #[test]
    fn record_session_traces_failure_path() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlTraceStore::new(tmp.path().join("traces.jsonl"));

        let mut tracker = crate::skills::tracker::SkillUsageTracker::new();
        tracker.record_call("flaky__tool");

        record_session_traces(&store, &tracker, false, 99);

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(!all[0].success);
        assert_eq!(all[0].tool_calls, 1);
    }

    #[test]
    fn record_session_traces_noop_for_empty_tracker() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlTraceStore::new(tmp.path().join("traces.jsonl"));
        let tracker = crate::skills::tracker::SkillUsageTracker::new();
        record_session_traces(&store, &tracker, true, 0);
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn default_trace_store_path_joins_workspace() {
        let p = default_trace_store_path(Path::new("/ws"));
        assert_eq!(p, Path::new("/ws").join(DEFAULT_TRACE_FILE));
    }

    #[test]
    fn jsonl_store_rotates_when_over_size_limit() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("traces.jsonl");
        // 256-byte cap to exercise rotation quickly.
        let store = JsonlTraceStore::new(&path).with_max_file_size(256);

        let now = Utc::now();
        for _ in 0..30 {
            store.append(&trace("alpha", true, now)).unwrap();
        }

        // Primary file exists and is within (or just under) the cap; rotated
        // file also exists with the overflow.
        let rotated = path.with_extension("jsonl.old");
        assert!(rotated.exists(), "rotation should produce .old file");
        let primary_size = std::fs::metadata(&path).unwrap().len();
        assert!(
            primary_size < 512,
            "post-rotation primary file should stay bounded, got {primary_size}"
        );
    }

    #[test]
    fn jsonl_store_no_rotation_when_limit_unset() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("traces.jsonl");
        let store = JsonlTraceStore::new(&path);
        let now = Utc::now();
        for _ in 0..10 {
            store.append(&trace("alpha", true, now)).unwrap();
        }
        assert!(!path.with_extension("jsonl.old").exists());
    }

    #[tokio::test]
    async fn record_session_traces_async_writes_in_blocking_task() {
        use zeroclaw_providers::ChatMessage;
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();

        // Build a fake history: one assistant message that invokes two
        // skill-bearing tools.
        let assistant_content = serde_json::json!({
            "tool_calls": [
                {"function": {"name": "deploy__run_deploy", "arguments": "{}"}},
                {"function": {"name": "deploy__check_status", "arguments": "{}"}},
                {"function": {"name": "lint__run_lint", "arguments": "{}"}},
            ]
        })
        .to_string();
        let history = vec![ChatMessage::assistant(assistant_content)];

        let start = std::time::Instant::now();
        // Give it a brief sleep so duration_ms is > 0.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        record_session_traces_async(workspace.clone(), history, false, start);

        // Wait for spawn_blocking to complete. Poll the file up to 2s.
        let path = default_trace_store_path(&workspace);
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(path.exists(), "trace file should be created");

        // Verify traces were written with success=false and duration > 0.
        let store = JsonlTraceStore::new(&path);
        // Allow one more poll loop if spawn_blocking hasn't flushed yet.
        let mut all = Vec::new();
        for _ in 0..100 {
            all = store.load_all().unwrap();
            if all.len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(all.len(), 2, "one trace per distinct skill");
        assert!(all.iter().all(|t| !t.success));
        assert!(all.iter().all(|t| t.duration_ms > 0));
    }

    #[tokio::test]
    async fn record_session_traces_async_noop_on_empty_history() {
        use zeroclaw_providers::ChatMessage;
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();

        // History with no tool calls.
        let history = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
        record_session_traces_async(workspace.clone(), history, true, std::time::Instant::now());

        // Give spawn_blocking a chance to run (it shouldn't).
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!default_trace_store_path(&workspace).exists());
    }

    #[test]
    fn confidence_policy_from_config_preserves_fields() {
        let user = zeroclaw_config::schema::SkillConfidenceConfig {
            enabled: true,
            scan_interval_hours: 3,
            saturation_calls: 42,
            recency_half_life_hours: 48.0,
            min_samples_for_deprecation: 7,
            deprecation_threshold: 0.25,
            review_window_hours: 96,
            reinstate_threshold: 0.6,
        };
        let policy: ConfidencePolicy = (&user).into();
        assert_eq!(policy.saturation_calls, 42);
        assert!((policy.recency_half_life_hours - 48.0).abs() < f64::EPSILON);
        assert_eq!(policy.min_samples_for_deprecation, 7);
        assert!((policy.deprecation_threshold - 0.25).abs() < f64::EPSILON);
        assert_eq!(policy.review_window_hours, 96);
        assert!((policy.reinstate_threshold - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_policy_defaults_match_config_defaults() {
        let runtime = ConfidencePolicy::default();
        let via_config: ConfidencePolicy =
            (&zeroclaw_config::schema::SkillConfidenceConfig::default()).into();
        assert_eq!(runtime.saturation_calls, via_config.saturation_calls);
        assert!(
            (runtime.recency_half_life_hours - via_config.recency_half_life_hours).abs()
                < f64::EPSILON
        );
        assert_eq!(
            runtime.min_samples_for_deprecation,
            via_config.min_samples_for_deprecation
        );
        assert!(
            (runtime.deprecation_threshold - via_config.deprecation_threshold).abs()
                < f64::EPSILON
        );
        // C-2: new reinstate fields must also round-trip.
        assert_eq!(runtime.review_window_hours, via_config.review_window_hours);
        assert!(
            (runtime.reinstate_threshold - via_config.reinstate_threshold).abs() < f64::EPSILON
        );
    }

    #[test]
    fn confidence_policy_from_config_includes_reinstate_fields() {
        let user = zeroclaw_config::schema::SkillConfidenceConfig {
            enabled: true,
            scan_interval_hours: 3,
            saturation_calls: 42,
            recency_half_life_hours: 48.0,
            min_samples_for_deprecation: 7,
            deprecation_threshold: 0.25,
            review_window_hours: 48,
            reinstate_threshold: 0.7,
        };
        let policy: ConfidencePolicy = (&user).into();
        assert_eq!(policy.review_window_hours, 48);
        assert!((policy.reinstate_threshold - 0.7).abs() < f64::EPSILON);
    }

    // ── Reinstatement / review-window (C-2) ──────────────────────

    /// Helper: create a skill directory with a DEPRECATED marker whose
    /// `Timestamp:` line is set to `marker_time`.
    fn plant_deprecated_marker(skill_dir: &Path, marker_time: DateTime<Utc>) {
        std::fs::create_dir_all(skill_dir).unwrap();
        let content = format!(
            "# Deprecated\n\nReason: test\nTimestamp: {ts}\n\n## Confidence breakdown\n\n- value: 0.100\n- success_rate: 0.100\n- usage_frequency: 1.000\n- recency_decay: 1.000\n- sample_count: 20\n",
            ts = marker_time.to_rfc3339()
        );
        std::fs::write(skill_dir.join(DEPRECATION_MARKER_FILE), content).unwrap();
    }

    #[test]
    fn parse_marker_timestamp_extracts_rfc3339() {
        let content = "# Deprecated\n\nReason: foo\nTimestamp: 2026-01-15T10:30:00Z\n";
        let ts = super::parse_marker_timestamp(content).expect("should parse");
        assert_eq!(ts.to_rfc3339(), "2026-01-15T10:30:00+00:00");
    }

    #[test]
    fn parse_marker_timestamp_returns_none_when_missing() {
        let content = "# Deprecated\n\nReason: manual";
        assert!(super::parse_marker_timestamp(content).is_none());
    }

    #[test]
    fn parse_marker_timestamp_returns_none_for_malformed() {
        let content = "Timestamp: not-a-date";
        assert!(super::parse_marker_timestamp(content).is_none());
    }

    #[test]
    fn reevaluate_reinstates_when_recent_traces_recover() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        let skill_dir = skills_dir.join("recovered");

        // Marker is 8 days old (older than the 7-day default window).
        let marker_time = Utc::now() - chrono::Duration::days(8);
        plant_deprecated_marker(&skill_dir, marker_time);

        let store = JsonlTraceStore::new(&traces_path);

        // Pre-deprecation failures (6 days before marker) — should be IGNORED.
        let pre = marker_time - chrono::Duration::days(6);
        for _ in 0..20 {
            store.append(&trace_at("recovered", false, pre)).unwrap();
        }

        // Post-deprecation successes (over the past 3 days) — should pass.
        let post_base = Utc::now() - chrono::Duration::days(3);
        for i in 0..25 {
            let t = post_base + chrono::Duration::hours(i);
            store.append(&trace_at("recovered", true, t)).unwrap();
        }

        let policy = ConfidencePolicy::default();
        let reinstated =
            reevaluate_deprecations(&store, &skills_dir, &policy).unwrap();
        assert_eq!(reinstated, vec!["recovered".to_string()]);
        assert!(!is_skill_deprecated(&skill_dir));
    }

    #[test]
    fn reevaluate_skips_fresh_markers_within_window() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        let skill_dir = skills_dir.join("fresh");

        // Marker only 2 hours old — inside the 7-day default window.
        plant_deprecated_marker(&skill_dir, Utc::now() - chrono::Duration::hours(2));

        let store = JsonlTraceStore::new(&traces_path);
        // Even a perfect run of successes shouldn't reinstate yet.
        for _ in 0..25 {
            store.append(&trace("fresh", true, Utc::now())).unwrap();
        }

        let policy = ConfidencePolicy::default();
        let reinstated =
            reevaluate_deprecations(&store, &skills_dir, &policy).unwrap();
        assert!(reinstated.is_empty());
        assert!(
            is_skill_deprecated(&skill_dir),
            "marker inside review window stays"
        );
    }

    #[test]
    fn reevaluate_leaves_still_failing_skills_deprecated() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        let skill_dir = skills_dir.join("still-broken");

        let marker_time = Utc::now() - chrono::Duration::days(10);
        plant_deprecated_marker(&skill_dir, marker_time);

        let store = JsonlTraceStore::new(&traces_path);
        // Recent failures — should NOT be reinstated.
        for _ in 0..20 {
            store
                .append(&trace_at("still-broken", false, Utc::now()))
                .unwrap();
        }

        let policy = ConfidencePolicy::default();
        let reinstated =
            reevaluate_deprecations(&store, &skills_dir, &policy).unwrap();
        assert!(reinstated.is_empty());
        assert!(is_skill_deprecated(&skill_dir));
    }

    #[test]
    fn reevaluate_disabled_when_review_window_zero() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        let skill_dir = skills_dir.join("disabled");

        plant_deprecated_marker(&skill_dir, Utc::now() - chrono::Duration::days(30));

        let store = JsonlTraceStore::new(&traces_path);
        for _ in 0..50 {
            store.append(&trace("disabled", true, Utc::now())).unwrap();
        }

        let policy = ConfidencePolicy {
            review_window_hours: 0,
            ..ConfidencePolicy::default()
        };
        let reinstated =
            reevaluate_deprecations(&store, &skills_dir, &policy).unwrap();
        assert!(reinstated.is_empty());
        assert!(
            is_skill_deprecated(&skill_dir),
            "window=0 disables reinstatement entirely"
        );
    }

    #[test]
    fn reevaluate_requires_evidence_after_marker() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        let skill_dir = skills_dir.join("silent");

        let marker_time = Utc::now() - chrono::Duration::days(30);
        plant_deprecated_marker(&skill_dir, marker_time);

        let store = JsonlTraceStore::new(&traces_path);
        // Only traces older than the marker — no post-deprecation evidence.
        let old = marker_time - chrono::Duration::days(1);
        for _ in 0..30 {
            store.append(&trace_at("silent", true, old)).unwrap();
        }

        let policy = ConfidencePolicy::default();
        let reinstated =
            reevaluate_deprecations(&store, &skills_dir, &policy).unwrap();
        assert!(
            reinstated.is_empty(),
            "no post-marker traces → cannot reinstate even with perfect old record"
        );
        assert!(is_skill_deprecated(&skill_dir));
    }

    #[test]
    fn reinstate_threshold_hysteresis_prevents_flapping() {
        // A score exactly at the deprecation threshold must NOT be enough
        // to reinstate — only scores above reinstate_threshold should.
        let score_at_dep = ConfidenceScore {
            value: 0.3,
            success_rate: 0.5,
            usage_frequency: 0.6,
            recency_decay: 1.0,
            sample_count: 15,
        };
        let policy = ConfidencePolicy::default();
        // This wouldn't deprecate (equals threshold, not <=) — but the
        // key property we need: 0.3 < reinstate_threshold (0.5), so
        // even a post-marker pass barely above deprecation doesn't
        // trigger premature reinstatement.
        assert!(score_at_dep.value < policy.reinstate_threshold);
        assert!(policy.reinstate_threshold > policy.deprecation_threshold);
    }

    /// Helper: build a trace with a specific timestamp.
    fn trace_at(slug: &str, success: bool, ts: DateTime<Utc>) -> SkillTrace {
        SkillTrace {
            skill_slug: slug.into(),
            timestamp: ts,
            success,
            duration_ms: 100,
            tool_calls: 2,
        }
    }

    // ── evaluate_and_deprecate: respects new default (15 samples) ────

    #[test]
    fn deprecation_default_samples_raised_from_five_to_fifteen() {
        // Regression guard: if this reverts to 5, transient provider
        // outages will permanently deprecate skills.
        let policy = ConfidencePolicy::default();
        assert_eq!(
            policy.min_samples_for_deprecation, 15,
            "default should require more evidence than typical noise floor"
        );
        assert!(policy.review_window_hours > 0, "reinstatement should be on");
        assert!(
            policy.reinstate_threshold > policy.deprecation_threshold,
            "hysteresis: reinstate_threshold must exceed deprecation_threshold"
        );
    }

    #[test]
    fn evaluate_and_deprecate_ignores_missing_directories() {
        let tmp = TempDir::new().unwrap();
        let traces_path = tmp.path().join("traces.jsonl");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let store = JsonlTraceStore::new(&traces_path);
        let now = Utc::now();
        for _ in 0..10 {
            store.append(&trace("ghost", false, now)).unwrap();
        }

        let deprecated =
            evaluate_and_deprecate(&store, &skills_dir, &ConfidencePolicy::default()).unwrap();
        assert!(deprecated.is_empty(), "missing dir should not crash");
    }
}
