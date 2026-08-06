//! Quickstart apply path.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use zeroclaw_config::helpers::kebab_to_snake;
use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
    SelectorChoice, recommended_runtime_preset, risk_preset, runtime_preset,
};
use zeroclaw_config::schema::{Config, WireApi};

/// Canonical config state for await-spanning Quickstart writes in one daemon.
///
/// Web and RPC/TUI must use the same instance: both persist credentials and
/// config while awaiting I/O, so independent snapshots or mutexes can let a
/// later successful submission erase an earlier one.
#[derive(Clone)]
pub struct QuickstartConfigState {
    config: Arc<RwLock<Config>>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Set after a successful supervised Quickstart commit and before the
    /// handler schedules its delayed daemon reload. It closes the interval in
    /// which a queued second Web/RPC apply could otherwise commit only to be
    /// interrupted by the first submission's reload.
    reload_admitted: Arc<AtomicBool>,
}

impl QuickstartConfigState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            reload_admitted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Reconstruct shared state from the daemon-owned config and transaction lock.
    ///
    /// Callers must pass the paired handles for the same daemon instance. This
    /// permits boundary adapters such as Web and RPC/TUI to share one complete
    /// Quickstart transaction without holding the synchronous config lock over
    /// persistence awaits.
    pub fn from_parts(
        config: Arc<RwLock<Config>>,
        write_lock: Arc<tokio::sync::Mutex<()>>,
        reload_admitted: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            write_lock,
            reload_admitted,
        }
    }

    pub fn config(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    pub fn write_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.write_lock)
    }

    /// Daemon-owned reload admission shared by every Quickstart adapter.
    pub fn reload_admission(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.reload_admitted)
    }

    /// Apply and commit one Quickstart submission while owning the shared
    /// config transaction. The lock is acquired before cloning and held until
    /// the persisted working copy replaces the shared live state.
    pub async fn apply(
        &self,
        submission: BuilderSubmission,
        surface: Surface,
    ) -> Result<QuickstartApplyOutcome, Vec<QuickstartError>> {
        self.apply_inner(submission, surface, false).await
    }

    /// Apply a submission that will immediately request a daemon reload.
    ///
    /// The pending-reload admission is set while the complete transaction is
    /// still locked. A later queued Web/RPC apply therefore fails cleanly
    /// instead of persisting a change the scheduled reload would interrupt.
    pub async fn apply_and_admit_reload(
        &self,
        submission: BuilderSubmission,
        surface: Surface,
    ) -> Result<QuickstartApplyOutcome, Vec<QuickstartError>> {
        self.apply_inner(submission, surface, true).await
    }

    /// Release an admission when the caller cannot schedule the reload (for
    /// example, a standalone gateway or test harness). A successful
    /// supervised reload intentionally leaves the admission set because the
    /// daemon process will be replaced.
    pub fn cancel_reload_admission(&self) {
        self.reload_admitted.store(false, Ordering::Release);
    }

    async fn apply_inner(
        &self,
        submission: BuilderSubmission,
        surface: Surface,
        admit_reload: bool,
    ) -> Result<QuickstartApplyOutcome, Vec<QuickstartError>> {
        let _transaction_guard = Arc::clone(&self.write_lock).lock_owned().await;
        if admit_reload && self.reload_admitted.load(Ordering::Acquire) {
            return Err(vec![QuickstartError::for_surface(
                None,
                QuickstartStep::Agent,
                "reload",
                "Quickstart is waiting for the daemon to reload after a previous successful submission",
                "cli-quickstart-error-reload-pending",
                &[],
            )]);
        }
        let mut working = self.config.read().clone();
        let outcome = apply_with_surface_outcome(submission, &mut working, surface).await?;
        *self.config.write() = working;
        if admit_reload {
            self.reload_admitted.store(true, Ordering::Release);
        }
        Ok(outcome)
    }
}

/// Store an Anthropic setup token in the profile bound to its provider alias.
///
/// The setup-token field is transport-only: an OAuth-mode model entry never
/// persists it in `config.toml`. Every Quickstart surface must use this helper
/// so the alias that selects `auth_mode = "oauth"` is also the profile name.
pub async fn store_anthropic_setup_token(
    config: &Config,
    alias: &str,
    token: &str,
) -> anyhow::Result<()> {
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("Anthropic setup token cannot be empty");
    }

    let kind =
        zeroclaw_providers::auth::anthropic_token::detect_auth_kind(token, Some("authorization"));
    let metadata = std::collections::HashMap::from([(
        "auth_kind".to_string(),
        kind.as_metadata_value().to_string(),
    )]);
    zeroclaw_providers::auth::AuthService::from_config(config)
        // An OAuth alias selects its own stored profile. It must not become
        // Anthropic's global active profile as a side effect of onboarding.
        .store_model_provider_token("anthropic", alias, token, metadata, false)
        .await?;
    Ok(())
}

/// Which surface invoked the Quickstart. Stamped on every event in
/// the apply path so SSE/dashboard consumers can filter by origin
/// without parsing message strings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Web,
    Tui,
    Cli,
    Test,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Web => "web",
            Surface::Tui => "tui",
            Surface::Cli => "cli",
            Surface::Test => "test",
        }
    }
}

/// Per-run attribution carried through the apply path so every emitted
/// event lands with the same correlation id. Constructed by `apply`
/// and `validate_only`; threaded down into `apply_into` and the
/// per-selector helpers via `&RunCtx`.
struct RunCtx {
    run_id: String,
    surface: Surface,
}

impl RunCtx {
    fn new(surface: Surface) -> Self {
        // Fall back to nanosecond timestamp if a system without a clock
        // is somehow in play. Either way the id is unique per process.
        let run_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| format!("{:x}{:x}", d.as_secs(), d.subsec_nanos()))
            .unwrap_or_else(|_| format!("{:x}", std::process::id()));
        Self { run_id, surface }
    }

    fn base_attrs(&self) -> serde_json::Value {
        serde_json::json!({
            "quickstart.run_id": self.run_id,
            "quickstart.surface": self.surface.as_str(),
        })
    }
}

/// Layer per-event attrs on top of the run-scoped base. Both must be
/// JSON objects; non-object inputs return `base` unchanged.
fn merge_attrs(base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    let (mut base_map, extra_map) = match (base, extra) {
        (serde_json::Value::Object(b), serde_json::Value::Object(e)) => (b, e),
        (b, _) => return b,
    };
    for (k, v) in extra_map {
        base_map.insert(k, v);
    }
    serde_json::Value::Object(base_map)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedAgent {
    pub alias: String,
    pub model_provider: String,
    pub risk_profile: String,
    pub runtime_profile: String,
    pub channels: Vec<String>,
    pub memory_backend: String,
}

/// A non-fatal issue discovered after the Quickstart configuration committed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartWarning {
    pub step: QuickstartStep,
    pub field: String,
    pub message: String,
}

impl QuickstartWarning {
    fn for_surface(
        ctx: Option<&RunCtx>,
        step: QuickstartStep,
        field: impl Into<String>,
        fallback: impl Into<String>,
        key: &str,
        args: &[(&str, &str)],
    ) -> Self {
        let fallback = fallback.into();
        let message = if matches!(ctx.map(|ctx| ctx.surface), Some(Surface::Cli)) {
            crate::i18n::get_required_cli_string_with_args(key, args)
        } else {
            fallback
        };
        Self {
            step,
            field: field.into(),
            message,
        }
    }
}

/// Durable Quickstart result, including non-fatal post-commit warnings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartApplyOutcome {
    pub agent: AppliedAgent,
    pub warnings: Vec<QuickstartWarning>,
}

type StagedAnthropicProfile = (
    zeroclaw_providers::auth::AuthService,
    String,
    zeroclaw_providers::auth::profiles::ProfileBindingSnapshot,
    zeroclaw_providers::auth::profiles::AuthProfile,
    zeroclaw_providers::auth::profiles::ProfileSaveOutcome,
);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartStep {
    ModelProvider,
    RiskProfile,
    RuntimeProfile,
    Memory,
    Channels,
    PeerGroups,
    Agent,
}

impl QuickstartStep {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ModelProvider => "Model provider",
            Self::RiskProfile => "Risk profile",
            Self::RuntimeProfile => "Runtime profile",
            Self::Memory => "Memory",
            Self::Channels => "Channels",
            Self::PeerGroups => "Peer groups",
            Self::Agent => "Agent",
        }
    }

    #[must_use]
    pub fn label_key(self) -> &'static str {
        match self {
            Self::ModelProvider => "cli-quickstart-step-model-provider",
            Self::RiskProfile => "cli-quickstart-step-risk-profile",
            Self::RuntimeProfile => "cli-quickstart-step-runtime-profile",
            Self::Memory => "cli-quickstart-step-memory",
            Self::Channels => "cli-quickstart-step-channels",
            Self::PeerGroups => "cli-quickstart-step-peer-groups",
            Self::Agent => "cli-quickstart-step-agent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartError {
    pub step: QuickstartStep,
    pub field: String,
    pub message: String,
}

impl QuickstartError {
    fn new(step: QuickstartStep, field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            step,
            field: field.into(),
            message: message.into(),
        }
    }

    fn for_surface(
        ctx: Option<&RunCtx>,
        step: QuickstartStep,
        field: impl Into<String>,
        fallback: impl Into<String>,
        key: &str,
        args: &[(&str, &str)],
    ) -> Self {
        let fallback = fallback.into();
        if !matches!(ctx.map(|ctx| ctx.surface), Some(Surface::Cli)) {
            return Self::new(step, field, fallback);
        }

        Self::new(
            step,
            field,
            crate::i18n::get_required_cli_string_with_args(key, args),
        )
    }
}

impl std::fmt::Display for QuickstartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.field.is_empty() {
            write!(f, "{:?}: {}", self.step, self.message)
        } else {
            write!(f, "{:?}.{}: {}", self.step, self.field, self.message)
        }
    }
}

pub fn validate_only(
    submission: &BuilderSubmission,
    config: &Config,
) -> Result<(), Vec<QuickstartError>> {
    validate_only_with_surface(submission, config, Surface::Web)
}

pub fn validate_only_with_surface(
    submission: &BuilderSubmission,
    config: &Config,
    surface: Surface,
) -> Result<(), Vec<QuickstartError>> {
    let ctx = RunCtx::new(surface);
    let mut staged = config.clone();
    let mut errors = Vec::new();
    // validate-only never commits; staged tempfiles drop at scope exit.
    let mut staged_files = Vec::new();
    apply_into(
        &mut staged,
        submission,
        &mut staged_files,
        &mut errors,
        Some(&ctx),
    );
    let ok = errors.is_empty();
    let attrs = merge_attrs(
        ctx.base_attrs(),
        serde_json::json!({"error_count": errors.len()}),
    );
    let outcome = if ok {
        ::zeroclaw_log::EventOutcome::Success
    } else {
        ::zeroclaw_log::EventOutcome::Failure
    };
    if ok {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate)
                .with_outcome(outcome)
                .with_attrs(attrs),
            "quickstart: validate_only"
        );
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate)
                .with_outcome(outcome)
                .with_attrs(attrs),
            "quickstart: validate_only"
        );
    }
    if ok { Ok(()) } else { Err(errors) }
}

pub async fn apply(
    submission: BuilderSubmission,
    config: &mut Config,
) -> Result<AppliedAgent, Vec<QuickstartError>> {
    apply_with_surface(submission, config, Surface::Web).await
}

pub async fn apply_with_surface(
    submission: BuilderSubmission,
    config: &mut Config,
    surface: Surface,
) -> Result<AppliedAgent, Vec<QuickstartError>> {
    Ok(apply_with_surface_outcome(submission, config, surface)
        .await?
        .agent)
}

/// Apply a Quickstart submission and retain any non-fatal post-commit
/// warnings for the calling user surface.
pub async fn apply_with_surface_outcome(
    submission: BuilderSubmission,
    config: &mut Config,
    surface: Surface,
) -> Result<QuickstartApplyOutcome, Vec<QuickstartError>> {
    let ctx = RunCtx::new(surface);
    let started = std::time::Instant::now();
    let setup_token = anthropic_setup_token_from_submission(&submission);
    // Callers retain this object after an error (notably the CLI path). Keep
    // the in-memory state aligned with the on-disk rollback contract.
    let original_config = config.clone();

    if matches!(surface, Surface::Web | Surface::Tui)
        && setup_token.is_some_and(|(_, token)| token.is_empty())
    {
        return Err(vec![QuickstartError::for_surface(
            Some(&ctx),
            QuickstartStep::ModelProvider,
            "api_key",
            "Anthropic setup-token authentication requires a setup token",
            "cli-quickstart-error-anthropic-setup-token-required",
            &[],
        )]);
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
            .with_attrs(ctx.base_attrs()),
        "quickstart: apply"
    );

    let mut errors = Vec::new();
    let mut staged_files = Vec::new();
    let applied = apply_into(
        config,
        &submission,
        &mut staged_files,
        &mut errors,
        Some(&ctx),
    );
    if !errors.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "error_count": errors.len(),
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                    }),
                )),
            "quickstart: apply rejected"
        );
        *config = original_config;
        return Err(errors);
    }
    let applied = match applied {
        Some(applied) => applied,
        None => {
            *config = original_config;
            return Err(vec![QuickstartError::for_surface(
                Some(&ctx),
                QuickstartStep::Agent,
                "apply",
                "internal error: apply_into returned no result despite no validation errors",
                "cli-quickstart-error-internal-no-result",
                &[],
            )]);
        }
    };

    config
        .set_prop_persistent("onboard_state.quickstart_completed", "true")
        .map_err(|err| {
            *config = original_config.clone();
            vec![QuickstartError::for_surface(
                Some(&ctx),
                QuickstartStep::Agent,
                "",
                format!("failed to flip quickstart-completed: {err}"),
                "cli-quickstart-error-completion-flag",
                &[("err", &err.to_string())],
            )]
        })?;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({"flag": "quickstart_completed"}),
            )
        ),
        "quickstart: completion flag flipped"
    );

    // Stage the transport-only token immediately before the config commit.
    // If the latter fails, compensate the exact profile binding rather than
    // leaving an OAuth alias's credential behind. The snapshot is limited to
    // the same profile binding; active-profile selection is deliberately not
    // part of this transaction.
    let staged_profile =
        if let Some((alias, token)) = setup_token.filter(|(_, token)| !token.is_empty()) {
            let auth_service = zeroclaw_providers::auth::AuthService::from_config(config);
            let staged = match auth_service
                .stage_model_provider_token(
                    "anthropic",
                    alias,
                    token,
                    std::collections::HashMap::from([(
                        "auth_kind".to_string(),
                        zeroclaw_providers::auth::anthropic_token::detect_auth_kind(
                            token,
                            Some("authorization"),
                        )
                        .as_metadata_value()
                        .to_string(),
                    )]),
                )
                .await
            {
                Ok(staged) => staged,
                Err(err) => {
                    *config = original_config.clone();
                    return Err(vec![QuickstartError::for_surface(
                        Some(&ctx),
                        QuickstartStep::ModelProvider,
                        "api_key",
                        format!("failed to store Anthropic setup token: {err}"),
                        "cli-quickstart-error-anthropic-setup-token-store",
                        &[],
                    )]);
                }
            };
            Some((
                auth_service,
                alias.to_string(),
                staged.snapshot,
                staged.staged_profile,
                staged.save_outcome,
            ))
        } else {
            None
        };

    let dirty_count = config.dirty_paths.len();
    let write_started = std::time::Instant::now();
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write).with_attrs(
            merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({"dirty_path_count": dirty_count}),
            )
        ),
        "quickstart: persist start"
    );
    let write_result = config.save_dirty_with_outcome().await;
    let write_ms = write_started.elapsed().as_millis() as u64;
    match &write_result {
        Ok(zeroclaw_config::schema::ConfigSaveOutcome::Durable) => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "dirty_path_count": dirty_count,
                        "elapsed_ms": write_ms,
                    }),
                )),
            "quickstart: persist complete"
        ),
        Ok(zeroclaw_config::schema::ConfigSaveOutcome::CommittedWithDurabilityWarning(err)) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(merge_attrs(
                        ctx.base_attrs(),
                        serde_json::json!({
                            "dirty_path_count": dirty_count,
                            "elapsed_ms": write_ms,
                            "error": err.to_string(),
                            "committed": true,
                        }),
                    )),
                "quickstart: persist committed with durability warning"
            );
        }
        Err(err) => ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "dirty_path_count": dirty_count,
                        "elapsed_ms": write_ms,
                        "error": err.to_string(),
                    }),
                )),
            "quickstart: persist failed"
        ),
    }
    let mut warnings =
        reconcile_config_save_outcome(write_result, staged_profile, config, original_config, &ctx)
            .await?;

    // Config landed atomically — now move the staged personality files
    // into place. Any failure here is reported but does not unwind the
    // already-persisted config; the agent is valid without them.
    commit_personality_files(staged_files, &mut warnings, Some(&ctx));

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({
                    "agent": applied.alias,
                    "channels": applied.channels.len(),
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                }),
            )),
        "quickstart: apply complete"
    );
    Ok(QuickstartApplyOutcome {
        agent: applied,
        warnings,
    })
}

