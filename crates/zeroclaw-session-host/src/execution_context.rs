//! Historical execution facts resolved from the current admission configuration.
//! This materializer owns no policy state and grants no authority on replay.
use std::path::Path;
use zeroclaw_config::schema::Config;
use zeroclaw_kernel::session::SessionError;

pub fn execution_context_evidence(
    config: &Config,
    alias: &str,
    cwd: Option<&Path>,
) -> Result<serde_json::Value, SessionError> {
    let agent = config
        .agent(alias)
        .ok_or_else(|| SessionError::PermissionDenied("agent missing".into()))?;
    let profile = config
        .risk_profile_for_agent(alias)
        .ok_or_else(|| SessionError::PermissionDenied("agent risk profile missing".into()))?;
    let workspace = config.agent_workspace_dir(alias);
    Ok(serde_json::json!({
        "cwd":cwd.unwrap_or(&workspace),
        "workspace_roots":[cwd.unwrap_or(&workspace)],
        "permission_profile":agent.risk_profile,
        "permissions":{
            "autonomy_level":profile.level,
            "workspace_only":profile.workspace_only,
            "allowed_roots":profile.allowed_roots,
            "forbidden_paths":profile.forbidden_paths,
            "allowed_commands":profile.allowed_commands,
            "allowed_tools":profile.effective_allowed_tools(),
            "excluded_tools":profile.excluded_tools,
            "auto_approve":profile.auto_approve,
            "always_ask":profile.always_ask,
            "require_approval_for_medium_risk":profile.require_approval_for_medium_risk,
            "block_high_risk_commands":profile.block_high_risk_commands,
            "approval_route":profile.approval_route,
            "delegation_policy":profile.delegation_policy,
            "sandbox":profile.sandbox_config(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_context_resolves_current_policy_without_reusing_historical_grants() {
        let mut config = Config::default();
        config.agents.insert(
            "fixture".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: "fixture-policy".into(),
                ..Default::default()
            },
        );
        config
            .risk_profiles
            .insert("fixture-policy".into(), Default::default());
        let first =
            execution_context_evidence(&config, "fixture", Some(Path::new("/fixture/session")))
                .unwrap();
        assert_eq!(first["cwd"], "/fixture/session");
        assert_eq!(
            first["workspace_roots"],
            serde_json::json!(["/fixture/session"])
        );
        assert_eq!(first["permission_profile"], "fixture-policy");
        config
            .risk_profiles
            .get_mut("fixture-policy")
            .unwrap()
            .deny_all_tools = true;
        let next = execution_context_evidence(&config, "fixture", None).unwrap();
        assert_eq!(
            next["cwd"],
            serde_json::json!(config.agent_workspace_dir("fixture"))
        );
        assert_eq!(next["permissions"]["allowed_tools"], serde_json::json!([]));
        assert_ne!(next["permissions"], first["permissions"]);
        assert!(execution_context_evidence(&config, "missing", None).is_err());
        config.risk_profiles.clear();
        assert!(execution_context_evidence(&config, "fixture", None).is_err());
    }
}
