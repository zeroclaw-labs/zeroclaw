use crate::api_error::{ConfigApiCode, ConfigApiError};
use crate::schema::Config;
use crate::validation_warnings::{PRE_EXISTING_VALIDATION_ERROR, ValidationWarning};

const MAX_EDIT_BASELINE_DIAGNOSTICS: usize = 256;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticIdentity {
    code: ConfigApiCode,
    path: String,
    message: String,
}

impl From<&ConfigApiError> for DiagnosticIdentity {
    fn from(error: &ConfigApiError) -> Self {
        Self {
            code: error.code,
            path: error.path.clone().unwrap_or_default(),
            message: error.message.clone(),
        }
    }
}

pub(crate) struct ValidationPass<'a> {
    retained: &'a [DiagnosticIdentity],
    pub(crate) emit_notices: bool,
    stop_related: Vec<String>,
}

impl<'a> ValidationPass<'a> {
    pub(crate) fn strict() -> Self {
        Self::new(&[], true)
    }

    fn new(retained: &'a [DiagnosticIdentity], emit_notices: bool) -> Self {
        Self {
            retained,
            emit_notices,
            stop_related: Vec::new(),
        }
    }

    pub(crate) fn retains(&self, error: &ConfigApiError) -> bool {
        self.retained.contains(&DiagnosticIdentity::from(error))
    }

    pub(crate) fn helper(
        &mut self,
        result: anyhow::Result<()>,
        related: &[String],
    ) -> anyhow::Result<()> {
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                if error
                    .downcast_ref::<ConfigApiError>()
                    .is_some_and(|structured| self.retains(structured))
                {
                    return Ok(());
                }
                self.stop_related = related.to_vec();
                Err(error)
            }
        }
    }
}

macro_rules! retainable_bail {
    // A retained check must name its other inputs so their edits are not unrelated.
    ($pass:expr, $code:ident, $path:expr, related = $related:expr, $($msg:tt)*) => {{
        let err = $crate::api_error::ConfigApiError::new(
            $crate::api_error::ConfigApiCode::$code,
            format!($($msg)*),
        )
        .with_path($path);
        $pass.helper(::std::result::Result::Err(::anyhow::Error::from(err)), $related)?;
    }};
    ($pass:expr, $code:ident, $path:expr, $($msg:tt)*) => {{
        let err = $crate::api_error::ConfigApiError::new(
            $crate::api_error::ConfigApiCode::$code,
            format!($($msg)*),
        )
        .with_path($path);
        if !$pass.retains(&err) {
            return Err(::anyhow::Error::from(err));
        }
    }};
}
pub(crate) use retainable_bail;

fn paths_overlap(left: &str, right: &str) -> bool {
    fn is_path_prefix(prefix: &str, path: &str) -> bool {
        path.strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
    }

    left == right || is_path_prefix(left, right) || is_path_prefix(right, left)
}

fn enumerate_baseline_diagnostics(baseline: &Config) -> Vec<DiagnosticIdentity> {
    let mut retained = Vec::new();
    loop {
        let mut pass = ValidationPass::new(&retained, false);
        let Err(error) = baseline.validate_with(&mut pass) else {
            break;
        };
        let Some(structured) = error.downcast_ref::<ConfigApiError>() else {
            break;
        };
        let identity = DiagnosticIdentity::from(structured);
        if identity.path.is_empty()
            || retained.contains(&identity)
            || retained.len() == MAX_EDIT_BASELINE_DIAGNOSTICS
        {
            break;
        }
        retained.push(identity);
    }
    retained
}