async fn reconcile_config_save_outcome(
    write_result: anyhow::Result<zeroclaw_config::schema::ConfigSaveOutcome>,
    staged_profile: Option<StagedAnthropicProfile>,
    config: &mut Config,
    original_config: Config,
    ctx: &RunCtx,
) -> Result<Vec<QuickstartWarning>, Vec<QuickstartError>> {
    let mut warnings = Vec::new();
    if let Some((
        _,
        _,
        _,
        _,
        zeroclaw_providers::auth::profiles::ProfileSaveOutcome::CommittedWithDurabilityWarning(err),
    )) = staged_profile.as_ref()
    {
        let detail = err.to_string();
        warnings.push(QuickstartWarning::for_surface(
            Some(ctx),
            QuickstartStep::ModelProvider,
            "api_key",
            format!(
                "Quickstart completed, but Anthropic credential durability could not be confirmed: {detail}"
            ),
            "cli-quickstart-warning-anthropic-profile-durability",
            &[("err", &detail)],
        ));
    }
    match write_result {
        Ok(zeroclaw_config::schema::ConfigSaveOutcome::Durable) => Ok(warnings),
        Ok(zeroclaw_config::schema::ConfigSaveOutcome::CommittedWithDurabilityWarning(err)) => {
            let detail = err.to_string();
            warnings.push(QuickstartWarning::for_surface(
                Some(ctx),
                QuickstartStep::Agent,
                "config",
                format!(
                    "Quickstart completed, but config durability could not be confirmed: {detail}"
                ),
                "cli-quickstart-warning-config-durability",
                &[("err", &detail)],
            ));
            Ok(warnings)
        }
        Err(err) => {
            let rollback_error = if let Some((auth_service, alias, snapshot, expected_current, _)) =
                staged_profile
            {
                auth_service
                    .restore_model_provider_profile(
                        "anthropic",
                        &alias,
                        snapshot,
                        &expected_current,
                    )
                    .await
                    .err()
            } else {
                None
            };
            *config = original_config;
            let detail = match rollback_error {
                Some(rollback_error) => {
                    format!("{err}; Anthropic setup-token rollback also failed: {rollback_error}")
                }
                None => err.to_string(),
            };
            Err(vec![QuickstartError::for_surface(
                Some(ctx),
                QuickstartStep::Agent,
                "",
                detail.clone(),
                "cli-quickstart-error-persist-config",
                &[("err", &detail)],
            )])
        }
    }
}

pub fn record_dismissed(run_id: &str, surface: Surface, last_step: Option<QuickstartStep>) {
    let last_step_str = last_step
        .map(|s| match s {
            QuickstartStep::ModelProvider => "model_provider",
            QuickstartStep::RiskProfile => "risk_profile",
            QuickstartStep::RuntimeProfile => "runtime_profile",
            QuickstartStep::Memory => "memory",
            QuickstartStep::Channels => "channels",
            QuickstartStep::PeerGroups => "peer_groups",
            QuickstartStep::Agent => "agent",
        })
        .unwrap_or("none");
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "quickstart.run_id": run_id,
                "quickstart.surface": surface.as_str(),
                "last_step": last_step_str,
                "dismissed": true,
            })),
        "quickstart: dismissed"
    );
}

/// `onboard_state.quickstart_completed` is false **and** no
/// `agents.*` entries exist. Returning users with existing agents
/// never see the auto-trigger even if the flag was never flipped.
pub fn should_auto_launch(config: &Config) -> bool {
    !config.onboard_state.quickstart_completed && config.agents.is_empty()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartState {
    pub quickstart_completed: bool,
    pub agents: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    /// Canonical runtime fallback used when a provider has no recommendation.
    pub default_runtime_profile: String,
    /// `<provider_type>.<alias>` refs for every configured model provider.
    pub model_providers: Vec<String>,
    /// `<channel_type>.<alias>` refs.
    pub channels: Vec<String>,
    #[serde(default)]
    pub unassigned_channels: Vec<String>,
    /// `<storage_type>.<alias>` refs.
    pub storage: Vec<String>,
    pub model_provider_types: Vec<QuickstartTypeOption>,
    pub channel_types: Vec<QuickstartTypeOption>,
    /// Risk presets from `zeroclaw_config::presets::RISK_PRESETS`.
    pub risk_presets: &'static [zeroclaw_config::presets::RiskPreset],
    /// Runtime presets from `zeroclaw_config::presets::RUNTIME_PRESETS`.
    pub runtime_presets: &'static [zeroclaw_config::presets::RuntimePreset],
    /// Memory backend snake-case kinds from `MemoryBackendKind`.
    pub memory_kinds: Vec<String>,
    /// Canonical personality filenames the Quickstart will accept.
    /// Surfaces iterate this; never hardcode the filename list.
    pub personality_files: &'static [&'static str],
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartTypeOption {
    /// Canonical identifier (e.g. `"anthropic"`, `"telegram"`).
    pub kind: String,
    /// Human-readable picker label (e.g. `"Anthropic"`, `"Telegram"`).
    pub display_name: String,
    /// `true` when the entry runs locally and needs no remote
    /// credential. Channels always report `false`; providers reflect
    /// their `local` flag from `ModelProviderInfo`.
    pub local: bool,
    /// Runtime preset the daemon recommends when this provider is selected.
    /// Resolved from provider registry metadata and the canonical preset table
    /// for each state snapshot; never persisted as config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_runtime_profile: Option<String>,
}

/// Resolve a Quickstart provider-type input to the canonical config family.
///
/// The provider registry remains the source of truth for real config families.
/// Auth-provider aliases (`openai-codex`, `claude`, `google`, `grok`, ...)
/// are accepted as Quickstart conveniences and normalize to existing config
/// families. Only `openai-codex` preselects a different Quickstart auth mode.
#[must_use]
pub fn resolve_model_provider_type(type_key: &str) -> Option<(&'static str, bool)> {
    let trimmed = type_key.trim();
    if let Some(info) = zeroclaw_providers::list_model_providers()
        .into_iter()
        .find(|info| info.name.eq_ignore_ascii_case(trimmed))
    {
        return Some((info.name, false));
    }

    let provider = trimmed
        .parse::<zeroclaw_providers::auth::AuthProvider>()
        .ok()?;
    Some(match provider {
        zeroclaw_providers::auth::AuthProvider::OpenaiCodex => ("openai", true),
        zeroclaw_providers::auth::AuthProvider::Anthropic => ("anthropic", false),
        zeroclaw_providers::auth::AuthProvider::Gemini => ("gemini", false),
        zeroclaw_providers::auth::AuthProvider::Xai => ("xai", false),
    })
}

/// Build a [`QuickstartState`] snapshot from the live config.
///
/// The two `*_types` lists are populated from the canonical sources
/// (`zeroclaw_providers::list_model_providers()` for providers,
/// `cfg.channels.channels()` for channel kinds). Adding a new entry in
/// either source automatically lights up here — no Quickstart code
/// change required. This is the DRY contract the plan calls out under
/// "Reads the per-provider field map at render time so adding a
/// provider in the schema doesn't require Quickstart code changes."
pub fn snapshot_state(cfg: &Config) -> QuickstartState {
    let model_provider_types = zeroclaw_providers::list_model_providers()
        .into_iter()
        .map(|info| QuickstartTypeOption {
            kind: info.name.to_string(),
            display_name: info.display_name.to_string(),
            local: info.local,
            default_runtime_profile: zeroclaw_providers::recommended_runtime_profile(info.name)
                .and_then(runtime_preset)
                .map(|preset| preset.preset_name.to_string()),
        })
        .collect();
    let channel_types = build_channel_type_options(&cfg.channels);
    QuickstartState {
        quickstart_completed: cfg.onboard_state.quickstart_completed,
        agents: cfg.agents.keys().cloned().collect(),
        risk_profiles: cfg.risk_profiles.keys().cloned().collect(),
        runtime_profiles: cfg.runtime_profiles.keys().cloned().collect(),
        default_runtime_profile: recommended_runtime_preset(None)
            .map(|preset| preset.preset_name.to_string())
            .unwrap_or_default(),
        model_providers: cfg
            .providers
            .models
            .iter_entries()
            .map(|(family, alias, _)| format!("{family}.{alias}"))
            .collect(),
        channels: collect_aliased_refs(&cfg.channels),
        unassigned_channels: collect_aliased_refs(&cfg.channels)
            .into_iter()
            .filter(|ch| cfg.agent_for_channel(ch).is_none())
            .collect(),
        storage: collect_aliased_refs(&cfg.storage),
        model_provider_types,
        channel_types,
        risk_presets: zeroclaw_config::presets::RISK_PRESETS,
        runtime_presets: zeroclaw_config::presets::RUNTIME_PRESETS,
        memory_kinds: memory_kind_keys(),
        personality_files: crate::agent::personality::EDITABLE_PERSONALITY_FILES,
    }
}

/// Snake-case wire keys for every `MemoryBackendKind` variant. Exhaustive
/// match probe catches missing variants at compile time; serde produces
/// the wire key so there's no parallel mapping.
fn memory_kind_keys() -> Vec<String> {
    use zeroclaw_config::multi_agent::MemoryBackendKind as M;
    [
        M::Sqlite,
        M::Markdown,
        M::Postgres,
        M::Qdrant,
        M::Lucid,
        M::None,
    ]
    .into_iter()
    .map(|k| {
        // Exhaustiveness guard: adding a new variant forces this match to fail
        // to compile until the contributor decides whether the new backend
        // belongs in the quickstart picker.
        match k {
            M::Sqlite | M::Markdown | M::Postgres | M::Qdrant | M::Lucid | M::None => (),
        }
        serde_json::to_value(k)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    })
    .collect()
}

fn build_channel_type_options(
    channels_cfg: &zeroclaw_config::schema::ChannelsConfig,
) -> Vec<QuickstartTypeOption> {
    channels_cfg
        .channels()
        .into_iter()
        .map(|info| QuickstartTypeOption {
            kind: info.kind.to_string(),
            display_name: info.name.to_string(),
            local: false,
            default_runtime_profile: None,
        })
        .collect()
}

fn resolve_channel_quickstart_type(channel_type: &str) -> (&str, bool) {
    match channel_type {
        "whatsapp-web" | "whatsapp_web" => ("whatsapp", true),
        other => (other, false),
    }
}

fn canonical_quickstart_channel_ref(reference: &str) -> Option<String> {
    let (channel_type, alias) = split_ref(reference.trim())?;
    let (config_channel_type, _) = resolve_channel_quickstart_type(channel_type);
    Some(format!("{config_channel_type}.{alias}"))
}

fn default_whatsapp_web_session_path(config: &Config, alias: &str) -> String {
    config
        .config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("state")
        .join("whatsapp-web")
        .join(format!("{alias}.db"))
        .to_string_lossy()
        .into_owned()
}

/// Walk the serialised form of `value` and yield `<type>.<alias>` refs
/// for every `HashMap<String, _>`-shaped subsection. Schema-driven —
/// adding a new channel or storage slot in the schema lights up here
/// for free, no code change required.
fn collect_aliased_refs<T: serde::Serialize>(value: &T) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(serde_json::Value::Object(map)) = serde_json::to_value(value) else {
        return out;
    };
    for (family, subvalue) in map {
        if let serde_json::Value::Object(entries) = subvalue {
            for alias in entries.keys() {
                out.push(format!("{family}.{alias}"));
            }
        }
    }
    out.sort();
    out
}

/// Selector kinds that the Quickstart "field shape" descriptor
/// covers. The TUI / web ask the runtime for the shape, then render
/// inputs dumbly off the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSection {
    ModelProvider,
    Channel,
    PeerGroup,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FieldDescriptor {
    /// Schema-side field key (kebab-case terminal segment). The
    /// caller submits this back through [`BuilderSubmission`].
    pub key: String,
    /// Human label shown next to the input.
    pub label: String,
    /// One-line help blurb. Empty when the schema field has no doc.
    pub help: String,
    /// Wire-tag for the input control to render. Mirrors
    /// `PropKind::wire_name`.
    pub kind: zeroclaw_config::traits::PropKind,
    /// `true` for `#[secret]` fields — the modal masks input.
    pub is_secret: bool,
    /// Closed-set choices for `Enum` kind. `None` for everything else.
    pub enum_variants: Option<Vec<String>>,
    /// `true` when Quickstart treats this field as required. Currently
    /// every field returned by [`field_shape`] is required, but the
    /// flag is exposed so future additions can include optional rows.
    pub required: bool,
    /// Pre-filled default the modal should show as ghost text /
    /// initial input value. `None` when the schema has no meaningful
    /// default for this field (e.g. API keys, bot tokens).
    pub default: Option<String>,
}

