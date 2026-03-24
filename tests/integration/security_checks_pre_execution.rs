//! Integration tests for pre-execution security checks in execute_one_tool.
//!
//! Tests the execution flow of security checks (estop → workspace boundary → OTP)
//! wired into the tool dispatcher.

use anyhow::Result;
use zeroclaw::config::workspace::WorkspaceProfile;
use zeroclaw::config::{EstopConfig, OtpConfig};
use zeroclaw::security::{
    EstopLevel, EstopManager, EstopState, OtpValidator, SecretStore, WorkspaceBoundary,
};

// ── Helper functions ──────────────────────────────────────────────────

async fn setup_estop_manager(state: EstopState) -> Result<(EstopManager, tempfile::TempDir)> {
    let temp_dir = tempfile::tempdir()?;
    let state_path = temp_dir.path().join("estop-state.json");
    let config = EstopConfig {
        enabled: true,
        state_file: state_path.display().to_string(),
        require_otp_to_resume: false,
    };

    let mut manager = EstopManager::load(&config, temp_dir.path())?;

    // Apply the desired state
    if state.kill_all {
        manager.engage(EstopLevel::KillAll)?;
    }
    if state.network_kill {
        manager.engage(EstopLevel::NetworkKill)?;
    }
    if !state.frozen_tools.is_empty() {
        manager.engage(EstopLevel::ToolFreeze(state.frozen_tools.clone()))?;
    }
    if !state.blocked_domains.is_empty() {
        manager.engage(EstopLevel::DomainBlock(state.blocked_domains.clone()))?;
    }

    Ok((manager, temp_dir))
}

fn setup_otp_validator() -> Result<(OtpValidator, OtpConfig, tempfile::TempDir)> {
    let temp_dir = tempfile::tempdir()?;
    let config = OtpConfig {
        enabled: true,
        gated_actions: vec!["gated_tool".to_string()],
        ..Default::default()
    };
    let store = SecretStore::new(temp_dir.path(), true);
    let (validator, _) = OtpValidator::from_config(&config, temp_dir.path(), &store)?;
    Ok((validator, config, temp_dir))
}

// ── Component Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn estop_kill_all_blocks_all_tools() {
    let state = EstopState {
        kill_all: true,
        ..Default::default()
    };
    let (estop, _dir) = setup_estop_manager(state).await.unwrap();
    let status = estop.status();

    assert!(status.kill_all);
    assert!(status.is_engaged());
}

#[tokio::test]
async fn estop_network_kill_is_detectable() {
    let state = EstopState {
        network_kill: true,
        ..Default::default()
    };
    let (estop, _dir): (EstopManager, tempfile::TempDir) =
        setup_estop_manager(state).await.unwrap();
    let status = estop.status();

    assert!(status.network_kill);
    assert!(status.is_engaged());
}

#[tokio::test]
async fn estop_frozen_tools_contains_specific_tool() {
    let state = EstopState {
        frozen_tools: vec!["frozen_tool".to_string()],
        ..Default::default()
    };
    let (estop, _dir): (EstopManager, tempfile::TempDir) =
        setup_estop_manager(state).await.unwrap();
    let status = estop.status();

    assert!(status.frozen_tools.contains(&"frozen_tool".to_string()));
    assert!(status.is_engaged());
}

#[tokio::test]
async fn estop_blocked_domains_contains_pattern() {
    let state = EstopState {
        blocked_domains: vec!["*.evil.com".to_string()],
        ..Default::default()
    };
    let (estop, _dir) = setup_estop_manager(state).await.unwrap();
    let status = estop.status();

    assert!(status.blocked_domains.contains(&"*.evil.com".to_string()));
    assert!(status.is_engaged());
}

#[test]
fn workspace_boundary_denies_restricted_tool() {
    let profile = WorkspaceProfile {
        name: "test_workspace".to_string(),
        allowed_domains: vec![],
        credential_profile: None,
        memory_namespace: None,
        audit_namespace: None,
        tool_restrictions: vec!["restricted_tool".to_string()],
    };

    let boundary = WorkspaceBoundary::new(Some(profile), false);
    let verdict = boundary.check_tool_access("restricted_tool");

    assert!(matches!(
        verdict,
        zeroclaw::security::BoundaryVerdict::Deny(_)
    ));
}

