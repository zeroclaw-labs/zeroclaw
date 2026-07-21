//! Repeated-run statistics: pass@k, pass^k, and variance / error bars for live
//! suites. Pure math, no agent I/O.

use crate::Mode;

/// Clamp a requested repeat count to 1..=50 and resolve it for the mode. Returns
/// the effective count and any warnings (clamping, replay-ignore). Replay is
/// deterministic, so `repeat > 1` runs once.
pub fn effective_repeat(mode: Mode, requested: u32) -> (u32, Vec<String>) {
    let mut warnings = Vec::new();
    let mut k = requested;
    if k < 1 {
        k = 1;
        warnings.push("repeat < 1 clamped to 1".to_string());
    } else if k > 50 {
        k = 50;
        warnings.push(format!("repeat {requested} clamped to 50"));
    }
    if mode == Mode::Replay && k > 1 {
        warnings.push("replay is deterministic; repeat ignored".to_string());
        k = 1;
    }
    (k, warnings)
}

/// pass@k: at least one of the k runs passed.
pub fn pass_at_k(passes: u32, k: u32) -> bool {
    k > 0 && passes > 0
}

/// pass^k: every one of the k runs passed (the consistency standard used for
/// gating and baselines).
pub fn pass_hat_k(passes: u32, k: u32) -> bool {
    k > 0 && passes == k
}

/// The probability that all k independent trials pass given a per-trial success
/// rate: `p^k`. (Sanity-checked estimator, not derived from observed runs.)
pub fn pass_hat_k_expectation(per_trial_rate: f64, k: u32) -> f64 {
    per_trial_rate.powi(k as i32)
}

/// Arithmetic mean, or 0.0 for an empty slice.
pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Sample standard deviation (Bessel's correction). 0.0 for fewer than 2 values.
pub fn sample_stddev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0);
    var.sqrt()
}

/// Standard error of the mean over per-case success proportions:
/// `sqrt(Σ(p_i − p̄)² / (n(n−1)))`. 0.0 for fewer than 2 values.
pub fn sem(proportions: &[f64]) -> f64 {
    let n = proportions.len();
    if n < 2 {
        return 0.0;
    }
    let m = mean(proportions);
    let ss: f64 = proportions.iter().map(|p| (p - m).powi(2)).sum();
    (ss / (n as f64 * (n as f64 - 1.0))).sqrt()
}

/// Collapse per-case proportions into cluster means: cases sharing a `cluster`
/// label are averaged into one value first (guarding correlated case families);
/// cases with no cluster each stand alone. The SEM is then taken over these.
pub fn cluster_means(items: &[(Option<String>, f64)]) -> Vec<f64> {
    use std::collections::BTreeMap;
    let mut clusters: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut solo: Vec<f64> = Vec::new();
    for (cluster, p) in items {
        match cluster {
            Some(name) => clusters.entry(name.clone()).or_default().push(*p),
            None => solo.push(*p),
        }
    }
    let mut out: Vec<f64> = clusters.values().map(|ps| mean(ps)).collect();
    out.extend(solo);
    out
}

/// Two-sided 95% Student-t multiplier for `df` degrees of freedom (t_{0.975}).
/// Small-n suites collapse to only a few units, where the normal z=1.96 badly
/// understates the interval; this returns the correct larger multiplier and
/// converges to ~1.96 for large df.
pub fn t95_multiplier(df: usize) -> f64 {
    // t_{0.975} table; values between listed df use the next-lower df (conservative).
    const TABLE: &[(usize, f64)] = &[
        (1, 12.706),
        (2, 4.303),
        (3, 3.182),
        (4, 2.776),
        (5, 2.571),
        (6, 2.447),
        (7, 2.365),
        (8, 2.306),
        (9, 2.262),
        (10, 2.228),
        (12, 2.179),
        (15, 2.131),
        (20, 2.086),
        (25, 2.060),
        (30, 2.042),
    ];
    if df == 0 {
        return f64::INFINITY;
    }
    if df > 30 {
        return 1.96;
    }
    // Use the largest listed df not exceeding `df` (next-lower table row, whose
    // multiplier is >= the true value): conservative for unlisted df.
    let mut mult = 1.96;
    for (d, t) in TABLE {
        if *d <= df {
            mult = *t;
        }
    }
    mult
}

/// A low-signal / suspect-task note for `passes` out of `k` runs, if any.
pub fn suspect_note(passes: u32, k: u32) -> Option<String> {
    if passes != 0 {
        return None;
    }
    if k >= 20 {
        Some(format!(
            "suspect: broken task (0% across {k} trials usually means the task, not the agent)"
        ))
    } else if k >= 5 {
        Some(format!(
            "0/{k}: low signal - possible broken task; rerun with higher repeat"
        ))
    } else {
        None
    }
}

/// One run's summary, fed into [`RepeatStats::from_runs`].
pub struct RunSample {
    pub passed: bool,
    pub total_tokens: u64,
    pub duration_ms: u64,
    /// Per-check `(name, passed)` for flip counting across runs.
    pub checks: Vec<(String, bool)>,
}