/// Return the renderable field shape for a single section + type
/// combination. Walks `prop_fields()` against a synthetic config with
/// one default-instantiated entry under the requested type, then
/// filters to the per-section "essential" allowlist.
pub fn field_shape(section: FieldSection, type_key: &str) -> Vec<FieldDescriptor> {
    const SYNTHETIC_ALIAS: &str = "qs0probe";
    let (section_path, essentials, codex_auth_preselected) = match section {
        FieldSection::ModelProvider => {
            let Some((provider_type, codex_auth_preselected)) =
                resolve_model_provider_type(type_key)
            else {
                return Vec::new();
            };
            (
                format!("providers.models.{provider_type}"),
                MODEL_PROVIDER_ESSENTIALS,
                codex_auth_preselected,
            )
        }
        FieldSection::Channel => {
            let essentials = if type_key == "webhook" {
                WEBHOOK_CHANNEL_ESSENTIALS
            } else {
                CHANNEL_ESSENTIALS
            };
            (format!("channels.{type_key}"), essentials, false)
        }
        FieldSection::PeerGroup => ("peer_groups".to_string(), PEER_GROUP_ESSENTIALS, false),
    };

    // A throwaway Config we can mutate freely. Inject one default
    // entry under the requested type so `prop_fields()` enumerates
    // its leaves.
    let mut probe = Config::default();
    if probe
        .create_map_key(&section_path, SYNTHETIC_ALIAS)
        .is_err()
    {
        return Vec::new();
    }
    let leaf_prefix = format!("{section_path}.{SYNTHETIC_ALIAS}.");
    // OpenAI and Anthropic translate their UI-only choices into their
    // respective persisted authentication settings. Do not also expose a
    // schema-level `auth_mode` row: it would duplicate the selector and can
    // advertise a persisted value that Quickstart does not accept as input.
    let has_custom_auth_mode_selector = matches!(section, FieldSection::ModelProvider)
        && section_path
            .strip_prefix("providers.models.")
            .is_some_and(|provider_type| auth_modes_for(provider_type).is_some());

    let mut out = Vec::new();
    for info in probe.prop_fields() {
        let Some(field_path) = info.name.strip_prefix(&leaf_prefix) else {
            continue;
        };
        if !essentials.contains(&field_path)
            || (has_custom_auth_mode_selector && field_path == QUICKSTART_AUTH_MODE_FIELD)
        {
            continue;
        }
        let default = if section_path == "channels.webhook" && field_path == "port" {
            Some(zeroclaw_config::schema::DEFAULT_WEBHOOK_CHANNEL_PORT.to_string())
        } else if info.is_secret {
            None
        } else {
            let raw = info.display_value.trim();
            if raw.is_empty() || raw == zeroclaw_config::traits::UNSET_DISPLAY {
                None
            } else {
                Some(raw.to_string())
            }
        };
        let help = if section_path == "providers.models.anthropic" && field_path == "api_key" {
            crate::i18n::get_required_cli_string("cli-quickstart-anthropic-api-key-help")
        } else {
            info.description.trim().to_string()
        };
        out.push(FieldDescriptor {
            key: field_path.to_string(),
            label: kebab_to_snake(field_path),
            help,
            kind: info.kind,
            is_secret: info.is_secret,
            enum_variants: info.enum_variants.map(|f| f()),
            // `uri` is an override-only field — operators set it only
            // when pointing at a self-hosted gateway. `api_key` is left
            // non-required because local providers (Ollama) and Codex
            // subscription auth don't need one — the runtime surfaces a clear
            // error at request time if a remote provider is missing its key.
            // Everything else in the essentials list is required to actually
            // issue a request.
            required: !matches!(field_path, "uri" | "api_key"),
            default,
        });
    }
    if matches!(section, FieldSection::ModelProvider)
        && let Some(provider_type) = section_path.strip_prefix("providers.models.")
        && let Some(descriptor) = auth_mode_descriptor(provider_type, codex_auth_preselected)
    {
        out.push(descriptor);
    }
    out.sort_by_key(|d| {
        essentials
            .iter()
            .position(|k| *k == d.key.as_str())
            .unwrap_or(usize::MAX)
    });
    out
}

/// Essentials per section kind. Kept in one place so adding a
/// provider type or channel kind lights up Quickstart for free,
/// while keeping the modal focused on what an agent cannot start
/// without.
const MODEL_PROVIDER_ESSENTIALS: &[&str] = &["model", QUICKSTART_AUTH_MODE_FIELD, "api_key", "uri"];
const CHANNEL_ESSENTIALS: &[&str] = &["bot_token", "token", "webhook_url", "allowed_users"];
const WEBHOOK_CHANNEL_ESSENTIALS: &[&str] = &[
    "bot_token",
    "token",
    "webhook_url",
    "allowed_users",
    "port",
    "secret",
];
const PEER_GROUP_ESSENTIALS: &[&str] = &["channel", "external_peers", "agents", "ignore"];

const QUICKSTART_AUTH_MODE_FIELD: &str = "auth_mode";
const QUICKSTART_AUTH_MODE_API_KEY: &str = "api_key";
const QUICKSTART_AUTH_MODE_CODEX: &str = "codex";
const QUICKSTART_AUTH_MODE_SETUP_TOKEN: &str = "setup_token";
const QUICKSTART_OPENAI_AUTH_MODES: &[&str] =
    &[QUICKSTART_AUTH_MODE_API_KEY, QUICKSTART_AUTH_MODE_CODEX];
const QUICKSTART_ANTHROPIC_AUTH_MODES: &[&str] = &[
    QUICKSTART_AUTH_MODE_API_KEY,
    QUICKSTART_AUTH_MODE_SETUP_TOKEN,
];

fn anthropic_setup_token_from_submission(submission: &BuilderSubmission) -> Option<(&str, &str)> {
    let SelectorChoice::Fresh(choice) = &submission.model_provider else {
        return None;
    };
    let (provider_type, _) = resolve_model_provider_type(&choice.provider_type)?;
    if provider_type != "anthropic"
        || !choice
            .fields
            .get(QUICKSTART_AUTH_MODE_FIELD)
            .is_some_and(|mode| {
                mode.trim()
                    .eq_ignore_ascii_case(QUICKSTART_AUTH_MODE_SETUP_TOKEN)
            })
    {
        return None;
    }
    Some((
        choice.alias.as_str(),
        choice.fields.get("api_key").map_or("", String::as_str),
    ))
}

fn auth_modes_for(provider_type: &str) -> Option<&'static [&'static str]> {
    match provider_type {
        "openai" => Some(QUICKSTART_OPENAI_AUTH_MODES),
        "anthropic" => Some(QUICKSTART_ANTHROPIC_AUTH_MODES),
        _ => None,
    }
}

fn auth_mode_descriptor(
    provider_type: &str,
    codex_auth_preselected: bool,
) -> Option<FieldDescriptor> {
    let modes = auth_modes_for(provider_type)?;
    let (label_key, help_key) = match provider_type {
        "openai" => (
            "cli-quickstart-openai-auth-mode-label",
            "cli-quickstart-openai-auth-mode-help",
        ),
        "anthropic" => (
            "cli-quickstart-anthropic-auth-mode-label",
            "cli-quickstart-anthropic-auth-mode-help",
        ),
        _ => return None,
    };
    let default = if provider_type == "openai" && codex_auth_preselected {
        QUICKSTART_AUTH_MODE_CODEX
    } else {
        QUICKSTART_AUTH_MODE_API_KEY
    };
    Some(FieldDescriptor {
        key: QUICKSTART_AUTH_MODE_FIELD.to_string(),
        label: crate::i18n::get_required_cli_string(label_key),
        help: crate::i18n::get_required_cli_string(help_key),
        kind: zeroclaw_config::traits::PropKind::Enum,
        is_secret: false,
        enum_variants: Some(modes.iter().map(|mode| (*mode).to_string()).collect()),
        required: true,
        default: Some(default.to_string()),
    })
}

fn apply_into(
    config: &mut Config,
    submission: &BuilderSubmission,
    staged_files: &mut Vec<StagedPersonalityWrite>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<AppliedAgent> {
    if !validate_runtime_profile_choice(config, &submission.runtime_profile, errors, ctx) {
        return None;
    }
    let provider_ref = apply_model_provider(config, &submission.model_provider, errors, ctx)?;
    emit_selector_pick(
        ctx,
        "model_provider",
        selector_mode(&submission.model_provider),
        &provider_ref,
    );

    let risk_alias = apply_named_preset(
        config,
        &submission.risk_profile,
        QuickstartStep::RiskProfile,
        risk_preset_keys,
        write_risk_preset,
        errors,
        ctx,
    )?;
    emit_selector_pick(
        ctx,
        "risk_profile",
        selector_mode(&submission.risk_profile),
        &risk_alias,
    );

    let runtime_alias = apply_named_preset(
        config,
        &submission.runtime_profile,
        QuickstartStep::RuntimeProfile,
        runtime_preset_keys,
        write_runtime_preset,
        errors,
        ctx,
    )?;
    emit_selector_pick(
        ctx,
        "runtime_profile",
        selector_mode(&submission.runtime_profile),
        &runtime_alias,
    );

    let memory_backend = apply_memory(config, &submission.memory, errors, ctx)?;
    emit_selector_pick(
        ctx,
        "memory",
        selector_mode(&submission.memory),
        &memory_backend,
    );

    let channel_refs = apply_channels(config, &submission.channels, errors, ctx);
    if let Some(ctx) = ctx {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "selector": "channels",
                        "count": channel_refs.len(),
                    }),
                )
            ),
            "quickstart: selector channels"
        );
    }

    if !errors.is_empty() {
        return None;
    }
    let alias = apply_agent(
        config,
        &submission.agent,
        &provider_ref,
        &risk_alias,
        &runtime_alias,
        &channel_refs,
        errors,
        ctx,
    )?;
    emit_selector_pick(ctx, "agent", "create_new", &alias);

    let peer_group_refs =
        apply_peer_groups(config, &submission.peer_groups, &channel_refs, errors, ctx);
    if let Some(ctx) = ctx {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "selector": "peer_groups",
                        "count": peer_group_refs.len(),
                    }),
                )
            ),
            "quickstart: selector peer_groups"
        );
    }

    apply_personality_files(
        config,
        &alias,
        &submission.agent.personality_files,
        staged_files,
        errors,
        ctx,
    );

    materialize_default_skills_bundle(config);

    if !errors.is_empty() {
        return None;
    }

    Some(AppliedAgent {
        alias,
        model_provider: provider_ref,
        risk_profile: risk_alias,
        runtime_profile: runtime_alias,
        channels: channel_refs,
        memory_backend,
    })
}

fn validate_runtime_profile_choice(
    config: &Config,
    choice: &SelectorChoice<String>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> bool {
    let message = match choice {
        SelectorChoice::Existing(alias) if !config.runtime_profiles.contains_key(alias) => {
            Some(QuickstartError::for_surface(
                ctx,
                QuickstartStep::RuntimeProfile,
                "",
                format!("no `{alias}` profile configured"),
                "cli-quickstart-error-no-profile",
                &[("alias", alias)],
            ))
        }
        SelectorChoice::Fresh(preset_name) if runtime_preset(preset_name).is_none() => {
            Some(QuickstartError::new(
                QuickstartStep::RuntimeProfile,
                "",
                format!("unknown runtime preset `{preset_name}`"),
            ))
        }
        _ => None,
    };
    if let Some(error) = message {
        errors.push(error);
        false
    } else {
        true
    }
}

/// Surface representation of a selector's submission mode for
/// observability. We never inspect the wrapped value here — only
/// whether the user picked an existing alias or created fresh.
fn selector_mode<T>(choice: &SelectorChoice<T>) -> &'static str {
    match choice {
        SelectorChoice::Existing(_) => "use_existing",
        SelectorChoice::Fresh(_) => "create_new",
    }
}

fn emit_selector_pick(ctx: Option<&RunCtx>, selector: &str, mode: &str, value: &str) {
    let Some(ctx) = ctx else { return };
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({
                    "selector": selector,
                    "mode": mode,
                    "value": value,
                }),
            )
        ),
        "quickstart: selector pick"
    );
}

// ── Model provider ─────────────────────────────────────────────────

