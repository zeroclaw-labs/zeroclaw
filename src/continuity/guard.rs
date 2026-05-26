use std::collections::VecDeque;

use super::types::{DriftLimits, Identity, IdentityCore};

pub struct ContinuityGuard {
    drift_limits: DriftLimits,
    whiplash_threshold: f64,
    conservative_mode: bool,
    recent_drift_samples: VecDeque<f64>,
}

impl ContinuityGuard {
    pub fn new(drift_limits: DriftLimits) -> Self {
        Self {
            drift_limits,
            whiplash_threshold: 0.03,
            conservative_mode: false,
            recent_drift_samples: VecDeque::new(),
        }
    }

    pub fn record_drift(&mut self, amount: f64) {
        self.recent_drift_samples.push_back(amount);
        if self.recent_drift_samples.len() > 10 {
            self.recent_drift_samples.pop_front();
        }
        self.check_whiplash();
    }

    pub fn is_conservative(&self) -> bool {
        self.conservative_mode
    }

    pub fn check_identity_integrity(&self, identity: &Identity) -> Vec<String> {
        let mut issues = Vec::new();

        if identity.core.name.is_empty() {
            issues.push("IDCORE name is empty".to_string());
        }

        if identity.core.constitution_hash.is_empty() {
            issues.push("IDCORE constitution_hash is empty".to_string());
        }

        if identity.core.creation_epoch == 0 {
            issues.push("IDCORE creation_epoch is zero".to_string());
        }

        let active_commitments: Vec<_> = identity
            .commitments
            .iter()
            .filter(|c| !c.fulfilled && c.expires_at.map_or(true, |e| e > now_timestamp()))
            .collect();

        if active_commitments.len() > 50 {
            issues.push(format!(
                "Too many active commitments: {}",
                active_commitments.len()
            ));
        }

        issues
    }

    pub fn validate_core_immutability(
        &self,
        before: &IdentityCore,
        after: &IdentityCore,
    ) -> Result<(), String> {
        if before.name != after.name {
            return Err("IDCORE name cannot be modified".to_string());
        }
        if before.constitution_hash != after.constitution_hash {
            return Err("IDCORE constitution_hash cannot be modified".to_string());
        }
        if before.creation_epoch != after.creation_epoch {
            return Err("IDCORE creation_epoch cannot be modified".to_string());
        }
        Ok(())
    }

    pub fn reset_conservative_mode(&mut self) {
        self.conservative_mode = false;
        self.recent_drift_samples.clear();
    }

    fn check_whiplash(&mut self) {
        if self.recent_drift_samples.len() < 3 {
            return;
        }
        let recent_total: f64 = self.recent_drift_samples.iter().sum();
        if recent_total > self.whiplash_threshold {
            self.conservative_mode = true;
        }
    }
}

pub fn prune_low_confidence(
    preferences: &mut Vec<super::types::Preference>,
    min_confidence: f64,
) -> Vec<super::types::Preference> {
    let mut pruned = Vec::new();
    preferences.retain(|p| {
        if p.confidence < min_confidence {
            pruned.push(p.clone());
            false
        } else {
            true
        }
    });
    pruned
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
