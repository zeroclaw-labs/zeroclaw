//! Authority-lifetime acknowledgements for published configuration changes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zeroclaw_config::live::ConfigRevision;

const MAX_APPLICATION_RECORDS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PublishedConfigRevision {
    pub epoch: String,
    pub sequence: u64,
}

impl From<ConfigRevision> for PublishedConfigRevision {
    fn from(revision: ConfigRevision) -> Self {
        Self {
            epoch: revision.epoch().to_string(),
            sequence: revision.sequence(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum ConfigApplicationTarget {
    /// Scoped to the publication epoch in the enclosing revision.
    Daemon,
    Session {
        id: String,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum ConfigApplicationOutcome {
    Pending,
    AppliedLive,
    QueuedForReload,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum ConfigApplicationReason {
    AwaitingAcknowledgement,
    DaemonReloadRequired,
    ChangeScopeUnavailable,
    TargetRetired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ConfigApplicationRecord {
    /// Canonical field/key components. A dotted map key is one component.
    pub path: Vec<String>,
    pub target: ConfigApplicationTarget,
    pub revision: PublishedConfigRevision,
    pub outcome: ConfigApplicationOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ConfigApplicationReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ConfigApplicationStatus {
    pub published_revision: PublishedConfigRevision,
    /// Latest observed changes/attempts, not every subsystem's startup state.
    pub records: Vec<ConfigApplicationRecord>,
    pub record_limit: usize,
    pub truncated: bool,
}

struct ApplicationResult {
    revision: ConfigRevision,
    outcome: ConfigApplicationOutcome,
    reason: Option<ConfigApplicationReason>,
}

#[derive(Default)]
pub(crate) struct ConfigApplicationLedger {
    records: BTreeMap<(Vec<String>, ConfigApplicationTarget), ApplicationResult>,
    truncated: bool,
}

impl ConfigApplicationLedger {
    pub(crate) fn published(
        &mut self,
        revision: ConfigRevision,
        changed_paths: Option<&[Vec<String>]>,
        attempts: &[(ConfigApplicationTarget, Vec<Vec<String>>)],
    ) {
        let unknown = vec![Vec::new()];
        let paths = changed_paths.unwrap_or(&unknown);
        // A changed subtree invalidates earlier acknowledgements in it,
        // including removed aliases. Unrelated paths keep their real revision.
        self.records
            .retain(|(path, _), _| !paths.iter().any(|changed| path.starts_with(changed)));
        for path in paths {
            self.records.insert(
                (path.clone(), ConfigApplicationTarget::Daemon),
                ApplicationResult {
                    revision,
                    outcome: ConfigApplicationOutcome::QueuedForReload,
                    reason: Some(if changed_paths.is_some() {
                        ConfigApplicationReason::DaemonReloadRequired
                    } else {
                        ConfigApplicationReason::ChangeScopeUnavailable
                    }),
                },
            );
        }
        if changed_paths.is_some() {
            for (target, requested) in attempts {
                for path in requested {
                    if paths.contains(path) {
                        self.records.insert(
                            (path.clone(), target.clone()),
                            ApplicationResult {
                                revision,
                                outcome: ConfigApplicationOutcome::Pending,
                                reason: Some(ConfigApplicationReason::AwaitingAcknowledgement),
                            },
                        );
                    }
                }
            }
        }
        while self.records.len() > MAX_APPLICATION_RECORDS {
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, result)| result.revision.sequence())
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                self.records.remove(&key);
                self.truncated = true;
            }
        }
    }

    pub(crate) fn complete(
        &mut self,
        revision: ConfigRevision,
        target: &ConfigApplicationTarget,
        paths: &[Vec<String>],
        applied: bool,
    ) {
        for path in paths {
            let Some(result) = self.records.get_mut(&(path.clone(), target.clone())) else {
                continue;
            };
            if result.revision != revision || result.outcome != ConfigApplicationOutcome::Pending {
                continue;
            }
            result.outcome = if applied {
                ConfigApplicationOutcome::AppliedLive
            } else {
                ConfigApplicationOutcome::Rejected
            };
            result.reason = (!applied).then_some(ConfigApplicationReason::TargetRetired);
        }
    }

    pub(crate) fn retire_targets(&mut self, current: impl Fn(&ConfigApplicationTarget) -> bool) {
        self.records.retain(|(_, target), _| current(target));
    }

    pub(crate) fn status(
        &self,
        revision: ConfigRevision,
        visible: impl Fn(&ConfigApplicationTarget) -> bool,
    ) -> ConfigApplicationStatus {
        ConfigApplicationStatus {
            published_revision: revision.into(),
            records: self
                .records
                .iter()
                .filter(|((_, target), _)| visible(target))
                .map(|((path, target), result)| ConfigApplicationRecord {
                    path: path.clone(),
                    target: target.clone(),
                    revision: result.revision.into(),
                    outcome: result.outcome,
                    reason: result.reason,
                })
                .collect(),
            record_limit: MAX_APPLICATION_RECORDS,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::{live::LiveConfig, schema::Config};

    fn next(live: &LiveConfig) -> ConfigRevision {
        let revision = live.next_revision().unwrap();
        live.publish(revision, Config::default()).unwrap();
        revision
    }

    #[test]
    fn config_application_is_path_target_and_revision_specific() {
        let live = LiveConfig::new(Config::default());
        let first = next(&live);
        let path = vec!["agents".into(), "worker".into(), "model_provider".into()];
        let other_path = vec!["gateway".into(), "port".into()];
        let a = ConfigApplicationTarget::Session {
            id: "a".into(),
            generation: 1,
        };
        let b = ConfigApplicationTarget::Session {
            id: "b".into(),
            generation: 2,
        };
        let mut ledger = ConfigApplicationLedger::default();
        ledger.published(
            first,
            Some(std::slice::from_ref(&path)),
            &[
                (a.clone(), vec![path.clone()]),
                (b.clone(), vec![path.clone()]),
            ],
        );
        ledger.complete(first, &a, std::slice::from_ref(&path), true);
        ledger.complete(first, &b, std::slice::from_ref(&path), false);
        let unrelated = next(&live);
        ledger.published(unrelated, Some(&[other_path]), &[]);
        let status = ledger.status(unrelated, |_| true);
        let result_a = status
            .records
            .iter()
            .find(|record| record.target == a)
            .unwrap();
        let result_b = status
            .records
            .iter()
            .find(|record| record.target == b)
            .unwrap();
        assert_eq!(result_a.revision, PublishedConfigRevision::from(first));
        assert_eq!(result_a.outcome, ConfigApplicationOutcome::AppliedLive);
        assert_eq!(result_b.outcome, ConfigApplicationOutcome::Rejected);

        let newer = next(&live);
        ledger.published(
            newer,
            Some(std::slice::from_ref(&path)),
            &[(a.clone(), vec![path.clone()])],
        );
        ledger.complete(first, &a, std::slice::from_ref(&path), true);
        let replacement_epoch = LiveConfig::new(Config::default());
        ledger.complete(
            next(&replacement_epoch),
            &a,
            std::slice::from_ref(&path),
            true,
        );
        let status = ledger.status(newer, |_| true);
        let result_a = status
            .records
            .iter()
            .find(|record| record.target == a)
            .unwrap();
        assert_eq!(result_a.outcome, ConfigApplicationOutcome::Pending);
        assert_eq!(result_a.revision, PublishedConfigRevision::from(newer));
        ledger.retire_targets(|target| target != &a);
        ledger.complete(newer, &a, &[path], true);
        assert!(
            ledger
                .status(newer, |_| true)
                .records
                .iter()
                .all(|record| record.target != a)
        );
    }

    #[test]
    fn config_application_unknown_scope_cannot_be_acknowledged() {
        let live = LiveConfig::new(Config::default());
        let revision = next(&live);
        let target = ConfigApplicationTarget::Session {
            id: "a".into(),
            generation: 1,
        };
        let mut ledger = ConfigApplicationLedger::default();
        ledger.published(revision, None, &[(target.clone(), vec![Vec::new()])]);
        ledger.complete(revision, &target, &[Vec::new()], true);
        let status = ledger.status(revision, |_| true);
        assert_eq!(status.records.len(), 1);
        assert!(status.records[0].path.is_empty());
        assert_eq!(
            status.records[0].outcome,
            ConfigApplicationOutcome::QueuedForReload
        );
        assert_eq!(
            status.records[0].reason,
            Some(ConfigApplicationReason::ChangeScopeUnavailable)
        );
    }

    #[test]
    fn config_application_retention_is_bounded_and_disclosed() {
        let live = LiveConfig::new(Config::default());
        let revision = next(&live);
        let paths: Vec<Vec<String>> = (0..MAX_APPLICATION_RECORDS + 1)
            .map(|index| vec!["aliases".into(), index.to_string()])
            .collect();
        let mut ledger = ConfigApplicationLedger::default();
        ledger.published(revision, Some(&paths), &[]);
        let status = ledger.status(revision, |_| true);
        assert_eq!(status.records.len(), MAX_APPLICATION_RECORDS);
        assert!(status.truncated);
        assert_eq!(status.record_limit, MAX_APPLICATION_RECORDS);
    }
}