/// Aggregated statistics over k isolated runs of one case.
#[derive(Debug, Clone)]
pub struct RepeatStats {
    pub k: u32,
    pub passes: u32,
    pub token_mean: f64,
    pub token_stddev: f64,
    pub duration_mean: f64,
    pub duration_stddev: f64,
    /// Per-check count of runs whose result differed from that check's modal
    /// (most common) result across the k runs.
    pub check_flips: std::collections::BTreeMap<String, u32>,
}

impl RepeatStats {
    pub fn from_runs(k: u32, runs: &[RunSample]) -> RepeatStats {
        let passes = runs.iter().filter(|r| r.passed).count() as u32;
        let tokens: Vec<f64> = runs.iter().map(|r| r.total_tokens as f64).collect();
        let durations: Vec<f64> = runs.iter().map(|r| r.duration_ms as f64).collect();

        // Flip counts: per check, runs differing from the modal result.
        let mut per_check: std::collections::BTreeMap<String, Vec<bool>> =
            std::collections::BTreeMap::new();
        for run in runs {
            for (name, passed) in &run.checks {
                per_check.entry(name.clone()).or_default().push(*passed);
            }
        }
        let mut check_flips = std::collections::BTreeMap::new();
        for (name, results) in per_check {
            let trues = results.iter().filter(|b| **b).count();
            let falses = results.len() - trues;
            let flips = trues.min(falses) as u32;
            if flips > 0 {
                check_flips.insert(name, flips);
            }
        }

        RepeatStats {
            k,
            passes,
            token_mean: mean(&tokens),
            token_stddev: sample_stddev(&tokens),
            duration_mean: mean(&durations),
            duration_stddev: sample_stddev(&durations),
            check_flips,
        }
    }

    pub fn proportion(&self) -> f64 {
        if self.k == 0 {
            0.0
        } else {
            self.passes as f64 / self.k as f64
        }
    }

    pub fn pass_at_k(&self) -> bool {
        pass_at_k(self.passes, self.k)
    }

    pub fn pass_hat_k(&self) -> bool {
        pass_hat_k(self.passes, self.k)
    }

    pub fn suspect_note(&self) -> Option<String> {
        suspect_note(self.passes, self.k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_hat_k_formula() {
        // Per-trial rate 0.75 over k=3 => 0.75^3 ~= 0.4219.
        let p = pass_hat_k_expectation(0.75, 3);
        assert!((p - 0.4219).abs() < 0.001, "got {p}");
    }

    #[test]
    fn pass_at_and_hat_k_booleans() {
        assert!(pass_at_k(1, 3));
        assert!(!pass_at_k(0, 3));
        assert!(pass_hat_k(3, 3));
        assert!(!pass_hat_k(2, 3));
    }

    #[test]
    fn sem_zero_when_all_cases_equal() {
        assert_eq!(sem(&[0.5, 0.5, 0.5, 0.5]), 0.0);
        assert!(sem(&[0.0, 1.0]) > 0.0);
    }

    #[test]
    fn cluster_averaging_reduces_n() {
        // Four cases: cluster "a" = {1.0, 0.0} (mean 0.5) and two solos {0.2, 0.8}
        // (neither 0.5). Result is 3 values, and the collapse is observable because
        // 0.5 comes only from the cluster mean, and the raw members are gone.
        let items = vec![
            (Some("a".to_string()), 1.0),
            (Some("a".to_string()), 0.0),
            (None, 0.2),
            (None, 0.8),
        ];
        let means = cluster_means(&items);
        assert_eq!(means.len(), 3);
        assert!(
            means.contains(&0.5),
            "cluster must collapse to its mean 0.5"
        );
        assert!(
            !means.contains(&1.0) && !means.contains(&0.0),
            "raw cluster members must be gone"
        );
    }

    #[test]
    fn t95_multiplier_widens_at_small_n() {
        // Small df need a much larger multiplier than the normal z=1.96.
        assert!(t95_multiplier(1) > 12.0);
        assert!(t95_multiplier(2) > 4.0);
        // Large df converges to the normal approximation.
        assert!((t95_multiplier(100) - 1.96).abs() < 1e-9);
        // Monotonic-ish: smaller df => larger (or equal) multiplier.
        assert!(t95_multiplier(2) >= t95_multiplier(10));
    }

    #[test]
    fn repeat_clamped_and_warned() {
        let (k, w) = effective_repeat(Mode::Live, 200);
        assert_eq!(k, 50);
        assert!(w.iter().any(|m| m.contains("clamped to 50")));
        let (k0, w0) = effective_repeat(Mode::Live, 0);
        assert_eq!(k0, 1);
        assert!(w0.iter().any(|m| m.contains("clamped to 1")));
    }

    #[test]
    fn replay_repeat_ignored_with_warning() {
        let (k, w) = effective_repeat(Mode::Replay, 10);
        assert_eq!(k, 1);
        assert!(w.iter().any(|m| m.contains("replay is deterministic")));
    }

    #[test]
    fn suspect_note_thresholds() {
        assert!(suspect_note(1, 20).is_none());
        assert!(suspect_note(0, 3).is_none());
        assert!(suspect_note(0, 5).unwrap().contains("low signal"));
        assert!(
            suspect_note(0, 20)
                .unwrap()
                .contains("suspect: broken task")
        );
    }
}
