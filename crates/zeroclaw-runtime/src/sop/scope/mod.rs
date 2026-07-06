mod groups;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use zeroclaw_config::policy::SecurityPolicy;

/// Per-step allow/deny tool scope. Enforcement is opt-in through SOP config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepToolScope {
    /// Only these names or groups are allowed when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// These names or groups are always subtracted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

/// Resolve the additional excluded tools for an active SOP step.
///
/// This resolver is narrow-only: step scope can remove tools from the registry,
/// never restore a tool already denied by the configured security policy.
pub fn resolve_excluded(
    registry_names: &[String],
    scope: &StepToolScope,
    security: Option<&SecurityPolicy>,
    mandatory_infra: &[String],
) -> Vec<String> {
    let allow = scope
        .allow
        .as_ref()
        .map(|entries| expand_entries(entries).collect::<HashSet<_>>());
    let deny = expand_entries(&scope.deny).collect::<HashSet<_>>();
    let mandatory = mandatory_infra
        .iter()
        .map(|name| normalize(name))
        .collect::<HashSet<_>>();

    let mut excluded = Vec::new();
    for name in registry_names {
        let normalized = normalize(name);
        let policy_allows = security.is_none_or(|policy| policy.is_tool_allowed(name));
        if !policy_allows {
            excluded.push(name.clone());
            continue;
        }

        if mandatory.contains(&normalized) {
            continue;
        }

        let step_allows = allow
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&normalized));
        let step_denies = deny.contains(&normalized);

        if !step_allows || step_denies {
            excluded.push(name.clone());
        }
    }

    excluded.sort();
    excluded.dedup();
    excluded
}

fn expand_entries<'a>(entries: &'a [String]) -> impl Iterator<Item = String> + 'a {
    entries.iter().flat_map(|entry| {
        groups::expand_group(entry)
            .map(|expanded| expanded.iter().map(|name| normalize(name)).collect())
            .unwrap_or_else(|| vec![normalize(entry)])
    })
}

fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::policy::SecurityPolicy;

    fn registry() -> Vec<String> {
        ["read_file", "write_file", "shell", "sop_advance"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn allow_scope_excludes_everything_else() {
        let scope = StepToolScope {
            allow: Some(vec!["read_file".into()]),
            deny: Vec::new(),
        };

        assert_eq!(
            resolve_excluded(&registry(), &scope, None, &[]),
            vec![
                "shell".to_string(),
                "sop_advance".to_string(),
                "write_file".to_string()
            ]
        );
    }

    #[test]
    fn deny_scope_subtracts_from_allow() {
        let scope = StepToolScope {
            allow: Some(vec!["fs".into()]),
            deny: vec!["write_file".into()],
        };

        assert_eq!(
            resolve_excluded(&registry(), &scope, None, &[]),
            vec![
                "shell".to_string(),
                "sop_advance".to_string(),
                "write_file".to_string()
            ]
        );
    }

    #[test]
    fn security_policy_can_only_narrow() {
        let policy = SecurityPolicy {
            allowed_tools: Some(vec!["read_file".into()]),
            excluded_tools: None,
            ..Default::default()
        };
        let scope = StepToolScope {
            allow: Some(vec!["read_file".into(), "shell".into()]),
            deny: Vec::new(),
        };

        assert_eq!(
            resolve_excluded(&registry(), &scope, Some(&policy), &[]),
            vec![
                "shell".to_string(),
                "sop_advance".to_string(),
                "write_file".to_string()
            ]
        );
    }

    #[test]
    fn mandatory_infra_survives_empty_allow() {
        let scope = StepToolScope {
            allow: Some(Vec::new()),
            deny: Vec::new(),
        };
        let mandatory = vec!["sop_advance".to_string()];

        assert_eq!(
            resolve_excluded(&registry(), &scope, None, &mandatory),
            vec![
                "read_file".to_string(),
                "shell".to_string(),
                "write_file".to_string()
            ]
        );
    }

    #[test]
    fn mandatory_infra_cannot_restore_policy_denied_tools() {
        let policy = SecurityPolicy {
            allowed_tools: Some(vec!["read_file".into(), "shell".into()]),
            excluded_tools: Some(vec!["sop_advance".into()]),
            ..Default::default()
        };
        let scope = StepToolScope {
            allow: None,
            deny: Vec::new(),
        };
        let mandatory = vec!["sop_advance".to_string()];

        assert_eq!(
            resolve_excluded(&registry(), &scope, Some(&policy), &mandatory),
            vec!["sop_advance".to_string(), "write_file".to_string()]
        );
    }
}