fn apply_model_provider(
    config: &mut Config,
    choice: &SelectorChoice<ModelProviderChoice>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String> {
    match choice {
        SelectorChoice::Existing(reference) => {
            let (family, alias) = match split_ref(reference) {
                Some(parts) => parts,
                None => {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::ModelProvider,
                        "",
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                        "cli-quickstart-error-not-type-alias-ref",
                        &[("reference", reference)],
                    ));
                    return None;
                }
            };
            if !section_has_alias(config, "providers.models", family, alias) {
                let path = format!("providers.models.{family}.{alias}");
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "",
                    format!("no `{path}` configured"),
                    "cli-quickstart-error-no-configured-path",
                    &[("path", &path)],
                ));
                return None;
            }
            Some(reference.clone())
        }
        SelectorChoice::Fresh(choice) => {
            if choice.provider_type.trim().is_empty()
                || choice.alias.trim().is_empty()
                || choice.model.trim().is_empty()
            {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "",
                    "provider type, alias, and model are required",
                    "cli-quickstart-error-provider-required",
                    &[],
                ));
                return None;
            }
            // Canonicalize the provider type against the registry. The picker
            // offers canonical `info.name` keys, but a hand-typed or
            // whitespace-padded value (e.g. "llamacpp ", "llama.cpp") would
            // otherwise reach `create_map_key` verbatim and fail with a cryptic
            // "no map-keyed/list section" because the family key doesn't match.
            // `openai-codex` is accepted as a convenience input alias and
            // normalizes to the existing `openai` config family.
            let Some((provider_type, codex_alias_requested)) =
                resolve_model_provider_type(&choice.provider_type)
            else {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "provider_type",
                    format!(
                        "unknown model provider type `{}` — pick one from the provider list",
                        choice.provider_type.trim()
                    ),
                    "cli-quickstart-error-unknown-provider-type",
                    &[("provider", choice.provider_type.trim())],
                ));
                return None;
            };
            let default_auth_mode = if codex_alias_requested {
                QUICKSTART_AUTH_MODE_CODEX
            } else {
                QUICKSTART_AUTH_MODE_API_KEY
            };
            let auth_mode = choice
                .fields
                .get(QUICKSTART_AUTH_MODE_FIELD)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default_auth_mode.to_string());
            if let Some(allowed_modes) = auth_modes_for(provider_type)
                && !allowed_modes.contains(&auth_mode.as_str())
            {
                let (provider_name, error_key) = if provider_type == "openai" {
                    ("OpenAI", "cli-quickstart-error-unknown-openai-auth-mode")
                } else {
                    (
                        "Anthropic",
                        "cli-quickstart-error-unknown-anthropic-auth-mode",
                    )
                };
                let expected = allowed_modes
                    .iter()
                    .map(|mode| format!("`{mode}`"))
                    .collect::<Vec<_>>()
                    .join(" or ");
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    QUICKSTART_AUTH_MODE_FIELD,
                    format!("unknown {provider_name} auth mode `{auth_mode}` — pick {expected}"),
                    error_key,
                    &[("mode", &auth_mode)],
                ));
                return None;
            }
            if section_has_alias(config, "providers.models", provider_type, &choice.alias) {
                let alias_ref = format!("{}.{}", provider_type, choice.alias);
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "alias",
                    format!("alias `{alias_ref}` already exists"),
                    "cli-quickstart-error-alias-exists",
                    &[("alias", &alias_ref)],
                ));
                return None;
            }
            let codex_auth = provider_type == "openai" && auth_mode == QUICKSTART_AUTH_MODE_CODEX;
            let anthropic_oauth =
                provider_type == "anthropic" && auth_mode == QUICKSTART_AUTH_MODE_SETUP_TOKEN;
            let prefix = format!("providers.models.{}.{}", provider_type, choice.alias);
            if let Err(err) = config.create_map_key(
                &format!("providers.models.{}", provider_type),
                &choice.alias,
            ) {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "provider_type",
                    err.to_string(),
                ));
                return None;
            }
            if let Err(err) = config.set_prop_persistent(&format!("{prefix}.model"), &choice.model)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "model",
                    err.to_string(),
                ));
                return None;
            }
            if codex_auth {
                if let Err(err) = config
                    .set_prop_persistent(&format!("{prefix}.wire_api"), WireApi::Responses.as_str())
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        "wire_api",
                        err.to_string(),
                    ));
                    return None;
                }
                if let Err(err) =
                    config.set_prop_persistent(&format!("{prefix}.requires_openai_auth"), "true")
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        "requires_openai_auth",
                        err.to_string(),
                    ));
                    return None;
                }
            }
            if anthropic_oauth
                && let Err(err) =
                    config.set_prop_persistent(&format!("{prefix}.auth_mode"), "oauth")
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "auth_mode",
                    err.to_string(),
                ));
                return None;
            }
            // Round-trip every field the surface echoed back. Keys are
            // whatever `field_shape()` emitted — the daemon authored
            // them, so it knows where they go.
            let mut entries: Vec<(&String, &String)> = choice.fields.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in entries {
                if key == QUICKSTART_AUTH_MODE_FIELD {
                    continue;
                }
                if (codex_auth || anthropic_oauth)
                    && matches!(
                        key.as_str(),
                        "api_key" | "wire_api" | "requires_openai_auth"
                    )
                {
                    continue;
                }
                if value.is_empty() {
                    continue;
                }
                if let Err(err) = config.set_prop_persistent(&format!("{prefix}.{key}"), value) {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        zeroclaw_config::helpers::kebab_to_snake(key),
                        err.to_string(),
                    ));
                    return None;
                }
            }
            // Auto-populate context_window from provider's /models endpoint if supported.
            // Silently ignores failures (falls back to config default).
            // Only runs on multi-threaded Tokio runtime (actual CLI), not single-threaded test runtime.
            let provider_config = zeroclaw_config::schema::ModelProviderConfig {
                model: Some(choice.model.clone()),
                uri: config.get_prop(&format!("{prefix}.uri")).ok(),
                api_key: choice
                    .fields
                    .get("api_key")
                    .and_then(|v| if v.is_empty() { None } else { Some(v.clone()) }),
                ..Default::default()
            };
            if tokio::runtime::Handle::try_current()
                .map(|h| {
                    matches!(
                        h.runtime_flavor(),
                        tokio::runtime::RuntimeFlavor::MultiThread
                    )
                })
                .unwrap_or(false)
                && let Some(ctx) = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(
                        zeroclaw_providers::fetch_context_window(provider_type, &provider_config),
                    )
                })
            {
                let _ = config
                    .set_prop_persistent(&format!("{prefix}.context_window"), &ctx.to_string());
            }
            Some(format!("{}.{}", provider_type, choice.alias))
        }
    }
}

// ── Risk / Runtime presets ─────────────────────────────────────────

fn apply_named_preset<K, W>(
    config: &mut Config,
    choice: &SelectorChoice<String>,
    step: QuickstartStep,
    list_existing: K,
    write_preset: W,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String>
where
    K: Fn(&Config) -> Vec<String>,
    W: Fn(&mut Config, &str) -> Result<String, String>,
{
    match choice {
        SelectorChoice::Existing(alias) => {
            if list_existing(config).iter().any(|a| a == alias) {
                Some(alias.clone())
            } else {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    step,
                    "",
                    format!("no `{alias}` profile configured"),
                    "cli-quickstart-error-no-profile",
                    &[("alias", alias)],
                ));
                None
            }
        }
        SelectorChoice::Fresh(preset_name) => match write_preset(config, preset_name) {
            Ok(alias) => Some(alias),
            Err(msg) => {
                errors.push(QuickstartError::new(step, "", msg));
                None
            }
        },
    }
}

fn risk_preset_keys(config: &Config) -> Vec<String> {
    config.risk_profiles.keys().cloned().collect()
}

fn runtime_preset_keys(config: &Config) -> Vec<String> {
    config.runtime_profiles.keys().cloned().collect()
}

fn write_risk_preset(config: &mut Config, preset_name: &str) -> Result<String, String> {
    let preset =
        risk_preset(preset_name).ok_or_else(|| format!("unknown risk preset `{preset_name}`"))?;
    // Existing block wins — never clobber a user-customised `[risk-profiles.<name>]`
    // that happens to share a preset name.
    if config.risk_profiles.contains_key(preset.preset_name) {
        return Ok(preset.preset_name.to_string());
    }
    config
        .create_map_key("risk_profiles", preset.preset_name)
        .map_err(|e| e.to_string())?;
    config
        .risk_profiles
        .insert(preset.preset_name.to_string(), (preset.values)());
    config.mark_dirty(&format!("risk_profiles.{}", preset.preset_name));
    Ok(preset.preset_name.to_string())
}

fn write_runtime_preset(config: &mut Config, preset_name: &str) -> Result<String, String> {
    let preset = runtime_preset(preset_name)
        .ok_or_else(|| format!("unknown runtime preset `{preset_name}`"))?;
    // Existing block wins — same rule as `write_risk_preset`.
    if config.runtime_profiles.contains_key(preset.preset_name) {
        return Ok(preset.preset_name.to_string());
    }
    config
        .create_map_key("runtime_profiles", preset.preset_name)
        .map_err(|e| e.to_string())?;
    config
        .runtime_profiles
        .insert(preset.preset_name.to_string(), (preset.values)());
    config.mark_dirty(&format!("runtime_profiles.{}", preset.preset_name));
    Ok(preset.preset_name.to_string())
}

// ── Memory ─────────────────────────────────────────────────────────

fn apply_memory(
    config: &mut Config,
    choice: &SelectorChoice<MemoryChoice>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String> {
    match choice {
        SelectorChoice::Existing(reference) => {
            let (family, alias) = match split_ref(reference) {
                Some(parts) => parts,
                None => {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Memory,
                        "",
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                        "cli-quickstart-error-not-type-alias-ref",
                        &[("reference", reference)],
                    ));
                    return None;
                }
            };
            if !storage_has_ref(config, reference) {
                let path = format!("storage.{family}.{alias}");
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::Memory,
                    "",
                    format!("no `{path}` configured"),
                    "cli-quickstart-error-no-configured-path",
                    &[("path", &path)],
                ));
                return None;
            }
            if let Err(err) = config.set_prop_persistent("memory.backend", reference) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "backend",
                    err.to_string(),
                ));
                return None;
            }
            Some(reference.clone())
        }
        SelectorChoice::Fresh(kind) => {
            let kind_name = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase());
            if matches!(kind, MemoryChoice::None) {
                if let Err(err) = config.set_prop_persistent("memory.backend", "none") {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Memory,
                        "backend",
                        err.to_string(),
                    ));
                    return None;
                }
                return Some("none".to_string());
            }
            let backend_ref = format!("{kind_name}.{kind_name}");
            let parent_path = format!("storage.{kind_name}");
            if let Err(err) = config.create_map_key(&parent_path, &kind_name) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "",
                    err.to_string(),
                ));
                return None;
            }
            if let Err(err) = config.set_prop_persistent("memory.backend", &backend_ref) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "backend",
                    err.to_string(),
                ));
                return None;
            }
            Some(backend_ref)
        }
    }
}

// ── Channels ───────────────────────────────────────────────────────

fn usable_quickstart_value(value: &str) -> Option<&str> {
    let value = value.trim();
    (!zeroclaw_config::traits::is_unset_display_value(value)).then_some(value)
}

fn apply_channels(
    config: &mut Config,
    channels: &[SelectorChoice<ChannelQuickStart>],
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Vec<String> {
    let mut refs = Vec::with_capacity(channels.len());
    for (idx, ch) in channels.iter().enumerate() {
        match ch {
            SelectorChoice::Existing(reference) => {
                if let Some((family, alias)) = split_ref(reference) {
                    if !channel_exists(config, family, alias) {
                        let path = format!("channels.{family}.{alias}");
                        errors.push(QuickstartError::for_surface(
                            ctx,
                            QuickstartStep::Channels,
                            format!("channels[{idx}]"),
                            format!("no `{path}` configured"),
                            "cli-quickstart-error-no-configured-path",
                            &[("path", &path)],
                        ));
                        continue;
                    }
                    // Existing channel already bound to a different agent
                    // cannot be re-used — one channel, one agent invariant.
                    if let Some(owner) = config.agent_for_channel(reference) {
                        errors.push(QuickstartError::for_surface(
                            ctx,
                            QuickstartStep::Channels,
                            format!("channels[{idx}]"),
                            format!("channel `{reference}` is already bound to agent `{owner}`"),
                            "cli-quickstart-error-channel-bound",
                            &[("reference", reference), ("owner", owner)],
                        ));
                        continue;
                    }
                    refs.push(reference.clone());
                } else {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}]"),
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                        "cli-quickstart-error-not-type-alias-ref",
                        &[("reference", reference)],
                    ));
                }
            }
            SelectorChoice::Fresh(entry) => {
                let submitted_channel_type = entry.channel_type.trim();
                let alias = entry.alias.trim();
                if submitted_channel_type.is_empty() || alias.is_empty() {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}]"),
                        "channel type and alias are required",
                        "cli-quickstart-error-channel-required",
                        &[],
                    ));
                    continue;
                }
                let (config_channel_type, is_whatsapp_web) =
                    resolve_channel_quickstart_type(submitted_channel_type);
                if channel_exists(config, config_channel_type, alias) {
                    let alias_ref = format!("{config_channel_type}.{alias}");
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].alias"),
                        format!("alias `{alias_ref}` already exists"),
                        "cli-quickstart-error-alias-exists",
                        &[("alias", &alias_ref)],
                    ));
                    continue;
                }
                let advertised: std::collections::HashSet<String> =
                    field_shape(FieldSection::Channel, &entry.channel_type)
                        .into_iter()
                        .map(|field| field.key)
                        .collect();
                if let Some(key) = entry
                    .fields
                    .keys()
                    .filter(|key| !advertised.contains(*key))
                    .min()
                {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.{key}"),
                        format!("channel field `{key}` is not available in Quickstart"),
                        "cli-quickstart-error-channel-field-not-advertised",
                        &[("field", key.as_str())],
                    ));
                    continue;
                }
                if config_channel_type == "webhook"
                    && entry
                        .fields
                        .get("secret")
                        .and_then(|value| usable_quickstart_value(value))
                        .is_none()
                {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.secret"),
                        "webhook secret is required",
                        "cli-quickstart-error-webhook-secret-required",
                        &[],
                    ));
                    continue;
                }
                let mut staged = config.clone();
                if let Err(err) =
                    staged.create_map_key(&format!("channels.{config_channel_type}"), alias)
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].channel_type"),
                        err.to_string(),
                    ));
                    continue;
                }
                let prefix = format!("channels.{config_channel_type}.{alias}");
                if config_channel_type == "webhook"
                    && entry
                        .fields
                        .get("port")
                        .and_then(|value| usable_quickstart_value(value))
                        .is_none()
                    && let Err(err) = staged.set_prop_persistent(
                        &format!("{prefix}.port"),
                        &zeroclaw_config::schema::DEFAULT_WEBHOOK_CHANNEL_PORT.to_string(),
                    )
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.port"),
                        err.to_string(),
                    ));
                    continue;
                }
                let mut fields: Vec<_> = entry
                    .fields
                    .iter()
                    .filter_map(|(key, value)| {
                        usable_quickstart_value(value).map(|value| (key, value))
                    })
                    .collect();
                fields.sort_by_key(|(left, _)| *left);

                let mut failed = false;
                for (key, value) in fields {
                    if let Err(err) = staged.set_prop_persistent(&format!("{prefix}.{key}"), value)
                    {
                        errors.push(QuickstartError::new(
                            QuickstartStep::Channels,
                            format!("channels[{idx}].fields.{key}"),
                            err.to_string(),
                        ));
                        failed = true;
                        break;
                    }
                }
                if !failed && is_whatsapp_web {
                    let default_path = default_whatsapp_web_session_path(config, alias);
                    if let Err(err) =
                        staged.set_prop_persistent(&format!("{prefix}.session_path"), &default_path)
                    {
                        errors.push(QuickstartError::new(
                            QuickstartStep::Channels,
                            format!("channels[{idx}].channel_type"),
                            err.to_string(),
                        ));
                        failed = true;
                    }
                }
                if !failed
                    && let Err(err) =
                        staged.set_prop_persistent(&format!("{prefix}.enabled"), "true")
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.enabled"),
                        err.to_string(),
                    ));
                    failed = true;
                }
                if !failed
                    && config_channel_type == "telegram"
                    && let Some(telegram) = staged.channels.telegram.get(alias)
                    && let Err(err) = telegram.validate_bot_token(alias)
                {
                    let structured =
                        zeroclaw_config::api_error::ConfigApiError::from_validation(err);
                    let terminal = structured
                        .path
                        .as_deref()
                        .and_then(|path| path.rsplit('.').next())
                        .unwrap_or("credential");
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.{terminal}"),
                        structured.message,
                        "cli-quickstart-error-channel-token-required",
                        &[],
                    ));
                    failed = true;
                }
                if !failed
                    && config_channel_type == "discord"
                    && let Some(discord) = staged.channels.discord.get(alias)
                    && let Err(err) = discord.validate_bot_token(alias)
                {
                    let structured =
                        zeroclaw_config::api_error::ConfigApiError::from_validation(err);
                    let terminal = structured
                        .path
                        .as_deref()
                        .and_then(|path| path.rsplit('.').next())
                        .unwrap_or("credential");
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.{terminal}"),
                        structured.message,
                        "cli-quickstart-error-channel-token-required",
                        &[],
                    ));
                    failed = true;
                }
                if failed {
                    continue;
                }
                *config = staged;
                refs.push(format!("{config_channel_type}.{alias}"));
            }
        }
    }
    refs
}

fn channel_exists(config: &Config, channel_type: &str, alias: &str) -> bool {
    let probe = format!("channels.{channel_type}.{alias}.enabled");
    config.get_prop(&probe).is_ok()
}

// ── Peer groups ────────────────────────────────────────────────────