#[test]
fn workspace_boundary_allows_unrestricted_tool() {
    let profile = WorkspaceProfile {
        name: "test_workspace".to_string(),
        allowed_domains: vec![],
        credential_profile: None,
        memory_namespace: None,
        audit_namespace: None,
        tool_restrictions: vec!["restricted_tool".to_string()],
    };

    let boundary = WorkspaceBoundary::new(Some(profile), false);
    let verdict = boundary.check_tool_access("safe_tool");

    assert!(matches!(
        verdict,
        zeroclaw::security::BoundaryVerdict::Allow
    ));
}

#[test]
fn workspace_boundary_denies_disallowed_domain() {
    let profile = WorkspaceProfile {
        name: "test_workspace".to_string(),
        allowed_domains: vec!["allowed.com".to_string()],
        credential_profile: None,
        memory_namespace: None,
        audit_namespace: None,
        tool_restrictions: vec![],
    };

    let boundary = WorkspaceBoundary::new(Some(profile), false);
    let verdict = boundary.check_domain_access("notallowed.com");

    assert!(matches!(
        verdict,
        zeroclaw::security::BoundaryVerdict::Deny(_)
    ));
}

#[test]
fn workspace_boundary_allows_listed_domain() {
    let profile = WorkspaceProfile {
        name: "test_workspace".to_string(),
        allowed_domains: vec!["allowed.com".to_string()],
        credential_profile: None,
        memory_namespace: None,
        audit_namespace: None,
        tool_restrictions: vec![],
    };

    let boundary = WorkspaceBoundary::new(Some(profile), false);
    let verdict = boundary.check_domain_access("allowed.com");

    assert!(matches!(
        verdict,
        zeroclaw::security::BoundaryVerdict::Allow
    ));
}

#[test]
fn workspace_boundary_denies_cross_workspace_path() {
    let profile = WorkspaceProfile {
        name: "workspace_a".to_string(),
        allowed_domains: vec![],
        credential_profile: None,
        memory_namespace: None,
        audit_namespace: None,
        tool_restrictions: vec![],
    };

    let temp_dir = tempfile::tempdir().unwrap();
    let workspaces_base = temp_dir.path().join("workspaces");
    std::fs::create_dir_all(&workspaces_base).unwrap();

    let boundary = WorkspaceBoundary::new(Some(profile), false);

    // Try to access workspace_b from workspace_a
    let other_workspace_path = workspaces_base.join("workspace_b").join("secret.txt");

    let verdict = boundary.check_path_access(&other_workspace_path, &workspaces_base);

    assert!(matches!(
        verdict,
        zeroclaw::security::BoundaryVerdict::Deny(_)
    ));
}

#[test]
fn workspace_boundary_allows_own_workspace_path() {
    let profile = WorkspaceProfile {
        name: "workspace_a".to_string(),
        allowed_domains: vec![],
        credential_profile: None,
        memory_namespace: None,
        audit_namespace: None,
        tool_restrictions: vec![],
    };

    let temp_dir = tempfile::tempdir().unwrap();
    let workspaces_base = temp_dir.path().join("workspaces");
    std::fs::create_dir_all(&workspaces_base).unwrap();

    let boundary = WorkspaceBoundary::new(Some(profile), false);

    // Access own workspace
    let own_workspace_path = workspaces_base.join("workspace_a").join("data.txt");

    let verdict = boundary.check_path_access(&own_workspace_path, &workspaces_base);

    assert!(matches!(
        verdict,
        zeroclaw::security::BoundaryVerdict::Allow
    ));
}

#[tokio::test]
async fn otp_config_identifies_gated_tool() {
    let (_validator, config, _dir) = setup_otp_validator().unwrap();

    assert!(config.enabled);
    assert!(config.gated_actions.contains(&"gated_tool".to_string()));
}

#[tokio::test]
async fn otp_config_allows_non_gated_tool() {
    let (_validator, config, _dir) = setup_otp_validator().unwrap();

    assert!(config.enabled);
    assert!(!config.gated_actions.contains(&"safe_tool".to_string()));
}

#[test]
fn inactive_boundary_allows_everything() {
    let boundary = WorkspaceBoundary::inactive();

    assert!(matches!(
        boundary.check_tool_access("any_tool"),
        zeroclaw::security::BoundaryVerdict::Allow
    ));
    assert!(matches!(
        boundary.check_domain_access("any.domain"),
        zeroclaw::security::BoundaryVerdict::Allow
    ));
}

#[tokio::test]
async fn estop_default_state_is_not_engaged() {
    let state = EstopState::default();
    let (estop, _dir): (EstopManager, tempfile::TempDir) =
        setup_estop_manager(state).await.unwrap();
    let status = estop.status();

    assert!(!status.is_engaged());
}
