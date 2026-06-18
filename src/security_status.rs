use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::BTreeMap;
use zeroclaw_config::config::CredentialSurfaceClass;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{RiskProfileConfig, SandboxBackend, SandboxConfig};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecurityStatusReport {
    pub source: String,
    pub agent: String,
    pub agent_enabled: bool,
    pub risk_profile: RiskProfileStatus,
    pub sandbox: SandboxStatus,
    pub workspace: WorkspaceStatus,
    pub credentials: CredentialStatus,
    pub gateway: GatewayStatus,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RiskProfileStatus {
    pub alias: String,
    pub level: String,
    pub require_approval_for_medium_risk: bool,
    pub block_high_risk_commands: bool,
    pub auto_approve_count: usize,
    pub always_ask_count: usize,
    pub allowed_tools_count: usize,
    pub excluded_tools_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SandboxStatus {
    pub requested_enabled: Option<bool>,
    pub requested_backend: String,
    pub active_backend: String,
    pub active_description: String,
    pub fallback: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceStatus {
    pub workspace_dir: String,
    pub workspace_only: bool,
    pub read_write_roots_count: usize,
    pub read_only_roots_count: usize,
    pub write_only_roots_count: usize,
    pub shell_env_passthrough_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CredentialStatus {
    pub encryption_enabled: bool,
    pub secret_fields_total: usize,
    pub secret_fields_set: usize,
    pub classified_fields_total: usize,
    pub classification_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GatewayStatus {
    pub host: String,
    pub port: u16,
    pub require_pairing: bool,
    pub allow_public_bind: bool,
    pub tls_enabled: bool,
}

pub fn build_report(config: &Config, agent_alias: &str) -> Result<SecurityStatusReport> {
    let resolved = resolve_agent_context(config, agent_alias)?;
    let sandbox_config = sandbox_config_from_policy(&resolved.policy);
    let sandbox = zeroclaw_runtime::security::sandbox_posture(
        &sandbox_config,
        config.runtime.kind.as_wire(),
        Some(&resolved.policy.workspace_dir),
    );

    let secret_fields = config.secret_fields();
    let prop_fields = config.prop_fields();
    let classification_counts = credential_classification_counts(&prop_fields);
    let classified_fields_total = classification_counts.values().sum();

    let mut warnings = Vec::new();
    if !resolved.agent_enabled {
        warnings.push(crate::t(
            "cli-security-status-warning-agent-disabled",
            "agent is disabled",
        ));
    }
    if sandbox_config.enabled == Some(false) {
        warnings.push(crate::t(
            "cli-security-status-warning-sandbox-disabled",
            "sandboxing is disabled for this agent risk profile",
        ));
    }
    if sandbox.active_backend == "none" {
        warnings.push(crate::t(
            "cli-security-status-warning-sandbox-none",
            "active sandbox is application-layer only",
        ));
    }
    if sandbox.fallback {
        warnings.push(crate::ta(
            "cli-security-status-warning-sandbox-fallback",
            &[
                ("requested", sandbox.requested_backend),
                ("active", sandbox.active_backend),
            ],
            "requested sandbox backend fell back",
        ));
    }
    if !resolved.policy.workspace_only {
        warnings.push(crate::t(
            "cli-security-status-warning-workspace-not-restricted",
            "workspace-only filesystem policy is disabled",
        ));
    }
    if !resolved.policy.shell_env_passthrough.is_empty() {
        let count = resolved.policy.shell_env_passthrough.len().to_string();
        warnings.push(crate::ta(
            "cli-security-status-warning-shell-env-passthrough",
            &[("count", &count)],
            "shell environment variables are passed through",
        ));
    }
    if !config.secrets.encrypt {
        warnings.push(crate::t(
            "cli-security-status-warning-secrets-unencrypted",
            "config secret encryption is disabled",
        ));
    }
    if classification_counts
        .get("requires_follow_up")
        .is_some_and(|count| *count > 0)
    {
        warnings.push(crate::t(
            "cli-security-status-warning-credential-follow-up",
            "some credential-shaped config surfaces still require follow-up",
        ));
    }
    if !config.gateway.require_pairing {
        warnings.push(crate::t(
            "cli-security-status-warning-pairing-disabled",
            "gateway pairing is not required",
        ));
    }
    let tls_enabled = config.gateway.tls.as_ref().is_some_and(|tls| tls.enabled);
    if config.gateway.allow_public_bind && !tls_enabled {
        warnings.push(crate::t(
            "cli-security-status-warning-public-bind-no-tls",
            "gateway allows public bind without TLS enabled",
        ));
    }

    Ok(SecurityStatusReport {
        source: format!("agents.{agent_alias}.risk_profile"),
        agent: agent_alias.to_string(),
        agent_enabled: resolved.agent_enabled,
        risk_profile: RiskProfileStatus {
            alias: resolved.profile_alias,
            level: autonomy_level_name(resolved.policy.autonomy).to_string(),
            require_approval_for_medium_risk: resolved.policy.require_approval_for_medium_risk,
            block_high_risk_commands: resolved.policy.block_high_risk_commands,
            auto_approve_count: resolved.policy.auto_approve.len(),
            always_ask_count: resolved.policy.always_ask.len(),
            allowed_tools_count: resolved.policy.allowed_tools.as_ref().map_or(0, Vec::len),
            excluded_tools_count: resolved.policy.excluded_tools.as_ref().map_or(0, Vec::len),
        },
        sandbox: SandboxStatus {
            requested_enabled: sandbox_config.enabled,
            requested_backend: sandbox.requested_backend.to_string(),
            active_backend: sandbox.active_backend.to_string(),
            active_description: sandbox.active_description.to_string(),
            fallback: sandbox.fallback,
        },
        workspace: WorkspaceStatus {
            workspace_dir: resolved.policy.workspace_dir.display().to_string(),
            workspace_only: resolved.policy.workspace_only,
            read_write_roots_count: resolved.policy.allowed_roots.len(),
            read_only_roots_count: resolved.policy.allowed_roots_read_only.len(),
            write_only_roots_count: resolved.policy.allowed_roots_write_only.len(),
            shell_env_passthrough_count: resolved.policy.shell_env_passthrough.len(),
        },
        credentials: CredentialStatus {
            encryption_enabled: config.secrets.encrypt,
            secret_fields_total: secret_fields.len(),
            secret_fields_set: secret_fields.iter().filter(|field| field.is_set).count(),
            classified_fields_total,
            classification_counts,
        },
        gateway: GatewayStatus {
            host: config.gateway.host.clone(),
            port: config.gateway.port,
            require_pairing: config.gateway.require_pairing,
            allow_public_bind: config.gateway.allow_public_bind,
            tls_enabled,
        },
        warnings,
    })
}

pub fn print_report(report: &SecurityStatusReport) {
    println!(
        "{}",
        crate::t("cli-security-status-title", "ZeroClaw Security Status")
    );
    println!(
        "{}",
        crate::ta(
            "cli-security-status-source",
            &[("v", &report.source)],
            "Source"
        )
    );
    println!(
        "{}",
        crate::ta(
            "cli-security-status-agent",
            &[("v", &report.agent)],
            "Agent"
        )
    );
    let agent_enabled = report.agent_enabled.to_string();
    println!(
        "{}",
        crate::ta(
            "cli-security-status-agent-enabled",
            &[("enabled", &agent_enabled)],
            "Agent enabled"
        )
    );
    println!(
        "{}",
        crate::ta(
            "cli-security-status-risk-profile",
            &[("v", &report.risk_profile.alias)],
            "Risk profile"
        )
    );
    println!(
        "{}",
        crate::ta(
            "cli-security-status-autonomy",
            &[("v", &report.risk_profile.level)],
            "Autonomy"
        )
    );
    let medium = report
        .risk_profile
        .require_approval_for_medium_risk
        .to_string();
    let high = report.risk_profile.block_high_risk_commands.to_string();
    println!(
        "{}",
        crate::ta(
            "cli-security-status-approvals",
            &[("medium", &medium), ("high", &high)],
            "Approvals"
        )
    );
    println!(
        "{}",
        crate::ta(
            "cli-security-status-sandbox",
            &[
                ("requested", &report.sandbox.requested_backend),
                ("active", &report.sandbox.active_backend),
                ("description", &report.sandbox.active_description),
            ],
            "Sandbox"
        )
    );
    let workspace_only = report.workspace.workspace_only.to_string();
    let read_write_roots = report.workspace.read_write_roots_count.to_string();
    let read_only_roots = report.workspace.read_only_roots_count.to_string();
    let write_only_roots = report.workspace.write_only_roots_count.to_string();
    let env_passthrough = report.workspace.shell_env_passthrough_count.to_string();
    println!(
        "{}",
        crate::ta(
            "cli-security-status-workspace",
            &[
                ("dir", &report.workspace.workspace_dir),
                ("workspace_only", &workspace_only),
                ("read_write_roots", &read_write_roots),
                ("read_only_roots", &read_only_roots),
                ("write_only_roots", &write_only_roots),
                ("env_passthrough", &env_passthrough),
            ],
            "Workspace"
        )
    );
    let encryption = report.credentials.encryption_enabled.to_string();
    let secrets_set = report.credentials.secret_fields_set.to_string();
    let secrets_total = report.credentials.secret_fields_total.to_string();
    let classified_total = report.credentials.classified_fields_total.to_string();
    let classification_summary = credential_classification_summary(&report.credentials);
    println!(
        "{}",
        crate::ta(
            "cli-security-status-credentials",
            &[
                ("encryption", &encryption),
                ("secrets_set", &secrets_set),
                ("secrets_total", &secrets_total),
                ("classified_total", &classified_total),
                ("classification_summary", &classification_summary),
            ],
            "Credentials"
        )
    );
    let gateway_port = report.gateway.port.to_string();
    let gateway_pairing = report.gateway.require_pairing.to_string();
    let gateway_public_bind = report.gateway.allow_public_bind.to_string();
    let gateway_tls = report.gateway.tls_enabled.to_string();
    println!(
        "{}",
        crate::ta(
            "cli-security-status-gateway",
            &[
                ("host", &report.gateway.host),
                ("port", &gateway_port),
                ("pairing", &gateway_pairing),
                ("public_bind", &gateway_public_bind),
                ("tls", &gateway_tls),
            ],
            "Gateway"
        )
    );
    if report.warnings.is_empty() {
        println!(
            "{}",
            crate::t("cli-security-status-warnings-none", "Warnings: none")
        );
    } else {
        println!(
            "{}",
            crate::ta(
                "cli-security-status-warnings",
                &[("v", &report.warnings.join("; "))],
                "Warnings"
            )
        );
    }
}

struct ResolvedAgentContext<'a> {
    profile_alias: String,
    _risk_profile: &'a RiskProfileConfig,
    agent_enabled: bool,
    policy: SecurityPolicy,
}

fn resolve_agent_context<'a>(
    config: &'a Config,
    agent_alias: &str,
) -> Result<ResolvedAgentContext<'a>> {
    let agent_config = config
        .agents
        .get(agent_alias)
        .with_context(|| format!("agents.{agent_alias} is not configured"))?;
    let profile_alias = agent_config.risk_profile.trim();
    if profile_alias.is_empty() {
        bail!("agents.{agent_alias}.risk_profile is empty");
    }
    let risk_profile = config.risk_profiles.get(profile_alias).with_context(|| {
        format!("agents.{agent_alias}.risk_profile names missing risk_profiles.{profile_alias}")
    })?;
    let policy = SecurityPolicy::for_agent(config, agent_alias)?;

    Ok(ResolvedAgentContext {
        profile_alias: profile_alias.to_string(),
        _risk_profile: risk_profile,
        agent_enabled: agent_config.enabled,
        policy,
    })
}

fn sandbox_config_from_policy(policy: &SecurityPolicy) -> SandboxConfig {
    SandboxConfig {
        enabled: policy.sandbox_enabled,
        backend: policy
            .sandbox_backend
            .as_deref()
            .map(str::trim)
            .filter(|backend| !backend.is_empty())
            .map(parse_sandbox_backend)
            .unwrap_or_default(),
        firejail_args: policy.firejail_args.clone(),
    }
}

fn parse_sandbox_backend(name: &str) -> SandboxBackend {
    match name.to_ascii_lowercase().as_str() {
        "landlock" => SandboxBackend::Landlock,
        "firejail" => SandboxBackend::Firejail,
        "bubblewrap" => SandboxBackend::Bubblewrap,
        "docker" => SandboxBackend::Docker,
        "sandbox-exec" | "sandboxexec" | "seatbelt" => SandboxBackend::SandboxExec,
        "none" => SandboxBackend::None,
        _ => SandboxBackend::Auto,
    }
}

fn credential_classification_counts(
    fields: &[zeroclaw_config::config::PropFieldInfo],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for field in fields {
        if let Some(class) = field.credential_class {
            *counts
                .entry(credential_class_name(class).to_string())
                .or_insert(0) += 1;
        }
    }
    counts
}

fn credential_classification_summary(credentials: &CredentialStatus) -> String {
    if credentials.classification_counts.is_empty() {
        return crate::t("cli-security-status-credentials-classes-none", "none");
    }

    credentials
        .classification_counts
        .iter()
        .map(|(class, count)| format!("{class}: {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn credential_class_name(class: CredentialSurfaceClass) -> &'static str {
    match class {
        CredentialSurfaceClass::EncryptedSecret => "encrypted_secret",
        CredentialSurfaceClass::PathOnlyReference => "path_only_reference",
        CredentialSurfaceClass::PublicValue => "public_value",
        CredentialSurfaceClass::ExternalAuthStore => "external_auth_store",
        CredentialSurfaceClass::LegacyEnvPath => "legacy_env_path",
        CredentialSurfaceClass::RequiresFollowUp => "requires_follow_up",
    }
}

fn autonomy_level_name(level: zeroclaw_config::autonomy::AutonomyLevel) -> &'static str {
    match level {
        zeroclaw_config::autonomy::AutonomyLevel::ReadOnly => "read-only",
        zeroclaw_config::autonomy::AutonomyLevel::Supervised => "supervised",
        zeroclaw_config::autonomy::AutonomyLevel::Full => "full",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_agent(agent: &str, profile_alias: &str, profile: RiskProfileConfig) -> Config {
        let mut config = Config::default();
        config
            .risk_profiles
            .insert(profile_alias.to_string(), profile);
        config.agents.insert(
            agent.to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: profile_alias.into(),
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn agent_report_uses_effective_policy_without_leaking_secret_names() {
        let mut config = config_with_agent("ops", "ops-risk", RiskProfileConfig::default());
        config.secrets.encrypt = true;

        let report = build_report(&config, "ops").expect("agent report");

        assert_eq!(report.source, "agents.ops.risk_profile");
        assert_eq!(report.agent, "ops");
        assert!(report.agent_enabled);
        assert_eq!(report.risk_profile.alias, "ops-risk");
        assert_eq!(report.risk_profile.level, "supervised");
        assert!(report.workspace.workspace_only);
        assert_eq!(
            report.workspace.workspace_dir,
            config.agent_workspace_dir("ops").display().to_string()
        );
        assert!(report.gateway.require_pairing);
        assert!(report.credentials.encryption_enabled);

        let json = serde_json::to_string(&report).expect("json");
        assert!(!json.contains("api_key"));
        assert!(!json.contains("access_token"));
        assert!(!json.contains("bot_token"));
    }

    #[test]
    fn full_autonomy_reports_effective_workspace_override() {
        let profile = RiskProfileConfig {
            level: zeroclaw_config::autonomy::AutonomyLevel::Full,
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        let config = config_with_agent("ops", "full-risk", profile);

        let report = build_report(&config, "ops").expect("agent report");

        assert_eq!(report.risk_profile.level, "full");
        assert!(!report.workspace.workspace_only);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("workspace-only"))
        );
    }

    #[test]
    fn workspace_access_reports_effective_root_tiers() {
        use zeroclaw_config::multi_agent::{AccessMode, AgentAlias};

        let mut config = config_with_agent("ops", "ops-risk", RiskProfileConfig::default());
        config.agents.insert(
            "docs".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: "ops-risk".into(),
                ..Default::default()
            },
        );
        config.agents.insert(
            "writer".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: "ops-risk".into(),
                ..Default::default()
            },
        );
        let ops = config.agents.get_mut("ops").expect("ops agent");
        ops.workspace
            .access
            .insert(AgentAlias::from("docs"), AccessMode::Read);
        ops.workspace
            .access
            .insert(AgentAlias::from("writer"), AccessMode::Write);

        let report = build_report(&config, "ops").expect("agent report");

        assert_eq!(report.workspace.read_only_roots_count, 2);
        assert_eq!(report.workspace.write_only_roots_count, 1);
        assert_eq!(report.workspace.read_write_roots_count, 0);
    }

    #[test]
    fn disabled_agent_is_reported_and_warned() {
        let mut config = config_with_agent("ops", "ops-risk", RiskProfileConfig::default());
        config.agents.get_mut("ops").expect("ops agent").enabled = false;

        let report = build_report(&config, "ops").expect("disabled agent report");

        assert!(!report.agent_enabled);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("agent is disabled"))
        );
    }

    #[test]
    fn agent_report_flags_security_gaps() {
        let mut profile = RiskProfileConfig {
            workspace_only: false,
            sandbox_enabled: Some(false),
            shell_env_passthrough: vec!["TOKEN_PATH".to_string()],
            ..RiskProfileConfig::default()
        };
        profile.auto_approve.push("shell".to_string());
        let mut config = config_with_agent("ops", "ops-risk", profile);
        config.gateway.require_pairing = false;
        config.gateway.allow_public_bind = true;

        let report = build_report(&config, "ops").expect("agent report");

        assert_eq!(report.sandbox.requested_enabled, Some(false));
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("sandboxing is disabled"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("workspace-only"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("gateway pairing"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("public bind without TLS"))
        );
    }

    #[test]
    fn missing_agent_is_an_error() {
        let config = Config::default();
        let err = build_report(&config, "missing").expect_err("missing agent should error");
        assert!(err.to_string().contains("agents.missing"));
    }
}