fn apply_peer_groups(
    config: &mut Config,
    peer_groups: &[zeroclaw_config::presets::QuickstartPeerGroup],
    staged_channel_refs: &[String],
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Vec<String> {
    let mut refs = Vec::with_capacity(peer_groups.len());
    for (idx, pg) in peer_groups.iter().enumerate() {
        if pg.name.trim().is_empty() {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].name"),
                "peer-group name is required",
                "cli-quickstart-error-peer-group-name-required",
                &[],
            ));
            continue;
        }
        if pg.channel.trim().is_empty() {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                "peer-group channel ref is required",
                "cli-quickstart-error-peer-group-channel-required",
                &[],
            ));
            continue;
        }
        let Some(channel_ref) = canonical_quickstart_channel_ref(&pg.channel) else {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                format!("`{}` is not a `<type>.<alias>` reference", pg.channel),
                "cli-quickstart-error-not-type-alias-ref",
                &[("reference", &pg.channel)],
            ));
            continue;
        };
        // Channel ref must resolve to either a channel already in config
        // OR a channel staged in this same submission.
        let staged_match = staged_channel_refs.iter().any(|r| r == &channel_ref);
        let configured_match = match split_ref(&channel_ref) {
            Some((family, alias)) => channel_exists(config, family, alias),
            None => false,
        };
        if !staged_match && !configured_match {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                format!(
                    "peer-group `{}` references unknown channel `{}`",
                    pg.name, pg.channel
                ),
                "cli-quickstart-error-peer-group-unknown-channel",
                &[("name", &pg.name), ("channel", &pg.channel)],
            ));
            continue;
        }
        // Collision: existing peer-group block wins. Surface the conflict
        // so the operator sees what they need to rename.
        if config.peer_groups.contains_key(&pg.name) {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].name"),
                format!("peer-group `{}` already exists", pg.name),
                "cli-quickstart-error-peer-group-exists",
                &[("name", &pg.name)],
            ));
            continue;
        }
        if let Err(err) = config.create_map_key("peer_groups", &pg.name) {
            errors.push(QuickstartError::new(
                QuickstartStep::Channels,
                format!("peer_groups[{idx}]"),
                err.to_string(),
            ));
            continue;
        }
        let prefix = format!("peer_groups.{}", pg.name);
        if let Err(err) = config.set_prop_persistent(&format!("{prefix}.channel"), &channel_ref) {
            errors.push(QuickstartError::new(
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                err.to_string(),
            ));
            continue;
        }
        if !pg.external_peers.is_empty() {
            let joined = pg
                .external_peers
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if let Err(err) =
                config.set_prop_persistent(&format!("{prefix}.external_peers"), &joined)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::Channels,
                    format!("peer_groups[{idx}].external_peers"),
                    err.to_string(),
                ));
                continue;
            }
        }
        if !pg.ignore.is_empty() {
            let joined = pg
                .ignore
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if let Err(err) = config.set_prop_persistent(&format!("{prefix}.ignore"), &joined) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Channels,
                    format!("peer_groups[{idx}].ignore"),
                    err.to_string(),
                ));
                continue;
            }
        }
        refs.push(pg.name.clone());
    }
    refs
}

// ── Personality files ──────────────────────────────────────────────

/// A personality file staged to a tempfile during `apply_into`, moved
/// into place only after the atomic config write succeeds. On config
/// failure the tempfile drops and cleans itself up — nothing orphaned.
struct StagedPersonalityWrite {
    tempfile: tempfile::NamedTempFile,
    dest: std::path::PathBuf,
}

fn apply_personality_files(
    config: &Config,
    agent_alias: &str,
    files: &[zeroclaw_config::presets::QuickstartPersonalityFile],
    staged: &mut Vec<StagedPersonalityWrite>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) {
    if files.is_empty() {
        return;
    }
    let workspace = config.agent_workspace_dir(agent_alias);
    if let Err(err) = std::fs::create_dir_all(&workspace) {
        errors.push(QuickstartError::for_surface(
            ctx,
            QuickstartStep::Agent,
            "personality_files",
            format!("could not create agent workspace: {err}"),
            "cli-quickstart-error-personality-workspace",
            &[("err", &err.to_string())],
        ));
        return;
    }
    for (idx, file) in files.iter().enumerate() {
        let trimmed = file.filename.trim();
        if trimmed.is_empty() {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}].filename"),
                "filename is required",
                "cli-quickstart-error-personality-filename-required",
                &[],
            ));
            continue;
        }
        if !crate::agent::personality::EDITABLE_PERSONALITY_FILES.contains(&trimmed) {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}].filename"),
                format!("`{trimmed}` is not an editable personality file"),
                "cli-quickstart-error-personality-not-editable",
                &[("filename", trimmed)],
            ));
            continue;
        }
        if file.content.chars().count() > crate::agent::personality::MAX_FILE_CHARS {
            let limit = crate::agent::personality::MAX_FILE_CHARS.to_string();
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}].content"),
                format!("content exceeds {} char limit", limit),
                "cli-quickstart-error-personality-too-large",
                &[("limit", &limit)],
            ));
            continue;
        }
        // Stage to a tempfile in the destination directory rather than
        // writing the final path now. The commit happens after the atomic
        // config persist in `apply_with_surface`.
        let mut tempfile = match tempfile::NamedTempFile::new_in(&workspace) {
            Ok(t) => t,
            Err(err) => {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::Agent,
                    format!("personality_files[{idx}]"),
                    format!("could not stage `{trimmed}`: {err}"),
                    "cli-quickstart-error-personality-stage-failed",
                    &[("filename", trimmed), ("err", &err.to_string())],
                ));
                continue;
            }
        };
        if let Err(err) = std::io::Write::write_all(&mut tempfile, file.content.as_bytes()) {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}]"),
                format!("could not stage `{trimmed}`: {err}"),
                "cli-quickstart-error-personality-stage-failed",
                &[("filename", trimmed), ("err", &err.to_string())],
            ));
            continue;
        }
        staged.push(StagedPersonalityWrite {
            tempfile,
            dest: workspace.join(trimmed),
        });
    }
}

/// Move every staged tempfile into place. Called only after the atomic
/// config write succeeds; a failure here is reported but the agent is
/// already persisted and valid.
fn commit_personality_files(
    staged: Vec<StagedPersonalityWrite>,
    warnings: &mut Vec<QuickstartWarning>,
    ctx: Option<&RunCtx>,
) {
    for write in staged {
        if let Err(err) = write.tempfile.persist(&write.dest) {
            let path = write.dest.display().to_string();
            warnings.push(QuickstartWarning::for_surface(
                ctx,
                QuickstartStep::Agent,
                "personality_files",
                format!(
                    "Quickstart completed, but could not write personality file `{path}`: {}",
                    err.error
                ),
                "cli-quickstart-warning-personality-write-failed",
                &[("path", &path), ("err", &err.error.to_string())],
            ));
        }
    }
}

// ── Default skills bundle FTUE ─────────────────────────────────────

fn materialize_default_skills_bundle(config: &mut Config) {
    if !config.skill_bundles.is_empty() {
        return;
    }
    // create_map_key returns Ok(false) on existing key (idempotent),
    // Ok(true) on insertion. We don't propagate the error: the FTUE
    // bundle is best-effort and the operator can configure one later.
    let _ = config.create_map_key("skill-bundles", "default");
}

// ── Agent ──────────────────────────────────────────────────────────