impl Config {
    /// Validate an editor candidate against its published pre-edit baseline.
    ///
    /// Structured diagnostics may be retained as warnings only when their
    /// exact code, path, and message already occur in `baseline`, neither the
    /// diagnostic nor the checks skipped at its site, including every other
    /// input the retained check reads, overlap a dirty path, and validation can
    /// continue past that site. New, dirty-related, pathless,
    /// unstructured, repeated, and baseline-overflow diagnostics fail closed.
    /// Candidate and baseline values are never mutated or default-substituted.
    pub fn validate_edit_against(
        &self,
        baseline: &Config,
    ) -> Result<Vec<ValidationWarning>, ConfigApiError> {
        let mut retained = Vec::new();
        let mut baseline_diagnostics = None;
        let mut emit_notices = true;

        loop {
            let mut pass = ValidationPass::new(&retained, emit_notices);
            emit_notices = false;
            let Err(error) = self.validate_with(&mut pass) else {
                break;
            };
            let Some(structured) = error.downcast_ref::<ConfigApiError>() else {
                return Err(ConfigApiError::from_validation(error));
            };
            if structured.path.as_deref().is_none_or(str::is_empty) {
                return Err(ConfigApiError::from_validation(error));
            }
            let diagnostic = structured.clone();
            let identity = DiagnosticIdentity::from(structured);

            if retained.contains(&identity)
                || self
                    .dirty_paths
                    .iter()
                    .any(|dirty| paths_overlap(&identity.path, dirty))
                || pass.stop_related.iter().any(|related| {
                    self.dirty_paths
                        .iter()
                        .any(|dirty| paths_overlap(related, dirty))
                })
            {
                return Err(diagnostic);
            }

            let baseline_diagnostics = baseline_diagnostics
                .get_or_insert_with(|| enumerate_baseline_diagnostics(baseline));
            if !baseline_diagnostics.contains(&identity) {
                return Err(diagnostic);
            }
            retained.push(identity);
        }

        Ok(retained
            .into_iter()
            .map(|diagnostic| {
                ValidationWarning::new(
                    PRE_EXISTING_VALIDATION_ERROR,
                    diagnostic.message,
                    diagnostic.path,
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_error::ConfigApiCode;
    use crate::providers::{ChannelRef, ModelProviderRef};
    use crate::schema::{
        AliasedAgentConfig, LucidStorageConfig, OpenAIModelProviderConfig, PluginEntryConfig,
        RiskProfileConfig, SignalConfig,
    };

    fn validate_without_mutation(
        candidate: &Config,
        baseline: &Config,
    ) -> Result<Vec<ValidationWarning>, ConfigApiError> {
        let candidate_before = toml::to_string(candidate).expect("candidate serializes");
        let baseline_before = toml::to_string(baseline).expect("baseline serializes");
        let result = candidate.validate_edit_against(baseline);
        assert_eq!(
            toml::to_string(candidate).expect("candidate still serializes"),
            candidate_before,
            "edit validation must not mutate the candidate"
        );
        assert_eq!(
            toml::to_string(baseline).expect("baseline still serializes"),
            baseline_before,
            "edit validation must not mutate the baseline"
        );
        result
    }

    fn assert_error(
        result: Result<Vec<ValidationWarning>, ConfigApiError>,
        code: ConfigApiCode,
        path: Option<&str>,
    ) -> ConfigApiError {
        let error = result.expect_err("edit must be rejected");
        assert_eq!(error.code, code);
        assert_eq!(error.path.as_deref(), path);
        error
    }

    fn isolate_config_path(config: &mut Config) -> tempfile::TempDir {
        let tempdir = tempfile::tempdir().expect("temporary config directory");
        config.config_path = tempdir.path().join("config.toml");
        tempdir
    }

    fn config_with_valid_goal_verifier() -> Config {
        let mut config = Config::default();
        config
            .providers
            .models
            .openai
            .insert("default".to_string(), OpenAIModelProviderConfig::default());
        config.goal.verifier.model_provider = ModelProviderRef::from("openai.default");
        config
    }

    #[test]
    fn p1_goal_enable_introducing_missing_verifier_is_rejected() {
        let baseline = Config::default();
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("goal.enabled", "true")
            .expect("goal.enabled is an editable property");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::RequiredFieldEmpty,
            Some("goal.verifier.model_provider"),
        );
    }

    #[test]
    fn p3_goal_enable_with_resolvable_verifier_is_accepted() {
        let baseline = config_with_valid_goal_verifier();
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("goal.enabled", "true")
            .expect("goal.enabled is an editable property");

        let warnings = validate_without_mutation(&candidate, &baseline)
            .expect("a resolvable Goal verifier must remain valid");
        assert!(warnings.is_empty());
    }

    #[test]
    fn p5_later_dirty_error_is_not_masked_by_an_unrelated_baseline_error() {
        let mut baseline = Config::default();
        baseline.gateway.host.clear();
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("delegate.timeout_secs", "0")
            .expect("delegate.timeout_secs is an editable property");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidNumericRange,
            Some("delegate.timeout_secs"),
        );
    }

    #[test]
    fn p9_signal_helper_scope_blocks_a_dirty_sibling() {
        let mut baseline = Config::default();
        baseline.channels.signal.insert(
            "s".to_string(),
            SignalConfig {
                enabled: true,
                http_url: String::new(),
                account: "+15551234567".to_string(),
                ..SignalConfig::default()
            },
        );
        let mut candidate = baseline.clone();
        candidate
            .channels
            .signal
            .get_mut("s")
            .expect("Signal fixture exists")
            .account
            .clear();
        candidate.mark_dirty("channels.signal.s.account");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::RequiredFieldEmpty,
            Some("channels.signal.s.http_url"),
        );
    }

    #[test]
    fn p10_goal_helper_scope_blocks_a_dirty_goal_sibling() {
        let mut baseline = Config::default();
        baseline.goal.default_cost_limit_usd = Some(-1.0);
        let mut candidate = baseline.clone();
        candidate.goal.verifier.model = Some("  ".to_string());
        candidate.mark_dirty("goal.verifier.model");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidNumericRange,
            Some("goal.default_cost_limit_usd"),
        );
    }

    #[test]
    fn goal_scope_retains_existing_goal_error_for_unrelated_provider_edit() {
        // An unrelated provider edit retains an existing Goal verifier error.
        let mut baseline = config_with_valid_goal_verifier();
        baseline.goal.enabled = true;
        baseline.goal.verifier.model_provider = ModelProviderRef::from("openai.missing");
        let mut candidate = baseline.clone();
        candidate.mark_dirty("providers.models.openai.default.model");

        let warnings = validate_without_mutation(&candidate, &baseline)
            .expect("an unrelated provider edit must retain the baseline Goal error");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, PRE_EXISTING_VALIDATION_ERROR);
        assert_eq!(warnings[0].path, "goal.verifier.model_provider");
    }