fn apply_agent(
    config: &mut Config,
    identity: &AgentIdentity,
    provider_ref: &str,
    risk_alias: &str,
    runtime_alias: &str,
    channel_refs: &[String],
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String> {
    if identity.name.trim().is_empty() {
        errors.push(QuickstartError::for_surface(
            ctx,
            QuickstartStep::Agent,
            "name",
            "agent name is required",
            "cli-quickstart-error-agent-name-required",
            &[],
        ));
        return None;
    }
    if config.agents.contains_key(&identity.name) {
        errors.push(QuickstartError::for_surface(
            ctx,
            QuickstartStep::Agent,
            "name",
            format!("agent `{}` already exists", identity.name),
            "cli-quickstart-error-agent-exists",
            &[("name", &identity.name)],
        ));
        return None;
    }

    let prefix = format!("agents.{}", identity.name);
    // Operator-facing surface: route through the shared guard so onboarding an
    // agent literally named `default` is refused (the reserved runtime fallback),
    // symmetric with the create/RPC/CLI surfaces. Non-`default` names create as
    // before.
    if let Err(err) =
        zeroclaw_config::alias_refs::create_map_key_checked(config, "agents", &identity.name)
    {
        errors.push(QuickstartError::new(
            QuickstartStep::Agent,
            "name",
            err.to_string(),
        ));
        return None;
    }
    let writes: [(&str, &str); 3] = [
        ("model_provider", provider_ref),
        ("risk_profile", risk_alias),
        ("runtime_profile", runtime_alias),
    ];
    for (field, value) in writes {
        let path = format!("{prefix}.{field}");
        if let Err(err) = config.set_prop_persistent(&path, value) {
            errors.push(QuickstartError::new(
                QuickstartStep::Agent,
                field,
                err.to_string(),
            ));
            return None;
        }
    }
    if !channel_refs.is_empty() {
        let path = format!("{prefix}.channels");
        let json = serde_json::to_string(channel_refs).unwrap_or_else(|_| "[]".to_string());
        if let Err(err) = config.set_prop_persistent(&path, &json) {
            errors.push(QuickstartError::new(
                QuickstartStep::Agent,
                "channels",
                err.to_string(),
            ));
            return None;
        }
    }
    Some(identity.name.clone())
}

// ── Shared helpers ─────────────────────────────────────────────────

fn split_ref(reference: &str) -> Option<(&str, &str)> {
    let (ty, alias) = reference.split_once('.')?;
    if ty.is_empty() || alias.is_empty() {
        None
    } else {
        Some((ty, alias))
    }
}

fn section_has_alias(config: &Config, prefix: &str, family: &str, alias: &str) -> bool {
    for probe_field in ["enabled", "model", "uri"] {
        let probe = format!("{prefix}.{family}.{alias}.{probe_field}");
        if config.get_prop(&probe).is_ok() {
            return true;
        }
    }
    false
}

fn storage_has_ref(config: &Config, reference: &str) -> bool {
    collect_aliased_refs(&config.storage)
        .iter()
        .any(|configured| configured == reference)
}

pub async fn model_catalog(
    model_provider: &str,
) -> (
    Vec<String>,
    Option<std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing>>,
    bool,
) {
    model_catalog_with_config(None, model_provider).await
}

pub async fn model_catalog_with_config(
    config: Option<&Config>,
    model_provider: &str,
) -> (
    Vec<String>,
    Option<std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing>>,
    bool,
) {
    let resolved = config.and_then(|cfg| cfg.providers.models.find_by_name(model_provider));
    let api_key = resolved
        .as_ref()
        .and_then(|(_family, _alias, base)| base.api_key.clone());
    // Honor a configured custom endpoint so proxied / self-hosted OpenAI-compatible
    // deployments list from their own `/models` rather than the family default.
    let api_url = resolved.as_ref().and_then(|(_family, _alias, base)| {
        base.uri
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(ToString::to_string)
    });
    // `create_model_provider` and the chat-catalog ranker expect a bare family
    // name, not a dotted ref. Prefer the family `find_by_name` resolved; when it
    // could not (no config / unknown alias) strip any `<family>.<alias>` suffix
    // ourselves so a dotted selector still constructs.
    let family: &str = resolved
        .as_ref()
        .map(|(family, _alias, _base)| *family)
        .unwrap_or_else(|| {
            model_provider
                .split_once('.')
                .map_or(model_provider, |(f, _)| f)
        });

    let handle = zeroclaw_providers::create_model_provider_with_url(
        family,
        api_key.as_deref(),
        api_url.as_deref(),
    );
    if let Ok(handle) = handle
        && let Ok(models) = zeroclaw_providers::ProviderDispatch::from_ref(&*handle)
            .list_models_with_pricing()
            .await
        && !models.is_empty()
    {
        let raw_pricing: std::collections::HashMap<
            String,
            zeroclaw_api::model_provider::ModelPricing,
        > = models
            .iter()
            .filter_map(|m| m.pricing.as_ref().map(|p| (m.id.clone(), p.clone())))
            .collect();
        let ids = models.into_iter().map(|m| m.id).collect();
        let Some(ids) = zeroclaw_providers::catalog::sort_model_catalog_for_chat(family, ids)
        else {
            return (Vec::new(), None, false);
        };
        let pricing: std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing> =
            ids.iter()
                .filter_map(|id| raw_pricing.get(id).map(|p| (id.clone(), p.clone())))
                .collect();
        let pricing = if pricing.is_empty() {
            None
        } else {
            Some(pricing)
        };
        return (ids, pricing, true);
    }
    match zeroclaw_providers::catalog::list_models_for_family(family).await {
        Ok(models) if !models.is_empty() => (
            zeroclaw_providers::catalog::sort_model_catalog_for_chat(family, models)
                .unwrap_or_default(),
            None,
            true,
        ),
        _ => (Vec::new(), None, false),
    }
}

/// `true` for model_provider families that need no remote credential.
#[must_use]
pub fn model_provider_is_local(model_provider: &str) -> bool {
    zeroclaw_providers::list_model_providers()
        .iter()
        .find(|p| p.name == model_provider)
        .is_some_and(|p| p.local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
        SelectorChoice,
    };
    use zeroclaw_config::schema::Config;

    #[test]
    fn channel_type_options_cover_every_schema_channel() {
        let cfg = Config::default();
        let picker = build_channel_type_options(&cfg.channels);
        let schema = cfg.channels.channels();
        assert_eq!(
            picker.len(),
            schema.len(),
            "Quickstart channel-type picker count diverged from \
             ChannelsConfig::channels(); picker has {} rows, schema has {}",
            picker.len(),
            schema.len(),
        );
        for (picked, expected) in picker.iter().zip(schema.iter()) {
            assert_eq!(
                picked.kind, expected.kind,
                "kind mismatch at {} — picker `{}`, schema `{}`",
                picked.display_name, picked.kind, expected.kind,
            );
            assert_eq!(
                picked.display_name, expected.name,
                "display_name mismatch at `{}` — picker `{}`, schema `{}`",
                picked.kind, picked.display_name, expected.name,
            );
        }
    }

    #[test]
    fn provider_runtime_defaults_follow_canonical_provider_recommendations() {
        let snapshot = snapshot_state(&Config::default());

        let local = snapshot
            .model_provider_types
            .iter()
            .find(|provider| provider.kind == "lmstudio")
            .expect("LM Studio should be present in the canonical provider registry");
        assert!(local.local);
        assert_eq!(
            local.default_runtime_profile.as_deref(),
            Some("local_small")
        );

        let ollama = snapshot
            .model_provider_types
            .iter()
            .find(|provider| provider.kind == "ollama")
            .expect("Ollama should be present in the canonical provider registry");
        assert!(ollama.local);
        assert_eq!(
            ollama.default_runtime_profile.as_deref(),
            None,
            "providers without native tools must use the canonical fallback",
        );

        let remote = snapshot
            .model_provider_types
            .iter()
            .find(|provider| provider.kind == "anthropic")
            .expect("Anthropic should be present in the canonical provider registry");
        assert!(!remote.local);
        assert_eq!(remote.default_runtime_profile, None);
        assert_eq!(snapshot.default_runtime_profile, "unbounded");

        let cli_shim = snapshot
            .model_provider_types
            .iter()
            .find(|provider| provider.kind == "gemini_cli")
            .expect("Gemini CLI should be present in the canonical provider registry");
        assert!(cli_shim.local);
        assert_eq!(
            cli_shim.default_runtime_profile.as_deref(),
            None,
            "credential-free cloud CLI providers must not inherit local-small policy",
        );

        for provider in &snapshot.model_provider_types {
            if let Some(default) = provider.default_runtime_profile.as_deref() {
                assert!(
                    snapshot
                        .runtime_presets
                        .iter()
                        .any(|preset| preset.preset_name == default),
                    "provider {} advertised unavailable runtime preset {default}",
                    provider.kind,
                );
            }
        }
    }

    fn fresh_submission(agent_name: &str) -> BuilderSubmission {
        BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([(
                    "api_key".to_string(),
                    "sk-test".to_string(),
                )]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: agent_name.into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        }
    }

    fn fresh_channel(
        channel_type: &str,
        alias: &str,
        fields: &[(&str, &str)],
    ) -> ChannelQuickStart {
        ChannelQuickStart {
            channel_type: channel_type.into(),
            alias: alias.into(),
            fields: fields
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn apply_fresh_provider(
        choice: ModelProviderChoice,
    ) -> (Config, Option<AppliedAgent>, Vec<QuickstartError>) {
        let mut cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.model_provider = SelectorChoice::Fresh(choice);
        let mut staged = Vec::new();
        let mut errors = Vec::new();
        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
        (cfg, applied, errors)
    }

    #[test]
    fn existing_postgres_memory_storage_ref_is_accepted() {
        let mut cfg = Config::default();
        cfg.storage.postgres.insert(
            "default".into(),
            zeroclaw_config::schema::PostgresStorageConfig::default(),
        );
        let choice = SelectorChoice::Existing("postgres.default".to_string());
        let mut errors = Vec::new();

        let applied = apply_memory(&mut cfg, &choice, &mut errors, None);

        assert!(errors.is_empty(), "apply_memory errors: {errors:?}");
        assert_eq!(applied.as_deref(), Some("postgres.default"));
        assert_eq!(cfg.memory.backend, "postgres.default");
    }

    #[test]
    fn memory_storage_refs_from_snapshot_are_accepted() {
        let mut cfg = Config::default();
        cfg.storage.postgres.insert(
            "default".into(),
            zeroclaw_config::schema::PostgresStorageConfig::default(),
        );
        let snapshot = snapshot_state(&cfg);

        assert!(
            snapshot
                .storage
                .iter()
                .any(|ref_| ref_ == "postgres.default"),
            "snapshot should expose configured postgres storage: {:?}",
            snapshot.storage
        );
        for reference in snapshot.storage {
            let mut candidate = cfg.clone();
            let choice = SelectorChoice::Existing(reference.clone());
            let mut errors = Vec::new();

            let applied = apply_memory(&mut candidate, &choice, &mut errors, None);

            assert!(
                errors.is_empty(),
                "snapshot storage ref {reference:?} should apply without errors: {errors:?}"
            );
            assert_eq!(applied.as_deref(), Some(reference.as_str()));
            assert_eq!(candidate.memory.backend, reference);
        }
    }

    #[test]
    fn apply_serializes_provider_fields_as_snake_case() {
        let mut cfg = Config::default();
        let submission = fresh_submission("bot");
        let mut staged = Vec::new();
        let mut errors = Vec::new();
        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some(), "apply_into should yield an agent");
        // The submission carries the snake field key `api_key` and it must
        // land on disk as the snake serde field `api_key`, never kebab.
        let toml = toml::to_string(&cfg).expect("serialize config");
        assert!(
            toml.contains("api_key"),
            "expected snake `api_key` in serialized config:\n{toml}"
        );
        assert!(
            !toml.contains("api-key"),
            "kebab `api-key` leaked into serialized config:\n{toml}"
        );
    }

    #[test]
    fn apply_provider_type_trims_and_canonicalizes_whitespace() {
        // A provider type with stray whitespace must canonicalize to the
        // registry's family key, not reach create_map_key verbatim (which would
        // fail with "no map-keyed/list section at providers.models.llamacpp ").
        let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "  llamacpp  ".into(),
            alias: "local".into(),
            model: "qwen2.5-coder".into(),
            fields: std::collections::HashMap::new(),
        });
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        assert!(
            cfg.providers.models.find("llamacpp", "local").is_some(),
            "expected providers.models.llamacpp.local to exist"
        );
        let agent = cfg.agents.get("bot").expect("agent created");
        assert_eq!(agent.model_provider.as_str(), "llamacpp.local");
    }

    #[test]
    fn apply_provider_type_case_insensitive() {
        let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "Anthropic".into(),
            alias: "main".into(),
            model: "claude-sonnet-4-5".into(),
            fields: std::collections::HashMap::new(),
        });
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        assert!(cfg.providers.models.find("anthropic", "main").is_some());
    }

    #[test]
    fn apply_claude_alias_writes_canonical_anthropic_config() {
        let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "claude".into(),
            alias: "max".into(),
            model: "claude-sonnet-4-5".into(),
            fields: std::collections::HashMap::from([
                ("auth_mode".to_string(), "setup_token".to_string()),
                ("api_key".to_string(), "sk-ant-oat01-test-token".to_string()),
            ]),
        });
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        let entry = cfg
            .providers
            .models
            .find("anthropic", "max")
            .expect("anthropic.max entry");
        assert_eq!(entry.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(entry.api_key, None);
        assert_eq!(
            cfg.get_prop("providers.models.anthropic.max.auth_mode")
                .expect("Anthropic OAuth mode"),
            "oauth"
        );
        let agent = cfg.agents.get("bot").expect("agent created");
        assert_eq!(agent.model_provider.as_str(), "anthropic.max");
    }

    #[test]
    fn apply_openai_codex_alias_writes_canonical_openai_auth_config() {
        let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "openai-codex".into(),
            alias: "coding".into(),
            model: "gpt-5.4".into(),
            fields: std::collections::HashMap::new(),
        });
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        let entry = cfg
            .providers
            .models
            .find("openai", "coding")
            .expect("openai.coding entry");
        assert_eq!(entry.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(entry.wire_api, Some(WireApi::Responses));
        assert!(entry.requires_openai_auth);
        let agent = cfg.agents.get("bot").expect("agent created");
        assert_eq!(agent.model_provider.as_str(), "openai.coding");
    }

    #[test]
    fn apply_openai_auth_mode_codex_ignores_api_key_field() {
        let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "openai".into(),
            alias: "coding".into(),
            model: "gpt-5.4".into(),
            fields: std::collections::HashMap::from([
                ("auth_mode".to_string(), "codex".to_string()),
                ("api_key".to_string(), "sk-should-not-persist".to_string()),
            ]),
        });
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        let entry = cfg
            .providers
            .models
            .find("openai", "coding")
            .expect("openai.coding entry");
        assert_eq!(entry.wire_api, Some(WireApi::Responses));
        assert!(entry.requires_openai_auth);
        assert!(
            entry.api_key.is_none(),
            "Codex auth must not persist an API key from the Quickstart form"
        );
    }

    #[test]
    fn apply_unknown_anthropic_auth_mode_errors_clearly() {
        let (_, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "anthropic".into(),
            alias: "main".into(),
            model: "claude-sonnet-4-5".into(),
            fields: std::collections::HashMap::from([(
                "auth_mode".to_string(),
                "not_real".to_string(),
            )]),
        });
        assert!(applied.is_none());
        assert!(
            errors
                .iter()
                .any(|e| e.step == QuickstartStep::ModelProvider
                    && e.field == "auth_mode"
                    && e.message.contains("unknown Anthropic auth mode")),
            "expected a clear unknown-Anthropic-auth-mode error, got: {errors:?}"
        );
    }

    #[test]
    fn apply_unknown_provider_type_errors_clearly() {
        let (_, applied, errors) = apply_fresh_provider(ModelProviderChoice {
            provider_type: "not_a_real_provider".into(),
            alias: "x".into(),
            model: "m".into(),
            fields: std::collections::HashMap::new(),
        });
        assert!(applied.is_none());
        assert!(
            errors
                .iter()
                .any(|e| e.step == QuickstartStep::ModelProvider
                    && e.message.contains("unknown model provider type")),
            "expected a clear unknown-provider error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_only_passes_on_fresh_submission() {
        let cfg = Config::default();
        let submission = fresh_submission("bot");
        validate_only(&submission, &cfg).expect("fresh submission validates");
    }

    #[test]
    fn validate_only_rejects_blank_agent_name() {
        let cfg = Config::default();
        let submission = fresh_submission("");
        let errors = validate_only(&submission, &cfg).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.step == QuickstartStep::Agent && e.field == "name")
        );
    }

    #[test]
    fn validate_only_rejects_existing_agent_name() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "bot".into(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let submission = fresh_submission("bot");
        let errors = validate_only(&submission, &cfg).unwrap_err();
        assert!(errors.iter().any(|e| e.step == QuickstartStep::Agent));
    }

    #[test]
    fn validate_only_rejects_unknown_risk_preset() {
        let cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.risk_profile = SelectorChoice::Fresh("does-not-exist".into());
        let errors = validate_only(&submission, &cfg).unwrap_err();
        assert!(errors.iter().any(|e| e.step == QuickstartStep::RiskProfile));
    }

    #[tokio::test]
    async fn rejected_runtime_selection_leaves_live_config_unchanged() {
        let mut cfg = Config::default();
        let before = serde_json::to_value(&cfg).expect("serialize initial config");
        let before_dirty_paths = cfg.dirty_paths.clone();
        let mut submission = fresh_submission("bot");
        submission.runtime_profile = SelectorChoice::Fresh("does-not-exist".into());

        let errors = apply(submission, &mut cfg).await.unwrap_err();

        assert!(
            errors
                .iter()
                .any(|error| error.step == QuickstartStep::RuntimeProfile),
            "expected a runtime-profile error, got {errors:?}",
        );
        assert_eq!(
            serde_json::to_value(&cfg).expect("serialize rejected config"),
            before,
        );
        assert_eq!(cfg.dirty_paths, before_dirty_paths);
    }

    #[tokio::test]
    async fn missing_existing_runtime_leaves_live_config_unchanged() {
        let mut cfg = Config::default();
        let before = serde_json::to_value(&cfg).expect("serialize initial config");
        let before_dirty_paths = cfg.dirty_paths.clone();
        let mut submission = fresh_submission("bot");
        submission.runtime_profile = SelectorChoice::Existing("missing".into());

        let errors = apply(submission, &mut cfg).await.unwrap_err();

        assert!(
            errors
                .iter()
                .any(|error| error.step == QuickstartStep::RuntimeProfile),
            "expected a runtime-profile error, got {errors:?}",
        );
        assert_eq!(
            serde_json::to_value(&cfg).expect("serialize rejected config"),
            before,
        );
        assert_eq!(cfg.dirty_paths, before_dirty_paths);
    }

    #[test]
    fn validate_only_accepts_every_builtin_risk_preset() {
        let cfg = Config::default();
        for p in zeroclaw_config::presets::RISK_PRESETS {
            let mut submission = fresh_submission("bot");
            submission.risk_profile = SelectorChoice::Fresh(p.preset_name.into());
            validate_only(&submission, &cfg).unwrap_or_else(|e| {
                panic!("risk preset `{}` failed validate: {e:?}", p.preset_name)
            });
        }
    }

    #[test]
    fn field_shape_returns_model_provider_rows_for_canonical_types() {
        for kind in ["anthropic", "openai", "ollama", "openrouter", "groq"] {
            let rows = super::field_shape(super::FieldSection::ModelProvider, kind);
            let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
            assert!(
                keys.contains(&"model"),
                "field_shape for `{kind}` is missing `model` row; got {keys:?}",
            );
            assert!(
                keys.contains(&"api_key"),
                "field_shape for `{kind}` is missing `api_key` row; got {keys:?}",
            );
        }
    }

    /// Codex subscription auth: `field_shape(ModelProvider, "openai")` exposes
    /// one Quickstart-only auth selector instead of raw config toggles. Apply
    /// translates `auth_mode = "codex"` into the canonical persisted
    /// `wire_api = "responses"` + `requires_openai_auth = true` fields.
    #[test]
    fn field_shape_openai_includes_codex_auth_mode() {
        let rows = super::field_shape(super::FieldSection::ModelProvider, "openai");
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert!(
            keys.contains(&"auth_mode"),
            "field_shape for openai must include `auth_mode` for Codex subscription; got {keys:?}",
        );
        assert!(
            !keys.contains(&"requires_openai_auth") && !keys.contains(&"wire_api"),
            "field_shape for openai should hide raw Codex config toggles; got {keys:?}",
        );
        let auth = rows
            .iter()
            .find(|row| row.key == "auth_mode")
            .expect("auth_mode row");
        assert!(auth.required);
        assert_eq!(
            auth.enum_variants.as_deref(),
            Some(["api_key".to_string(), "codex".to_string()].as_slice())
        );
        assert_eq!(auth.default.as_deref(), Some("api_key"));
        // No row may carry the `<unset>` placeholder as its default.
        // It's a display sentinel for an unset Option; echoing it back
        // through any surface (CLI/TUI/web) makes the daemon validate
        // `<unset>` against the field's real type and reject it.
        for row in &rows {
            assert_ne!(
                row.default.as_deref(),
                Some(zeroclaw_config::traits::UNSET_DISPLAY),
                "`{}` must not default to the <unset> placeholder",
                row.key
            );
        }
    }

    #[test]
    fn field_shape_openai_codex_alias_preselects_codex_auth() {
        let rows = super::field_shape(super::FieldSection::ModelProvider, "openai-codex");
        let auth = rows
            .iter()
            .find(|row| row.key == "auth_mode")
            .expect("auth_mode row");
        assert_eq!(auth.default.as_deref(), Some("codex"));
    }

    #[test]
    fn field_shape_anthropic_includes_claude_auth_mode() {
        let rows = super::field_shape(super::FieldSection::ModelProvider, "claude");
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert!(
            keys.contains(&"auth_mode"),
            "field_shape for claude/anthropic must include `auth_mode`; got {keys:?}",
        );
        let auth = rows
            .iter()
            .find(|row| row.key == "auth_mode")
            .expect("auth_mode row");
        assert!(auth.required);
        assert_eq!(
            auth.enum_variants.as_deref(),
            Some(["api_key".to_string(), "setup_token".to_string()].as_slice())
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.key == QUICKSTART_AUTH_MODE_FIELD)
                .count(),
            1,
            "Anthropic must expose one UI-facing auth selector"
        );
        assert_eq!(auth.default.as_deref(), Some("api_key"));
        let api_key = rows
            .iter()
            .find(|row| row.key == "api_key")
            .expect("api_key row");
        assert!(
            api_key.help.contains("claude setup-token"),
            "Anthropic API key help should mention setup-token flow; got {:?}",
            api_key.help
        );
    }

    /// `api_key` must be non-required in the Quickstart form so Codex
    /// subscription (no API key) and local providers (Ollama) can proceed
    /// without one.
    #[test]
    fn field_shape_api_key_is_not_required() {
        for kind in ["openai", "ollama"] {
            let rows = super::field_shape(super::FieldSection::ModelProvider, kind);
            let api_key_row = rows.iter().find(|r| r.key == "api_key");
            assert!(
                api_key_row.is_some(),
                "field_shape for `{kind}` must include `api_key`",
            );
            assert!(
                !api_key_row.unwrap().required,
                "`api_key` must be non-required for `{kind}` (Codex subscription / local providers don't need one)",
            );
        }
    }

    #[test]
    fn webhook_channel_quickstart_fields_include_port_and_secret() {
        let rows = super::field_shape(super::FieldSection::Channel, "webhook");
        let port = rows
            .iter()
            .find(|row| row.key == "port")
            .expect("webhook port row");
        assert_eq!(port.default.as_deref(), Some("8090"));
        assert!(port.required);
        assert!(!port.is_secret);

        let secret = rows
            .iter()
            .find(|row| row.key == "secret")
            .expect("webhook secret row");
        assert!(secret.required);
        assert!(secret.is_secret);
        assert_eq!(secret.default, None);
    }

    #[test]
    fn non_webhook_channel_quickstart_fields_exclude_webhook_requirements() {
        for channel_type in ["irc", "lark"] {
            let rows = super::field_shape(super::FieldSection::Channel, channel_type);
            assert!(
                rows.iter()
                    .all(|row| row.key != "port" && row.key != "secret"),
                "{channel_type} must not inherit webhook-only fields; got {rows:?}"
            );
        }
    }

    async fn apply_to_temp(submission: BuilderSubmission) -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let mut config = config;
        super::apply(submission, &mut config)
            .await
            .expect("apply should succeed");
        (dir, config)
    }

    fn reload(dir: &tempfile::TempDir) -> Config {
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        toml::from_str(&raw).expect("on-disk config must round-trip")
    }

    #[tokio::test]
    async fn fresh_preset_profiles_persist_to_disk() {
        let (dir, applied) = apply_to_temp(fresh_submission("bot")).await;
        assert!(applied.risk_profiles.contains_key("balanced"));
        assert!(applied.runtime_profiles.contains_key("balanced"));
        let reloaded = reload(&dir);
        assert!(
            reloaded.risk_profiles.contains_key("balanced"),
            "risk_profiles.balanced must survive save_dirty + reload, not dangle"
        );
        assert!(
            reloaded.runtime_profiles.contains_key("balanced"),
            "runtime_profiles.balanced must survive save_dirty + reload, not dangle"
        );
        let agent = reloaded.agents.get("bot").expect("agent persisted");
        assert_eq!(agent.risk_profile, "balanced");
        assert_eq!(agent.runtime_profile, "balanced");
    }

    #[tokio::test]
    async fn web_setup_token_persists_same_alias_profile_without_inline_key() {
        let mut submission = fresh_submission("bot");
        let SelectorChoice::Fresh(choice) = &mut submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        choice.alias = "subscription".to_string();
        choice.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "sk-ant-oat01-test-token".to_string()),
        ]);

        let (dir, config) = apply_to_temp(submission).await;
        let entry = config
            .providers
            .models
            .find("anthropic", "subscription")
            .expect("configured Anthropic alias");
        assert_eq!(entry.api_key, None);
        let auth = zeroclaw_providers::auth::AuthService::new(dir.path(), false);
        let profile = auth
            .get_profile("anthropic", Some("subscription"))
            .await
            .expect("load auth profile")
            .expect("same-alias profile");
        assert_eq!(profile.token.as_deref(), Some("sk-ant-oat01-test-token"));
        assert!(
            !auth
                .load_profiles()
                .await
                .expect("load profile state")
                .active_profiles
                .contains_key("anthropic"),
            "Quickstart alias storage must not change Anthropic's active profile"
        );
    }

    #[tokio::test]
    async fn shared_quickstart_state_preserves_web_and_tui_setup_submissions() {
        use tokio::time::{Duration, timeout};

        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let state = QuickstartConfigState::new(config);

        let mut web_submission = fresh_submission("web_bot");
        let SelectorChoice::Fresh(web_provider) = &mut web_submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        web_provider.alias = "web_subscription".to_string();
        web_provider.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "synthetic-web-token".to_string()),
        ]);

        let mut tui_submission = fresh_submission("tui_bot");
        let SelectorChoice::Fresh(tui_provider) = &mut tui_submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        tui_provider.alias = "tui_subscription".to_string();
        tui_provider.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "synthetic-tui-token".to_string()),
        ]);

        // Hold the shared transaction lock first, so both surface tasks prove
        // they wait before cloning the configuration they will persist.
        let held_transaction = state.write_lock().lock_owned().await;
        let web_state = state.clone();
        let mut web_task =
            zeroclaw_spawn::spawn!(
                async move { web_state.apply(web_submission, Surface::Web).await }
            );
        let tui_state = state.clone();
        let mut tui_task =
            zeroclaw_spawn::spawn!(
                async move { tui_state.apply(tui_submission, Surface::Tui).await }
            );
        assert!(
            timeout(Duration::from_millis(50), &mut web_task)
                .await
                .is_err(),
            "Web Quickstart must wait for the shared transaction"
        );
        assert!(
            timeout(Duration::from_millis(50), &mut tui_task)
                .await
                .is_err(),
            "TUI Quickstart must wait for the shared transaction"
        );
        drop(held_transaction);

        web_task
            .await
            .expect("Web Quickstart task must complete")
            .expect("Web Quickstart submission must apply");
        tui_task
            .await
            .expect("TUI Quickstart task must complete")
            .expect("TUI Quickstart submission must apply");

        let in_memory = state.config().read().clone();
        let reloaded = reload(&dir);
        for config in [&in_memory, &reloaded] {
            assert!(config.agents.contains_key("web_bot"));
            assert!(config.agents.contains_key("tui_bot"));
            assert!(
                config
                    .providers
                    .models
                    .find("anthropic", "web_subscription")
                    .is_some()
            );
            assert!(
                config
                    .providers
                    .models
                    .find("anthropic", "tui_subscription")
                    .is_some()
            );
        }
        let auth = zeroclaw_providers::auth::AuthService::new(dir.path(), false);
        assert_eq!(
            auth.get_profile("anthropic", Some("web_subscription"))
                .await
                .unwrap()
                .and_then(|profile| profile.token),
            Some("synthetic-web-token".to_string())
        );
        assert_eq!(
            auth.get_profile("anthropic", Some("tui_subscription"))
                .await
                .unwrap()
                .and_then(|profile| profile.token),
            Some("synthetic-tui-token".to_string())
        );
    }

    #[tokio::test]
    async fn personality_commit_failure_is_a_successful_setup_warning() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let workspace = config.agent_workspace_dir("bot");
        std::fs::create_dir_all(workspace.join("SOUL.md")).unwrap();

        let mut submission = fresh_submission("bot");
        let SelectorChoice::Fresh(choice) = &mut submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        choice.alias = "subscription".to_string();
        choice.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "synthetic-setup-token".to_string()),
        ]);
        submission.agent.personality_files =
            vec![zeroclaw_config::presets::QuickstartPersonalityFile {
                filename: "SOUL.md".to_string(),
                content: "synthetic personality".to_string(),
            }];

        let outcome = super::apply_with_surface_outcome(submission, &mut config, Surface::Web)
            .await
            .expect("personality finalization must not undo durable setup");
        assert_eq!(outcome.agent.alias, "bot");
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(outcome.warnings[0].field, "personality_files");
        assert!(
            config.agents.contains_key("bot"),
            "the in-memory configuration must remain committed"
        );
        assert!(
            config
                .providers
                .models
                .find("anthropic", "subscription")
                .is_some(),
            "the OAuth alias must remain committed"
        );
        let profile = zeroclaw_providers::auth::AuthService::from_config(&config)
            .get_profile("anthropic", Some("subscription"))
            .await
            .unwrap()
            .expect("the OAuth profile must remain committed");
        assert_eq!(profile.token.as_deref(), Some("synthetic-setup-token"));
    }

    #[tokio::test]
    async fn committed_config_warning_keeps_oauth_profile_and_alias() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        let original_config = config.clone();
        let mut submission = fresh_submission("bot");
        let SelectorChoice::Fresh(choice) = &mut submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        choice.alias = "subscription".to_string();
        choice.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "synthetic-setup-token".to_string()),
        ]);
        let mut staged_files = Vec::new();
        let mut errors = Vec::new();
        super::apply_into(
            &mut config,
            &submission,
            &mut staged_files,
            &mut errors,
            None,
        );
        assert!(errors.is_empty(), "setup submission must apply: {errors:?}");

        let auth = zeroclaw_providers::auth::AuthService::from_config(&config);
        let staged = auth
            .stage_model_provider_token(
                "anthropic",
                "subscription",
                "synthetic-setup-token",
                std::collections::HashMap::from([(
                    "auth_kind".to_string(),
                    "authorization".to_string(),
                )]),
            )
            .await
            .unwrap();
        let warnings = super::reconcile_config_save_outcome(
            Ok(
                zeroclaw_config::schema::ConfigSaveOutcome::CommittedWithDurabilityWarning(
                    anyhow::Error::msg("injected directory sync failure"),
                ),
            ),
            Some((
                auth.clone(),
                "subscription".to_string(),
                staged.snapshot,
                staged.staged_profile,
                staged.save_outcome,
            )),
            &mut config,
            original_config,
            &super::RunCtx::new(Surface::Web),
        )
        .await
        .expect("a committed config warning must not trigger OAuth rollback");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "config");
        assert!(
            config
                .providers
                .models
                .find("anthropic", "subscription")
                .is_some(),
            "the committed OAuth alias must remain configured"
        );
        let profile = auth
            .get_profile("anthropic", Some("subscription"))
            .await
            .unwrap()
            .expect("the committed OAuth profile must remain stored");
        assert_eq!(profile.token.as_deref(), Some("synthetic-setup-token"));
        assert!(
            !auth
                .load_profiles()
                .await
                .unwrap()
                .active_profiles
                .contains_key("anthropic"),
            "the committed warning path must not mutate the active profile selector"
        );
    }

    #[tokio::test]
    async fn committed_profile_warning_is_returned_without_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        let original_config = config.clone();
        let auth = zeroclaw_providers::auth::AuthService::from_config(&config);
        let staged = auth
            .stage_model_provider_token(
                "anthropic",
                "subscription",
                "synthetic-setup-token",
                std::collections::HashMap::new(),
            )
            .await
            .unwrap();

        let warnings = super::reconcile_config_save_outcome(
            Ok(zeroclaw_config::schema::ConfigSaveOutcome::Durable),
            Some((
                auth.clone(),
                "subscription".to_string(),
                staged.snapshot,
                staged.staged_profile,
                zeroclaw_providers::auth::profiles::ProfileSaveOutcome::CommittedWithDurabilityWarning(
                    anyhow::Error::msg("injected directory sync failure"),
                ),
            )),
            &mut config,
            original_config,
            &super::RunCtx::new(Surface::Web),
        )
        .await
        .expect("a committed profile warning must not trigger OAuth rollback");

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "api_key");
        assert_eq!(
            auth.get_profile("anthropic", Some("subscription"))
                .await
                .unwrap()
                .expect("the committed OAuth profile must remain stored")
                .token
                .as_deref(),
            Some("synthetic-setup-token")
        );
    }

    #[tokio::test]
    async fn setup_token_config_write_failure_restores_profile_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let original = std::fs::read_to_string(&config.config_path).unwrap();

        let auth = zeroclaw_providers::auth::AuthService::from_config(&config);
        auth.store_model_provider_token(
            "anthropic",
            "subscription",
            "existing-token",
            std::collections::HashMap::new(),
            false,
        )
        .await
        .expect("store prior profile");

        // `save_dirty` must fail after the profile has been staged but before
        // it can commit the OAuth alias.
        std::fs::remove_file(&config.config_path).unwrap();
        std::fs::create_dir(&config.config_path).unwrap();

        let mut submission = fresh_submission("bot");
        let SelectorChoice::Fresh(choice) = &mut submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        choice.alias = "subscription".to_string();
        choice.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "new-setup-token".to_string()),
        ]);

        let errors = super::apply_with_surface(submission, &mut config, Surface::Cli)
            .await
            .expect_err("config write must fail");
        let error = errors
            .iter()
            .find(|error| error.message.contains("persist config"))
            .expect("CLI persistence error");
        assert_eq!(
            error.message.matches("failed to persist config").count(),
            1,
            "the localized prefix must not be duplicated"
        );
        let profile = auth
            .get_profile("anthropic", Some("subscription"))
            .await
            .expect("read restored profile")
            .expect("prior profile must remain");
        assert_eq!(profile.token.as_deref(), Some("existing-token"));
        assert!(
            config
                .providers
                .models
                .find("anthropic", "subscription")
                .is_none()
        );
        std::fs::remove_dir(&config.config_path).unwrap();
        std::fs::write(&config.config_path, original).unwrap();
    }

    #[tokio::test]
    async fn setup_token_profile_prepare_failure_does_not_change_config() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state");
        std::fs::write(&state_path, "not a directory").unwrap();
        let mut config = Config {
            config_path: state_path.join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };

        let mut submission = fresh_submission("bot");
        let SelectorChoice::Fresh(choice) = &mut submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        choice.alias = "subscription".to_string();
        choice.fields = std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "new-setup-token".to_string()),
        ]);

        let errors = super::apply(submission, &mut config)
            .await
            .expect_err("profile snapshot must fail when its state directory is invalid");
        assert!(errors.iter().any(|error| error.field == "api_key"));
        assert!(
            config
                .providers
                .models
                .find("anthropic", "subscription")
                .is_none(),
            "failed profile preparation must not leave OAuth config in memory"
        );
    }

    #[tokio::test]
    async fn existing_runtime_profile_is_reused_without_writing_preset() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.runtime_profiles.insert(
            "small-laptop".into(),
            zeroclaw_config::schema::RuntimeProfileConfig {
                max_tool_iterations: 2,
                ..Default::default()
            },
        );
        config.save().await.unwrap();

        let mut submission = fresh_submission("bot");
        submission.runtime_profile = SelectorChoice::Existing("small-laptop".into());
        super::apply(submission, &mut config)
            .await
            .expect("apply should reuse existing runtime profile");

        let reloaded = reload(&dir);
        assert!(
            reloaded.runtime_profiles.contains_key("small-laptop"),
            "existing runtime profile must stay configured"
        );
        assert!(
            !reloaded.runtime_profiles.contains_key("balanced"),
            "existing runtime profile choice must not write the fresh preset"
        );
        let agent = reloaded.agents.get("bot").expect("agent persisted");
        assert_eq!(agent.runtime_profile, "small-laptop");
        assert_eq!(
            reloaded
                .runtime_profiles
                .get("small-laptop")
                .expect("existing profile persisted")
                .max_tool_iterations,
            2,
            "existing profile values must not be clobbered"
        );
    }

    #[tokio::test]
    async fn multiple_channels_all_bind_to_agent() {
        let mut submission = fresh_submission("bot");
        submission.channels = vec![
            SelectorChoice::Fresh(fresh_channel("telegram", "tg", &[("bot_token", "tok-a")])),
            SelectorChoice::Fresh(fresh_channel("discord", "dc", &[("bot_token", "tok-b")])),
        ];
        let (dir, _applied) = apply_to_temp(submission).await;
        let reloaded = reload(&dir);
        let agent = reloaded.agents.get("bot").expect("agent persisted");
        let bound: Vec<String> = agent.channels.iter().map(|c| c.to_string()).collect();
        assert!(
            bound.iter().any(|c| c.contains("tg")),
            "first channel must stay bound; got {bound:?}"
        );
        assert!(
            bound.iter().any(|c| c.contains("dc")),
            "second channel must also be bound; got {bound:?}"
        );
        assert_eq!(bound.len(), 2, "both channels bound, not just the last");
        let store = zeroclaw_config::secrets::SecretStore::new(dir.path(), true);
        assert_eq!(
            store
                .decrypt(&reloaded.channels.telegram["tg"].bot_token)
                .unwrap(),
            "tok-a"
        );
        assert!(reloaded.channels.telegram["tg"].enabled);
        assert_eq!(
            store
                .decrypt(&reloaded.channels.discord["dc"].bot_token)
                .unwrap(),
            "tok-b"
        );
        assert!(reloaded.channels.discord["dc"].enabled);
    }

    #[tokio::test]
    async fn telegram_channel_fields_persist_canonical_bot_token() {
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "telegram",
            "ops",
            &[("bot_token", " 123:ABC ")],
        ))];

        let (dir, _) = apply_to_temp(submission).await;
        let reloaded = reload(&dir);
        let store = zeroclaw_config::secrets::SecretStore::new(dir.path(), true);
        assert_eq!(
            store
                .decrypt(&reloaded.channels.telegram["ops"].bot_token)
                .unwrap(),
            "123:ABC"
        );
        assert!(reloaded.channels.telegram["ops"].enabled);
    }

    #[tokio::test]
    async fn webhook_channel_quickstart_fields_persist() {
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "webhook",
            "inbound",
            &[("port", "9191"), ("secret", "shared-secret")],
        ))];

        let (dir, _) = apply_to_temp(submission).await;
        let reloaded = reload(&dir);
        let webhook = &reloaded.channels.webhook["inbound"];
        assert_eq!(webhook.port, 9191);
        assert!(webhook.enabled);

        let stored_secret = webhook.secret.as_deref().expect("persisted webhook secret");
        assert!(zeroclaw_config::secrets::SecretStore::is_encrypted(
            stored_secret
        ));
        let store = zeroclaw_config::secrets::SecretStore::new(dir.path(), true);
        assert_eq!(store.decrypt(stored_secret).unwrap(), "shared-secret");
    }

    #[test]
    fn webhook_channel_defaults_port_and_rejects_unusable_secret() {
        for value in [
            None,
            Some(""),
            Some("   "),
            Some(zeroclaw_config::traits::UNSET_DISPLAY),
        ] {
            let mut cfg = Config::default();
            let fields = value.map_or_else(Vec::new, |value| vec![("secret", value)]);
            let channels = vec![SelectorChoice::Fresh(fresh_channel(
                "webhook", "inbound", &fields,
            ))];
            let mut errors = Vec::new();

            let refs = apply_channels(&mut cfg, &channels, &mut errors, None);

            assert!(refs.is_empty());
            assert!(errors.iter().any(|error| {
                error.field == "channels[0].fields.secret" && error.message.contains("required")
            }));
            assert!(!cfg.channels.webhook.contains_key("inbound"));
        }

        let mut cfg = Config::default();
        let channels = vec![SelectorChoice::Fresh(fresh_channel(
            "webhook",
            "inbound",
            &[("secret", "shared-secret")],
        ))];
        let mut errors = Vec::new();

        let refs = apply_channels(&mut cfg, &channels, &mut errors, None);

        assert!(errors.is_empty(), "apply errors: {errors:?}");
        assert_eq!(refs, ["webhook.inbound"]);
        assert_eq!(
            cfg.channels.webhook["inbound"].port,
            zeroclaw_config::schema::DEFAULT_WEBHOOK_CHANNEL_PORT
        );
    }

    #[test]
    fn webhook_channel_cli_reports_webhook_secret_error() {
        let cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "webhook",
            "inbound",
            &[("secret", "")],
        ))];

        let errors = validate_only_with_surface(&submission, &cfg, Surface::Cli)
            .expect_err("empty webhook secret must be rejected");

        assert!(errors.iter().any(|error| {
            error.field == "channels[0].fields.secret"
                && error.message == "Webhook shared secret is required"
        }));
    }

    #[test]
    fn telegram_channel_fields_reject_unusable_bot_token_values() {
        for value in [
            None,
            Some(""),
            Some("   "),
            Some(zeroclaw_config::traits::UNSET_DISPLAY),
        ] {
            let cfg = Config::default();
            let mut submission = fresh_submission("bot");
            let fields = value.map_or_else(Vec::new, |value| vec![("bot_token", value)]);
            submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
                "telegram", "ops", &fields,
            ))];

            let errors = validate_only(&submission, &cfg).expect_err("token must be rejected");
            assert!(errors.iter().any(|error| {
                error.step == QuickstartStep::Channels
                    && error.field == "channels[0].fields.bot_token"
                    && error.message.contains("required")
            }));
        }
    }

    #[tokio::test]
    async fn shared_reload_admission_rejects_queued_cross_surface_apply_until_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let state = QuickstartConfigState::new(config);
        let web_state = QuickstartConfigState::from_parts(
            state.config(),
            state.write_lock(),
            state.reload_admission(),
        );
        let tui_state = QuickstartConfigState::from_parts(
            state.config(),
            state.write_lock(),
            state.reload_admission(),
        );

        let mut web_submission = fresh_submission("web_bot");
        let SelectorChoice::Fresh(web_provider) = &mut web_submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        web_provider.alias = "web_provider".to_string();
        let mut tui_submission = fresh_submission("tui_bot");
        let SelectorChoice::Fresh(tui_provider) = &mut tui_submission.model_provider else {
            panic!("fresh submission must create a provider");
        };
        tui_provider.alias = "tui_provider".to_string();

        web_state
            .apply_and_admit_reload(web_submission, Surface::Web)
            .await
            .expect("first supervised Quickstart apply must commit");

        let errors = tui_state
            .apply_and_admit_reload(tui_submission.clone(), Surface::Tui)
            .await
            .expect_err("queued TUI apply must not commit while Web reload is pending");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "reload");
        assert!(
            !state.config().read().agents.contains_key("tui_bot"),
            "rejected queued apply must not modify the outgoing daemon configuration"
        );

        web_state.cancel_reload_admission();
        tui_state
            .apply_and_admit_reload(tui_submission, Surface::Tui)
            .await
            .expect("standalone caller cancellation must permit a later apply");
    }

    #[test]
    fn discord_channel_fields_reject_unusable_bot_token_values() {
        // Discord twin of telegram_channel_fields_reject_unusable_bot_token_values:
        // the quickstart arm calls DiscordConfig::validate_bot_token.
        for value in [
            None,
            Some(""),
            Some("   "),
            Some(zeroclaw_config::traits::UNSET_DISPLAY),
        ] {
            let cfg = Config::default();
            let mut submission = fresh_submission("bot");
            let fields = value.map_or_else(Vec::new, |value| vec![("bot_token", value)]);
            submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
                "discord", "ops", &fields,
            ))];

            let errors = validate_only(&submission, &cfg).expect_err("token must be rejected");
            assert!(errors.iter().any(|error| {
                error.step == QuickstartStep::Channels
                    && error.field == "channels[0].fields.bot_token"
                    && error.message.contains("required")
            }));
        }
    }

    #[test]
    fn channel_fields_reject_unknown_keys_without_exposing_values() {
        let mut cfg = Config::default();
        let before = serde_json::to_value(&cfg).expect("serialize config");
        let before_dirty_paths = cfg.dirty_paths.clone();
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "discord",
            "ops",
            &[("unknown_secret", "super-secret-value")],
        ))];
        let mut errors = Vec::new();

        let refs = apply_channels(&mut cfg, &submission.channels, &mut errors, None);

        assert!(refs.is_empty());
        let error = errors
            .iter()
            .find(|error| error.field == "channels[0].fields.unknown_secret")
            .expect("structured unknown-field error");
        assert!(!error.message.contains("super-secret-value"));
        assert_eq!(
            serde_json::to_value(&cfg).expect("serialize config"),
            before
        );
        assert_eq!(cfg.dirty_paths, before_dirty_paths);
    }

    #[test]
    fn channel_fields_reject_valid_but_unadvertised_schema_keys() {
        let mut cfg = Config::default();
        let before = serde_json::to_value(&cfg).expect("serialize config");
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "telegram",
            "ops",
            &[
                ("bot_token", "123:ABC"),
                ("api_base_url", "https://example.invalid"),
            ],
        ))];
        let mut errors = Vec::new();

        let refs = apply_channels(&mut cfg, &submission.channels, &mut errors, None);

        assert!(refs.is_empty());
        assert!(errors.iter().any(|error| {
            error.field == "channels[0].fields.api_base_url"
                && error.message.contains("not available in Quickstart")
        }));
        assert_eq!(
            serde_json::to_value(&cfg).expect("serialize config"),
            before
        );
    }

    #[test]
    fn channel_fields_materialize_credential_free_channel() {
        let mut cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "imessage",
            "local",
            &[],
        ))];
        let mut staged = Vec::new();
        let mut errors = Vec::new();

        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);

        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        assert!(channel_exists(&cfg, "imessage", "local"));
    }

    #[tokio::test]
    async fn fresh_whatsapp_web_channel_persists_under_whatsapp_config_family() {
        for submitted_type in ["whatsapp-web", "whatsapp_web"] {
            let mut submission = fresh_submission("bot");
            submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
                submitted_type,
                "personal",
                &[],
            ))];
            submission.peer_groups = vec![zeroclaw_config::presets::QuickstartPeerGroup {
                name: "self_chat".into(),
                channel: format!("{submitted_type}.personal"),
                external_peers: vec!["*".into()],
                ignore: vec![],
            }];

            let (dir, _applied) = apply_to_temp(submission).await;
            let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
            assert!(
                raw.contains("[channels.whatsapp.personal]"),
                "WhatsApp Web quickstart must persist under the canonical WhatsApp config family:\n{raw}"
            );
            assert!(
                !raw.contains("[channels.whatsapp-web.personal]")
                    && !raw.contains("[channels.whatsapp_web.personal]"),
                "Quickstart must not write a non-schema WhatsApp Web table:\n{raw}"
            );

            let reloaded = reload(&dir);
            let whatsapp = reloaded
                .channels
                .whatsapp
                .get("personal")
                .expect("canonical WhatsApp alias persisted");
            let expected_session = dir
                .path()
                .join("state")
                .join("whatsapp-web")
                .join("personal.db")
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                whatsapp.session_path.as_deref(),
                Some(expected_session.as_str())
            );
            assert!(
                whatsapp.is_web_config(),
                "fresh WhatsApp Web entry must seed a Web selector"
            );
            let agent = reloaded.agents.get("bot").expect("agent persisted");
            let bound: Vec<String> = agent.channels.iter().map(|c| c.to_string()).collect();
            assert_eq!(
                bound,
                vec!["whatsapp.personal".to_string()],
                "agent must bind the canonical channel ref"
            );
            let group = reloaded
                .peer_groups
                .get("self_chat")
                .expect("peer group persisted");
            assert_eq!(group.channel, "whatsapp.personal");
        }
    }

    #[tokio::test]
    async fn peer_groups_persist_to_canonical_section() {
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "telegram",
            "tg",
            &[("bot_token", "tok-a")],
        ))];
        submission.peer_groups = vec![zeroclaw_config::presets::QuickstartPeerGroup {
            name: "team".into(),
            channel: "telegram.tg".into(),
            external_peers: vec!["*".into()],
            ignore: vec![],
        }];

        let (dir, _applied) = apply_to_temp(submission).await;
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            raw.contains("[peer_groups.team]"),
            "Quickstart must serialize peer groups through canonical snake_case paths:\n{raw}"
        );
        assert!(
            !raw.contains("[peer-groups.team]"),
            "Quickstart must not write the stale kebab-case peer-groups path:\n{raw}"
        );

        let reloaded = reload(&dir);
        let group = reloaded
            .peer_groups
            .get("team")
            .expect("peer group persisted");
        assert_eq!(group.channel, "telegram.tg");
        assert_eq!(group.external_peers, vec!["*".to_string()]);
    }

    #[tokio::test]
    async fn model_catalog_with_config_uses_native_endpoint_when_credentialed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Native /models endpoint advertising a model that is NOT in the
        // static models.dev snapshot (a freshly released Grok).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "grok-4.5-native-only"},
                    {"id": "grok-4.3"}
                ]
            })))
            .mount(&server)
            .await;

        let mut config = Config::default();
        config.providers.models.xai.insert(
            "default".to_string(),
            zeroclaw_config::schema::XaiModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    api_key: Some("xai-test-key".to_string()),
                    uri: Some(server.uri()),
                    ..Default::default()
                },
            },
        );

        let (models, _pricing, live) =
            model_catalog_with_config(Some(&config), "xai.default").await;

        assert!(live, "credentialed native listing must report live=true");
        assert!(
            models.iter().any(|m| m == "grok-4.5-native-only"),
            "native /models result must surface the freshly-released model \
             that models.dev does not carry; got {models:?}"
        );
    }

    #[tokio::test]
    async fn model_catalog_with_config_resolves_named_alias_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "grok-named-alias-native"}]
            })))
            .mount(&server)
            .await;

        let mut config = Config::default();
        // Non-default alias name to prove the dotted ref targets it precisely.
        config.providers.models.xai.insert(
            "prod".to_string(),
            zeroclaw_config::schema::XaiModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    api_key: Some("xai-test-key".to_string()),
                    uri: Some(server.uri()),
                    ..Default::default()
                },
            },
        );

        let (models, _pricing, live) = model_catalog_with_config(Some(&config), "xai.prod").await;

        assert!(live);
        assert!(
            models.iter().any(|m| m == "grok-named-alias-native"),
            "dotted `<family>.<alias>` selector must resolve that alias's \
             configured endpoint; got {models:?}"
        );
    }
}