    #[test]
    fn goal_scope_rejects_retention_when_the_verifier_profile_is_dirty() {
        // A dirty verifier profile must not retain an invalid Goal budget.
        let mut baseline = config_with_valid_goal_verifier();
        baseline.goal.enabled = true;
        baseline.goal.default_cost_limit_usd = Some(-1.0);
        let mut candidate = baseline.clone();
        candidate.mark_dirty("providers.models.openai.default.model");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidNumericRange,
            Some("goal.default_cost_limit_usd"),
        );
    }

    #[test]
    fn p11_unstructured_baseline_error_remains_fatal() {
        let mut baseline = Config::default();
        baseline.security.otp.token_ttl_secs = 100;
        baseline.security.otp.cache_valid_secs = 50;
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("gateway.websocket_ping_interval_secs", "45")
            .expect("gateway ping interval is an editable property");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::ValidationFailed,
            None,
        );
    }

    fn malformed_plugin_baseline() -> Config {
        let mut config = Config::default();
        config.plugins.entries.push(PluginEntryConfig {
            name: "p".to_string(),
            egress_hosts: vec!["https://api.example.com".to_string()],
            ..PluginEntryConfig::default()
        });
        config
    }

    #[test]
    fn p12_noncontinuable_plugin_host_error_blocks_a_dirty_dependent_sibling() {
        let baseline = malformed_plugin_baseline();
        let mut candidate = baseline.clone();
        candidate.plugins.entries[0]
            .egress_allow_private
            .push("127.0.0.1".to_string());
        candidate.mark_dirty("plugins.entries.p.egress_allow_private");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidFormat,
            Some("plugins.entries.p.egress_hosts"),
        );
    }

    #[test]
    fn p12_noncontinuable_plugin_host_error_blocks_an_unrelated_edit() {
        let baseline = malformed_plugin_baseline();
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("gateway.websocket_ping_interval_secs", "45")
            .expect("gateway ping interval is an editable property");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidFormat,
            Some("plugins.entries.p.egress_hosts"),
        );
    }

    #[test]
    fn p13_new_cross_path_typed_memory_error_is_rejected() {
        let mut baseline = Config::default();
        baseline.memory.types.enabled = true;
        baseline.memory.backend = "sqlite".to_string();
        let mut candidate = baseline.clone();
        candidate.memory.backend = "none".to_string();
        candidate.mark_dirty("memory.backend");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidFormat,
            Some("memory.types.enabled"),
        );
    }

    #[test]
    fn p14_array_element_diagnostic_overlaps_its_dirty_parent() {
        let mut baseline = Config::default();
        baseline
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        baseline
            .providers
            .models
            .openai
            .insert("default".to_string(), OpenAIModelProviderConfig::default());
        baseline.agents.insert(
            "a".to_string(),
            AliasedAgentConfig {
                channels: vec![ChannelRef::from("telegram.missing")],
                model_provider: ModelProviderRef::from("openai.default"),
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let mut candidate = baseline.clone();
        candidate.mark_dirty("agents.a.channels");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::DanglingReference,
            Some("agents.a.channels[0]"),
        );
    }

    #[test]
    fn p16_valid_edit_has_no_retained_validation_warning() {
        let baseline = Config::default();
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("gateway.websocket_ping_interval_secs", "45")
            .expect("gateway ping interval is an editable property");

        let warnings = validate_without_mutation(&candidate, &baseline)
            .expect("a valid edit against a valid baseline must pass");
        assert!(warnings.is_empty());
    }

    #[test]
    fn p23_strict_validation_preserves_the_first_error_exactly() {
        let mut config = Config::default();
        config.gateway.host.clear();
        config.delegate.timeout_secs = 0;
        let before = toml::to_string(&config).expect("config serializes");

        let error = config
            .validate()
            .expect_err("strict validation must retain first-error behavior");
        assert_eq!(
            error.to_string(),
            "[required_field_empty] gateway.host must not be empty (gateway.host)"
        );
        assert_eq!(format!("{error:#}"), error.to_string());
        let structured = error
            .downcast_ref::<ConfigApiError>()
            .expect("gateway.host remains a structured validation error");
        assert_eq!(structured.code, ConfigApiCode::RequiredFieldEmpty);
        assert_eq!(structured.path.as_deref(), Some("gateway.host"));
        assert_eq!(
            toml::to_string(&config).expect("config still serializes"),
            before,
            "strict validation must not mutate the config"
        );
    }

    #[test]
    fn p27_diagnostic_beyond_the_baseline_bound_fails_closed() {
        let mut baseline = Config::default();
        for index in 0..300 {
            baseline.storage.lucid.insert(
                format!("{index:03}"),
                LucidStorageConfig {
                    binary_path: Some(String::new()),
                    ..LucidStorageConfig::default()
                },
            );
        }
        let mut candidate = baseline.clone();
        let _config_tempdir = isolate_config_path(&mut candidate);
        candidate
            .set_prop_persistent("gateway.websocket_ping_interval_secs", "45")
            .expect("gateway ping interval is an editable property");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::RequiredFieldEmpty,
            Some("storage.lucid.256.binary_path"),
        );
    }

    #[test]
    fn cross_field_input_edit_rejects_retained_mdns_ttl_error() {
        // The announce interval is another input of the unchanged TTL diagnostic.
        let mut baseline = Config::default();
        baseline.nodes.mdns.peer_ttl_secs = 30;
        baseline.nodes.mdns.announce_interval_secs = 60;
        let mut candidate = baseline.clone();
        candidate.nodes.mdns.announce_interval_secs = 120;
        candidate.mark_dirty("nodes.mdns.announce_interval_secs");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidNumericRange,
            Some("nodes.mdns.peer_ttl_secs"),
        );
    }

    #[test]
    fn unrelated_edit_retains_mdns_ttl_error_as_warning() {
        // A scheduler edit does not participate in the failing TTL relation.
        let mut baseline = Config::default();
        baseline.nodes.mdns.peer_ttl_secs = 30;
        baseline.nodes.mdns.announce_interval_secs = 60;
        let mut candidate = baseline.clone();
        candidate.scheduler.max_tasks = baseline.scheduler.max_tasks + 1;
        candidate.mark_dirty("scheduler.max_tasks");

        let warnings = validate_without_mutation(&candidate, &baseline)
            .expect("unrelated edit retains the existing TTL diagnostic");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, PRE_EXISTING_VALIDATION_ERROR);
        assert_eq!(warnings[0].path, "nodes.mdns.peer_ttl_secs");
    }

    #[test]
    fn repairing_mdns_announce_interval_saves_without_warning() {
        // Repairing the related input removes the baseline error completely.
        let mut baseline = Config::default();
        baseline.nodes.mdns.peer_ttl_secs = 30;
        baseline.nodes.mdns.announce_interval_secs = 60;
        let mut candidate = baseline.clone();
        candidate.nodes.mdns.announce_interval_secs = 20;
        candidate.mark_dirty("nodes.mdns.announce_interval_secs");

        let warnings = validate_without_mutation(&candidate, &baseline)
            .expect("a repaired TTL relation must validate");
        assert!(warnings.is_empty());
    }

    #[test]
    fn final_state_mdns_repair_across_both_fields_saves() {
        // A multi-field edit is checked as one valid final state, not intermediate states.
        let mut baseline = Config::default();
        baseline.nodes.mdns.peer_ttl_secs = 30;
        baseline.nodes.mdns.announce_interval_secs = 60;
        let mut candidate = baseline.clone();
        candidate.nodes.mdns.announce_interval_secs = 120;
        candidate.nodes.mdns.peer_ttl_secs = 240;
        candidate.mark_dirty("nodes.mdns.announce_interval_secs");
        candidate.mark_dirty("nodes.mdns.peer_ttl_secs");

        let warnings = validate_without_mutation(&candidate, &baseline)
            .expect("the valid final TTL relation must validate");
        assert!(warnings.is_empty());
    }

    #[test]
    fn typed_memory_flag_edit_rejects_retained_agent_backend_error() {
        // Both global typed-memory flags participate in the agent-backend diagnostic.
        let mut baseline = config_with_valid_goal_verifier();
        baseline.memory.backend = "sqlite".to_string();
        baseline.memory.types.enabled = true;
        baseline.memory.consolidation_extract_facts = false;
        baseline
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        let mut agent = AliasedAgentConfig {
            model_provider: ModelProviderRef::from("openai.default"),
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        };
        agent.memory.backend = crate::multi_agent::MemoryBackendKind::Markdown;
        baseline.agents.insert("a".to_string(), agent);
        let mut candidate = baseline.clone();
        candidate.memory.consolidation_extract_facts = true;
        candidate.mark_dirty("memory.consolidation_extract_facts");

        assert_error(
            validate_without_mutation(&candidate, &baseline),
            ConfigApiCode::InvalidFormat,
            Some("agents.a.memory.backend"),
        );

        let mut unrelated = baseline.clone();
        unrelated.scheduler.max_tasks = baseline.scheduler.max_tasks + 1;
        unrelated.mark_dirty("scheduler.max_tasks");
        let warnings = validate_without_mutation(&unrelated, &baseline)
            .expect("an unrelated scheduler edit retains the agent-backend error");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, PRE_EXISTING_VALIDATION_ERROR);
        assert_eq!(warnings[0].path, "agents.a.memory.backend");
    }

    #[test]
    fn strict_validation_keeps_mdns_ttl_diagnostic_identity() {
        // Adding related metadata must not alter strict validation's original diagnostic.
        let mut config = Config::default();
        config.nodes.mdns.peer_ttl_secs = 30;
        config.nodes.mdns.announce_interval_secs = 60;

        let error = config.validate().expect_err("the TTL relation is invalid");
        let structured = error
            .downcast_ref::<ConfigApiError>()
            .expect("the TTL diagnostic remains structured");
        assert_eq!(structured.code, ConfigApiCode::InvalidNumericRange);
        assert_eq!(structured.path.as_deref(), Some("nodes.mdns.peer_ttl_secs"));
        assert_eq!(
            structured.message,
            "nodes.mdns.peer_ttl_secs must be greater than nodes.mdns.announce_interval_secs"
        );
    }
}
