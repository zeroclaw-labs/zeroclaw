//! Auth-first bootstrap for a separately owned native-provider instance.
//! Config/Quickstart own executable state; the receipt records this transaction
//! and its last observation, never grants runtime authority.

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, MemoryChoice, SelectorChoice, risk_preset,
};
use zeroclaw_config::schema::Config;
use zeroclaw_providers::auth::{self, AuthFlowContext, AuthService};
use zeroclaw_runtime::quickstart::{self, QuickstartApplyOutcome, Surface};

const RECEIPT: &str = "native-onboard.json";
const LEASE: &str = ".native-onboard.guard";
const READY_REPLY: &str = "NATIVE_ONBOARD_READY";
const PROOF_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Client {
    ChatgptPlan,
    #[value(skip)]
    ClaudeCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExpectedBilling {
    Subscription,
    Api,
    #[value(name = "cloud_or_gateway")]
    CloudOrGateway,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RiskPreset {
    Balanced,
    Yolo,
}

impl RiskPreset {
    fn name(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Yolo => "yolo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Args)]
pub(crate) struct Request {
    #[arg(long, value_enum)]
    pub client: Client,
    #[arg(long)]
    pub provider_alias: String,
    #[arg(long)]
    pub agent_alias: String,
    #[arg(long)]
    pub model: String,
    #[arg(long, value_enum)]
    pub risk_preset: RiskPreset,
    #[arg(long, value_enum)]
    pub expected_billing: ExpectedBilling,
    #[arg(long)]
    pub accept_yolo: bool,
    #[arg(long)]
    pub accept_api_billing: bool,
    #[arg(long)]
    pub native_config_dir: Option<PathBuf>,
    #[arg(long, default_value = "subscriber")]
    pub auth_profile: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    PendingAuth,
    Configured,
    Ready,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationObservation {
    at_unix_seconds: u64,
    model_provider: String,
    model: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    transaction_id: String,
    /// This directory, rather than a copied marker, was created by bootstrap.
    root_identity: Option<(u64, u64)>,
    request: Request,
    phase: Phase,
    /// Admission fingerprint for recovery after config committed but receipt
    /// did not. Executable policy always comes from freshly hydrated Config.
    prepared_config_digest: Option<String>,
    last_validation: Option<ValidationObservation>,
    /// Stable stage key only; never serialize vendor errors or credentials.
    failure_stage: Option<String>,
}

#[derive(Debug)]
struct CliFailure {
    key: String,
    message: String,
}

impl std::fmt::Display for CliFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliFailure {}

fn error(key: &str, fallback: &str) -> anyhow::Error {
    anyhow::Error::new(CliFailure {
        key: key.into(),
        message: crate::t(key, fallback),
    })
}

fn root_error() -> anyhow::Error {
    error(
        "cli-native-onboard-root-refused",
        "Native onboarding requires an absent directory or this command's matching owned fresh instance. No existing credentials or files were changed.",
    )
}

fn validate_request(request: &Request) -> Result<()> {
    if request.risk_preset == RiskPreset::Yolo && !request.accept_yolo {
        return Err(error(
            "cli-native-onboard-yolo-acceptance",
            "YOLO requires --accept-yolo on this invocation.",
        ));
    }
    if request.expected_billing == ExpectedBilling::Api && !request.accept_api_billing {
        return Err(error(
            "cli-native-onboard-api-acceptance",
            "API billing requires --accept-api-billing on this invocation.",
        ));
    }
    if request.client == Client::ClaudeCode {
        // The typed native family and its probe arrive in a separate dependent
        // layer. Never materialize an unknown family that serde could discard.
        return Err(error(
            "cli-native-onboard-claude-unavailable",
            "This build does not contain the native Claude Code onboarding provider. Use the dependent native-provider build; no instance was created.",
        ));
    }
    if request.expected_billing != ExpectedBilling::Subscription
        || request.native_config_dir.is_some()
    {
        return Err(error(
            "cli-native-onboard-plan-billing",
            "ChatGPT plan onboarding requires subscription billing and owns its instance auth store; --native-config-dir is unsupported.",
        ));
    }
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(error(
            "cli-native-onboard-platform",
            "Native onboarding currently requires macOS or Linux (including WSL).",
        ));
    }
    if request.model.trim().is_empty()
        || request.model.trim() != request.model
        || request.model.contains('|')
        || request.model.starts_with("hint:")
        || request.model.len() > 4096
        || request.model.chars().any(char::is_control)
    {
        return Err(error(
            "cli-native-onboard-arguments",
            "Provide a nonempty model and valid provider, agent, and auth-profile aliases.",
        ));
    }
    // Ask the canonical alias API to validate each name, without persisting.
    let mut names = Config::default();
    for (section, alias) in [
        ("providers.models.openai", &request.provider_alias),
        ("agents", &request.agent_alias),
        ("providers.models.openai", &request.auth_profile),
    ] {
        zeroclaw_config::alias_refs::create_map_key_checked(&mut names, section, alias).map_err(
            |_| {
                error(
                    "cli-native-onboard-arguments",
                    "Provide a nonempty model and valid provider, agent, and auth-profile aliases.",
                )
            },
        )?;
    }
    Ok(())
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    options
}

fn check_private(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "bootstrap state is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            metadata.mode().trailing_zeros() >= 6 && metadata.uid() == unsafe { libc::geteuid() },
            "bootstrap state is not privately owned"
        );
    }
    Ok(())
}

fn read_receipt(root: &Path) -> Result<Receipt> {
    let mut file = private_options().read(true).open(root.join(RECEIPT))?;
    check_private(&file)?;
    anyhow::ensure!(
        file.metadata()?.len() < 64 * 1024,
        "bootstrap receipt is oversized"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_receipt(root: &Path, receipt: &Receipt) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(root.join(RECEIPT)) {
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "bootstrap receipt is not a regular leaf"
        );
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".native-onboard-receipt.")
        .tempfile_in(root)?;
    temporary.write_all(&serde_json::to_vec_pretty(receipt)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(root.join(RECEIPT))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

/// Publish an already-owned directory without ever replacing a racing empty
/// directory. Unsupported platforms/filesystems refuse rather than weakening
/// the exclusive fresh-root contract to check-then-rename.
fn publish_root(staged: &Path, destination: &Path) -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(staged.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        // SAFETY: both CStrings are terminated and live through the syscall.
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        // SAFETY: both CStrings are terminated and live through the syscall.
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("exclusive bootstrap root publication failed");
        }
        File::open(
            destination
                .parent()
                .context("bootstrap root has no parent")?,
        )?
        .sync_all()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(root_error())
    }
}

struct OwnedRoot {
    root: PathBuf,
    lease: File,
    receipt: Receipt,
}

impl OwnedRoot {
    fn acquire(path: &Path, request: &Request) -> Result<Self> {
        anyhow::ensure!(
            path.is_absolute()
                && !path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)),
            "bootstrap root must be an absolute path without parent traversal"
        );
        let parent = std::fs::canonicalize(path.parent().context("bootstrap root has no parent")?)?;
        let root = parent.join(path.file_name().context("bootstrap root has no name")?);
        let (lease, receipt) = match std::fs::symlink_metadata(&root) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "bootstrap root is not a directory leaf"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.mode().trailing_zeros() >= 6
                            && metadata.uid() == unsafe { libc::geteuid() },
                        "bootstrap root is not privately owned"
                    );
                }
                // Open existing state only; never create a lease in somebody
                // else's precreated directory just to discover it is unowned.
                let lease = private_options()
                    .read(true)
                    .write(true)
                    .open(root.join(LEASE))?;
                check_private(&lease)?;
                lease.try_lock().map_err(|_| root_error())?;
                let receipt = read_receipt(&root)?;
                anyhow::ensure!(
                    receipt.schema_version == 1
                        && receipt.request == *request
                        && !receipt.transaction_id.is_empty(),
                    "bootstrap ownership/request mismatch"
                );
                (lease, receipt)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let staged = tempfile::Builder::new()
                    .prefix(".native-onboard.")
                    .tempdir_in(&parent)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        staged.path(),
                        std::fs::Permissions::from_mode(0o700),
                    )?;
                }
                let lease = private_options()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(staged.path().join(LEASE))?;
                lease.try_lock()?;
                let receipt = Receipt {
                    schema_version: 1,
                    transaction_id: staged
                        .path()
                        .file_name()
                        .context("staged root has no name")?
                        .to_string_lossy()
                        .into_owned(),
                    root_identity: {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            let metadata = std::fs::symlink_metadata(staged.path())?;
                            Some((metadata.dev(), metadata.ino()))
                        }
                        #[cfg(not(unix))]
                        {
                            None
                        }
                    },
                    request: request.clone(),
                    phase: Phase::PendingAuth,
                    prepared_config_digest: None,
                    last_validation: None,
                    failure_stage: None,
                };
                write_receipt(staged.path(), &receipt)?;
                publish_root(staged.path(), &root)?;
                // Ownership transferred atomically. TempDir's old sibling path
                // no longer exists; no cleanup ever targets the published root.
                let _ = staged.keep();
                (lease, receipt)
            }
            Err(error) => return Err(error.into()),
        };
        let owned = Self {
            root,
            lease,
            receipt,
        };
        owned.check_identity()?;
        Ok(owned)
    }

    fn check_identity(&self) -> Result<()> {
        let metadata = std::fs::symlink_metadata(&self.root)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "bootstrap root changed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            anyhow::ensure!(
                Some((metadata.dev(), metadata.ino())) == self.receipt.root_identity,
                "bootstrap root identity changed"
            );
            let held = self.lease.metadata()?;
            let named = std::fs::symlink_metadata(self.root.join(LEASE))?;
            anyhow::ensure!(
                named.is_file() && (held.dev(), held.ino()) == (named.dev(), named.ino()),
                "bootstrap lease identity changed"
            );
        }
        Ok(())
    }

    fn transition(&mut self, phase: Phase, failure: Option<&str>) -> Result<()> {
        self.check_identity()?;
        if phase == Phase::Ready {
            anyhow::ensure!(
                self.receipt.prepared_config_digest.is_some()
                    && self
                        .receipt
                        .last_validation
                        .as_ref()
                        .is_some_and(|observation| observation.at_unix_seconds > 0
                            && observation.model == self.receipt.request.model
                            && !observation.model_provider.is_empty()),
                "ready requires a completed validation observation"
            );
        }
        self.receipt.phase = phase;
        self.receipt.failure_stage = failure.map(str::to_owned);
        if phase != Phase::Ready {
            self.receipt.last_validation = None;
        }
        write_receipt(&self.root, &self.receipt)
    }
}

fn ensure_owned_path(root: &Path, path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.starts_with(root),
        "bootstrap path escapes its instance"
    );
    let relative = path.strip_prefix(root)?;
    let mut ancestor = root.to_path_buf();
    for part in relative.components() {
        anyhow::ensure!(
            matches!(part, Component::Normal(_)),
            "bootstrap path traverses its instance"
        );
        ancestor.push(part);
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "bootstrap path is a symlink"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct SourceModelFields {
    family: &'static str,
    fields: Vec<(&'static str, String)>,
}

impl SourceModelFields {
    fn for_request(request: &Request) -> Self {
        // The request validator refuses unavailable native Claude before root
        // acquisition. Its dependent layer supplies a typed family here.
        Self {
            family: "openai",
            fields: vec![
                ("kind", "chatgpt-plan".into()),
                ("model", request.model.clone()),
                ("wire_api", "responses".into()),
                (
                    "chatgpt_plan_auth.registration",
                    format!("chatgpt-plan:{}", request.auth_profile),
                ),
            ],
        }
    }

    fn materialize(&self, config: &mut Config, request: &Request) -> Result<String> {
        let section = format!("providers.models.{}", self.family);
        zeroclaw_config::alias_refs::create_map_key_checked(
            config,
            &section,
            &request.provider_alias,
        )?;
        let prefix = format!("{section}.{}", request.provider_alias);
        for (field, value) in &self.fields {
            config.set_prop(&format!("{prefix}.{field}"), value)?;
        }
        Ok(format!("{}.{}", self.family, request.provider_alias))
    }
}

fn config_digest(config: &Config) -> Result<String> {
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted = map
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>();
                serde_json::Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key, canonical(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            }
            other => other,
        }
    }
    let bytes = serde_json::to_vec(&canonical(serde_json::to_value(config)?))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn verify_configuration(config: &Config, owned: &OwnedRoot, provider: &str) -> Result<()> {
    owned.check_identity()?;
    let request = &owned.receipt.request;
    anyhow::ensure!(
        config.config_path == owned.root.join("config.toml")
            && config.data_dir == owned.root.join("data"),
        "bootstrap config/data root changed"
    );
    for path in [
        config.config_path.clone(),
        config.data_dir.clone(),
        config.agent_workspace_dir(&request.agent_alias),
        auth::state_dir_from_config(config),
    ] {
        ensure_owned_path(&owned.root, &path)?;
    }
    config.validate()?;
    zeroclaw_runtime::rpc::auth::validate_accepted_auth_config(config)?;
    let agent = config
        .agents
        .get(&request.agent_alias)
        .context("bootstrap agent missing")?;
    anyhow::ensure!(
        config.agents.len() == 1
            && agent.model_provider.as_str() == provider
            && (agent.classifier_provider.as_str().is_empty()
                || agent.classifier_provider.as_str() == provider)
            && config
                .effective_summary_provider(&request.agent_alias)
                .is_none_or(|reference| reference.as_str() == provider),
        "bootstrap auxiliary/provider binding changed"
    );
    anyhow::ensure!(
        !agent.workspace.unrestricted_filesystem
            && agent.workspace.access.is_empty()
            && agent.workspace.path.is_none()
            && agent.workspace.read_memory_from.is_empty()
            && agent.delegates.is_empty(),
        "bootstrap workspace/delegate binding changed"
    );
    anyhow::ensure!(
        config.memory.backend == "none"
            && config.memory.embedding_provider == "none"
            && config.memory.embedding_api_key.is_none()
            && config.embedding_routes.is_empty()
            && config.model_routes.is_empty()
            && config.decision_models.is_empty()
            && !config.query_classification.enabled
            && config.reliability.api_keys.is_empty(),
        "bootstrap auxiliary billing path is enabled"
    );
    let entry = config
        .model_provider_for_agent(&request.agent_alias)
        .context("bootstrap provider missing")?;
    anyhow::ensure!(
        entry.kind.as_deref() == Some("chatgpt-plan")
            && entry
                .chatgpt_plan_auth
                .as_ref()
                .is_some_and(|binding| binding.registration
                    == format!("chatgpt-plan:{}", request.auth_profile))
            && entry.model.as_deref() == Some(request.model.as_str())
            && entry.api_key.is_none()
            && !entry.requires_openai_auth
            && entry.fallback.is_empty()
            && entry.fallback_models.is_empty(),
        "bootstrap explicit grant changed"
    );
    let configured = config
        .providers
        .models
        .find("openai", &request.provider_alias);
    let options = zeroclaw_providers::model_provider_runtime_options_from_model_provider_entry(
        config, configured,
    );
    let _provider = zeroclaw_providers::create_model_provider_for_alias(
        config,
        "openai",
        &request.provider_alias,
        None,
        &options,
    )?;
    let expected = (risk_preset(request.risk_preset.name())
        .context("unknown bootstrap risk preset")?
        .values)();
    let risk = config
        .risk_profile_for_agent(&request.agent_alias)
        .context("bootstrap risk missing")?;
    anyhow::ensure!(
        serde_json::to_value(risk)? == serde_json::to_value(&expected)?,
        "bootstrap risk preset was changed"
    );
    let runtime = config
        .runtime_profile_for_agent(&request.agent_alias)
        .context("bootstrap runtime missing")?;
    let expected_runtime = (zeroclaw_config::presets::runtime_preset("balanced")
        .context("bootstrap runtime preset missing")?
        .values)();
    anyhow::ensure!(
        serde_json::to_value(runtime)? == serde_json::to_value(expected_runtime)?,
        "bootstrap runtime preset was changed"
    );
    let policy = zeroclaw_config::policy::SecurityPolicy::for_agent(config, &request.agent_alias)?;
    anyhow::ensure!(
        policy.autonomy == expected.level
            && policy.workspace_only == expected.workspace_only
            && policy.sandbox_enabled == expected.sandbox_enabled
            && policy.block_high_risk_commands == expected.block_high_risk_commands
            && policy.require_approval_for_medium_risk == expected.require_approval_for_medium_risk
            && policy.auto_approve == expected.auto_approve
            && policy.allowed_commands == expected.allowed_commands
            && policy.forbidden_paths == expected.forbidden_paths
            && policy.workspace_dir == config.agent_workspace_dir(&request.agent_alias),
        "bootstrap effective policy does not match the accepted preset"
    );
    Ok(())
}

fn private_candidate(root: &Path) -> Result<Config> {
    let mut candidate = Config {
        config_path: root.join("config.toml"),
        data_dir: root.join("data"),
        ..Config::default()
    };
    // Defaults whose paths are computed from the ambient install must be
    // rebased explicitly before any factory or auth path can consume them.
    for (field, path) in [
        ("plugins.plugins_dir", root.join("plugins")),
        ("knowledge.db_path", root.join("data/knowledge.db")),
        (
            "project_intel.report_output_dir",
            root.join("data/project-reports"),
        ),
    ] {
        candidate.set_prop(field, path.to_str().context("bootstrap path is not UTF-8")?)?;
    }
    candidate.set_prop("claude_code.enabled", "false")?;
    candidate.set_prop("channels.cli", "true")?;
    Ok(candidate)
}

async fn load_committed(owned: &OwnedRoot) -> Result<Config> {
    ensure_owned_path(&owned.root, &owned.root.join("config.toml"))?;
    let mut file = private_options()
        .read(true)
        .open(owned.root.join("config.toml"))?;
    check_private(&file)?;
    anyhow::ensure!(
        file.metadata()?.len() <= 4 * 1024 * 1024,
        "bootstrap config is oversized"
    );
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    let config = Box::pin(Config::prepare_from_migrated_toml(
        &raw,
        &owned.root.join("config.toml"),
        &owned.root.join("data"),
    ))
    .await?;
    anyhow::ensure!(
        owned.receipt.prepared_config_digest.as_deref() == Some(config_digest(&config)?.as_str()),
        "committed config does not match this bootstrap transaction"
    );
    Ok(config)
}

async fn authorize(config: &Config, request: &Request) -> Result<()> {
    let auth_service = AuthService::from_config(config);
    let binding = format!("chatgpt-plan:{}", request.auth_profile);
    // An already validated grant from a cancelled run can be resumed. The
    // canonical service rechecks scope, identity, expiry and uncertainty.
    if auth_service
        .get_valid_chatgpt_plan_access_token(&binding)
        .await
        .is_err()
    {
        let flow = auth::flow_for_model_provider("chatgpt-plan")?;
        let client = reqwest::Client::new();
        let format_cli =
            |key: &str, args: &[(&str, &str)], fallback: &str| crate::ta(key, args, fallback);
        let context = AuthFlowContext {
            config,
            auth_service: &auth_service,
            client: &client,
            format_cli: &format_cli,
        };
        flow.login(&context, &request.auth_profile, false, None)
            .await?;
    }
    let data = auth_service.load_profiles().await?;
    let profile = data
        .profiles
        .get(&binding)
        .context("validated bootstrap grant missing")?;
    anyhow::ensure!(
        profile.model_provider == "chatgpt-plan"
            && profile.kind == auth::profiles::AuthProfileKind::OAuth
            && profile
                .plan_registration
                .as_ref()
                .is_some_and(|identity| !identity.client_id.is_empty()
                    && !identity.subject.is_empty()
                    && identity.refresh_started_at.is_none()),
        "bootstrap grant identity is not validated"
    );
    auth_service
        .get_valid_chatgpt_plan_access_token(&binding)
        .await?;
    Ok(())
}

#[cfg(test)]
tokio::task_local! { static PAUSE_STAGE: (String, PathBuf); }

#[cfg(test)]
async fn pause_stage(stage: &str) {
    if let Ok((expected, ready)) = PAUSE_STAGE.try_with(Clone::clone)
        && expected == stage
    {
        std::fs::write(ready, stage).unwrap();
        std::future::pending::<()>().await;
    }
}

pub(crate) async fn run(root: Option<&str>, request: Request) -> Result<()> {
    validate_request(&request)?;
    let root = root.filter(|root| !root.trim().is_empty()).ok_or_else(|| error("cli-native-onboard-root-required", "Native onboarding requires an explicit absolute --config-dir for a separate fresh instance."))?;
    let mut owned = OwnedRoot::acquire(Path::new(root), &request).map_err(|_| root_error())?;
    let result = Box::pin(run_owned(&mut owned)).await;
    if let Err(failure) = result {
        // Preserve auth and any canonical config commit. Recovery never removes
        // credentials/files or overwrites an already committed config.
        let stage = failure
            .downcast_ref::<CliFailure>()
            .map_or("bootstrap", |failure| failure.key.as_str());
        owned.transition(Phase::Failed, Some(stage)).map_err(|_| error("cli-native-onboard-state-failed", "Native onboarding could not update its ownership receipt. Owned files were retained; no readiness was claimed."))?;
        return match failure.downcast::<CliFailure>() {
            Ok(failure) => Err(anyhow::Error::new(failure)),
            Err(_) => Err(error(
                "cli-native-onboard-failed",
                "Native onboarding validation failed. Owned auth/config state was retained; no readiness was claimed.",
            )),
        };
    }
    Ok(())
}

async fn run_owned(owned: &mut OwnedRoot) -> Result<()> {
    let request = owned.receipt.request.clone();
    ensure_owned_path(&owned.root, &owned.root.join("data"))?;
    let _config_ownership = zeroclaw_runtime::live_config_authority::ConfigOwnershipGuard::acquire(
        &owned.root.join("data"),
    )
    .map_err(|_| root_error())?;
    #[cfg(unix)]
    let mut cancellation =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let has_config = owned.root.join("config.toml").try_exists()?;
    let mut candidate = if has_config {
        Box::pin(load_committed(owned)).await?
    } else {
        private_candidate(&owned.root)?
    };
    owned.transition(
        if has_config {
            Phase::Configured
        } else {
            Phase::PendingAuth
        },
        None,
    )?;
    println!(
        "{}",
        crate::t(
            "cli-native-onboard-pending-auth",
            "Native onboarding: pending_auth. Authorize only this separate instance; interrupted auth remains resumable."
        )
    );
    #[cfg(unix)]
    tokio::select! {
        biased;
        _ = cancellation.recv() => return Err(error("cli-native-onboard-cancelled", "Native onboarding cancelled. Owned auth/config state was retained; this instance is not ready. Rerun the identical command to resume.")),
        result = Box::pin(authorize(&candidate, &request)) => result.map_err(|_| error("cli-native-onboard-auth-failed", "Native authorization failed. Owned state was retained; no readiness was claimed."))?,
    }
    #[cfg(not(unix))]
    authorize(&candidate, &request).await?;
    #[cfg(test)]
    pause_stage("authorized").await;
    let source = SourceModelFields::for_request(&request);
    let provider = format!("{}.{}", source.family, request.provider_alias);
    if !has_config {
        source.materialize(&mut candidate, &request)?;
        candidate.set_prop("memory.embedding_provider", "none")?;
        candidate.set_prop("memory.auto_save", "false")?;
        candidate.set_prop("query_classification.enabled", "false")?;
        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Existing(provider.clone()),
            risk_profile: SelectorChoice::Fresh(request.risk_preset.name().into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::None),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity { name: request.agent_alias.clone(), system_prompt: "You are a personal assistant. Respect the configured policy and use only ZeroClaw's tool loop.".into(), personality_file: None, personality_files: vec![] },
        };
        // Quickstart alone owns agents/presets/channel/memory and the commit.
        // Its staged check verifies canonical consumer inheritance for the
        // auxiliary refs before the private candidate reaches persistence.
        let staged =
            quickstart::stage_apply_checked(submission, &mut candidate, Surface::Cli, &|config| {
                // Empty classifier/summary inherit this explicit agent provider.
                let check = || -> Result<()> {
                    let mut effective = config.clone();
                    zeroclaw_config::env_overrides::apply_env_overrides(&mut effective)?;
                    anyhow::ensure!(
                        config_digest(&effective)? == config_digest(config)?,
                        "environment overrides would alter the fresh instance"
                    );
                    verify_configuration(&effective, owned, &provider)
                };
                check().map_err(|_| "native onboarding policy/billing validation failed".into())
            })
            .map_err(|_| {
                error(
                    "cli-native-onboard-config-failed",
                    "Native onboarding configuration was rejected; owned auth was retained.",
                )
            })?;
        // Stage's own working copy is immutable to callers. Auxiliary empty
        // refs inherit the one explicit provider, and verifier enforces this.
        owned.receipt.prepared_config_digest = Some(config_digest(&candidate)?);
        write_receipt(&owned.root, &owned.receipt)?;
        #[cfg(test)]
        pause_stage("prepared").await;
        // Never drop a partially executing persistence future on cancellation.
        // SIGINT stays queued and is consumed before the engine proof.
        match Box::pin(quickstart::complete_staged_apply(staged)).await.map_err(|_| error("cli-native-onboard-config-failed", "Native onboarding configuration could not be committed; owned auth was retained."))? {
            QuickstartApplyOutcome::Applied(_) => {}
            QuickstartApplyOutcome::CommittedWithSideEffectErrors { .. } => return Err(error("cli-native-onboard-partial", "Native onboarding saved configuration with incomplete side effects. Owned state was retained; this instance is not ready.")),
        }
    }
    #[cfg(test)]
    pause_stage("committed").await;
    candidate = Box::pin(load_committed(owned)).await?;
    verify_configuration(&candidate, owned, &provider)?;
    owned.transition(Phase::Configured, None)?;
    println!(
        "{}",
        crate::t(
            "cli-native-onboard-configured",
            "Native onboarding: configured. Checking one bounded real engine completion under the accepted policy."
        )
    );
    let proof = tokio::time::timeout(PROOF_TIMEOUT, async {
        #[cfg(test)]
        pause_stage("engine").await;
        let mut agent = Box::pin(zeroclaw_runtime::agent::Agent::from_config(
            &candidate,
            &request.agent_alias,
        ))
        .await?;
        let reply = Box::pin(
            agent.turn("Reply exactly NATIVE_ONBOARD_READY. Do not call tools or access files."),
        )
        .await?;
        anyhow::ensure!(
            reply.trim() == READY_REPLY,
            "native onboarding engine proof did not complete with the expected response"
        );
        Ok::<_, anyhow::Error>(())
    });
    #[cfg(unix)]
    tokio::select! {
        biased;
        _ = cancellation.recv() => return Err(error("cli-native-onboard-cancelled", "Native onboarding cancelled. Owned auth/config state was retained; this instance is not ready. Rerun the identical command to resume.")),
        result = proof => result.map_err(|_| error("cli-native-onboard-engine-failed", "Native onboarding engine validation failed or timed out. Configured state was retained; this instance is not ready."))?.map_err(|_| error("cli-native-onboard-engine-failed", "Native onboarding engine validation failed or timed out. Configured state was retained; this instance is not ready."))?,
    }
    #[cfg(not(unix))]
    proof.await??;
    // Engine tools cannot silently rewrite the candidate we just verified.
    let observed = Box::pin(load_committed(owned)).await?;
    verify_configuration(&observed, owned, &provider)?;
    owned.receipt.last_validation = Some(ValidationObservation {
        at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        model_provider: provider,
        model: request.model,
    });
    owned.transition(Phase::Ready, None)?;
    println!(
        "{}",
        crate::t(
            "cli-native-onboard-ready",
            "Native onboarding: ready. This receipt records the last verified engine completion and policy, not ongoing account, quota, or policy authority."
        )
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::Arc;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    fn request() -> Request {
        let cli = crate::Cli::try_parse_from([
            "zeroclaw",
            "native-onboard",
            "--client",
            "chatgpt-plan",
            "--provider-alias",
            "subscriber",
            "--agent-alias",
            "assistant",
            "--model",
            "model-fixture",
            "--risk-preset",
            "balanced",
            "--expected-billing",
            "subscription",
        ])
        .unwrap();
        let crate::Commands::NativeOnboard(request) = cli.command else {
            panic!("native command");
        };
        request
    }

    async fn server() -> MockServer {
        let server = MockServer::start().await;
        let identity = Arc::new(zeroclaw_providers::plan_test_transport::SyntheticIdentity::new());
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(identity.jwks()))
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/token"))
            .respond_with(move |request: &wiremock::Request| {
                let form = reqwest::Url::parse(&format!("http://127.0.0.1/?{}", String::from_utf8_lossy(&request.body))).unwrap().query_pairs().into_owned().collect::<std::collections::HashMap<_, _>>();
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["client_id"], "oaiapp_native_fixture");
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token":"synthetic-native-access", "refresh_token":"synthetic-native-refresh",
                    "id_token":identity.id_token("oaiapp_native_fixture", "synthetic-subject", &form["code"]),
                    "expires_in":3600, "token_type":"Bearer", "scope":"chatgpt.tokens.use.direct"
                }))
            }).mount(&server).await;
        // A real engine error must never become ready, on this text-only base
        // or on the later tool-enabled provider dependency.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        server
    }

    fn child(
        root: &Path,
        action: &str,
        ready: &Path,
        base: &str,
        stage: Option<&str>,
    ) -> tokio::process::Child {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "native_onboard::tests::native_onboard_process_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZEROCLAW_NATIVE_FIXTURE_ROOT", root)
            .env("ZEROCLAW_NATIVE_FIXTURE_ACTION", action)
            .env("ZEROCLAW_NATIVE_FIXTURE_READY", ready)
            .env("ZEROCLAW_NATIVE_FIXTURE_BASE", base)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .kill_on_drop(true);
        if let Some(stage) = stage {
            command.env("ZEROCLAW_NATIVE_FIXTURE_STAGE", stage);
        }
        command.spawn().unwrap()
    }

    async fn completed_response(server: &MockServer, reply: &str) {
        let reply = reply.to_owned();
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["model"], "model-fixture");
                assert!(
                    body["tools"]
                        .as_array()
                        .is_some_and(|tools| !tools.is_empty()),
                    "proof must use the normal tool-capable agent request"
                );
                ResponseTemplate::new(200).set_body_raw(
                    format!(
                        "data: {}\n\n",
                        serde_json::json!({
                            "type":"response.completed",
                            "response":{"status":"completed","output":[{
                                "type":"message","role":"assistant",
                                "content":[{"type":"output_text","text":reply}]
                            }]}
                        })
                    ),
                    "text/event-stream",
                )
            })
            .with_priority(1)
            .mount(server)
            .await;
    }

    async fn wait_ready(ready: &Path) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("synthetic child must reach the named boundary");
    }

    async fn success(child: &mut tokio::process::Child) {
        assert!(
            tokio::time::timeout(Duration::from_secs(20), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn native_onboard_process_child() {
        let Ok(root) = std::env::var("ZEROCLAW_NATIVE_FIXTURE_ROOT") else {
            return;
        };
        crate::i18n::init("en");
        let mut request = request();
        let ready: PathBuf = std::env::var_os("ZEROCLAW_NATIVE_FIXTURE_READY")
            .unwrap()
            .into();
        let action = std::env::var("ZEROCLAW_NATIVE_FIXTURE_ACTION").unwrap();
        if action == "ready-yolo" {
            request.risk_preset = RiskPreset::Yolo;
            request.accept_yolo = true;
        }
        if action == "claim" {
            validate_request(&request).unwrap();
            let _owned = OwnedRoot::acquire(Path::new(&root), &request).unwrap();
            std::fs::write(&ready, b"claimed").unwrap();
            std::future::pending::<()>().await;
        }
        let observer = Arc::new({
            let ready = ready.clone();
            let action = action.clone();
            move |url: &str| {
                let query = reqwest::Url::parse(url)
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect::<std::collections::HashMap<_, _>>();
                if action == "pending" {
                    std::fs::write(&ready, b"pending_auth").unwrap();
                } else {
                    let callback = format!(
                        "{}?state={}&code={}&client_id=oaiapp_native_fixture",
                        query["redirect_uri"], query["state"], query["nonce"]
                    );
                    ::zeroclaw_spawn::spawn!(async move {
                        reqwest::Client::new().get(callback).send().await.unwrap();
                    });
                }
            }
        });
        let base = std::env::var("ZEROCLAW_NATIVE_FIXTURE_BASE").unwrap();
        let operation = async {
            let result = Box::pin(run(Some(&root), request)).await;
            if action.starts_with("ready") {
                result.unwrap();
                let receipt = read_receipt(Path::new(&root)).unwrap();
                assert_eq!(receipt.phase, Phase::Ready);
                assert!(receipt.failure_stage.is_none());
                let observation = receipt.last_validation.unwrap();
                assert!(observation.at_unix_seconds > 0);
                assert_eq!(observation.model_provider, "openai.subscriber");
                assert_eq!(observation.model, "model-fixture");
                let owned = OwnedRoot::acquire(Path::new(&root), &receipt.request).unwrap();
                let config = Box::pin(load_committed(&owned)).await.unwrap();
                verify_configuration(&config, &owned, "openai.subscriber").unwrap();
                return;
            }
            assert!(
                result.is_err(),
                "failed/cancelled real engine proof cannot grant readiness"
            );
            let receipt = read_receipt(Path::new(&root)).unwrap();
            assert_eq!(receipt.phase, Phase::Failed);
            assert!(receipt.last_validation.is_none());
            if action == "complete" {
                assert!(
                    Path::new(&root).join("config.toml").exists(),
                    "signed auth must reach canonical config commit before engine failure: {result:?}"
                );
                assert!(receipt.prepared_config_digest.is_some());
            }
            if action == "env-rejected" {
                assert!(
                    !Path::new(&root).join("config.toml").exists(),
                    "an effective-policy override must be rejected before persistence"
                );
                assert!(receipt.prepared_config_digest.is_none());
            }
            if action == "ownership-rejected" {
                assert!(!Path::new(&root).join("config.toml").exists());
                assert!(
                    !Path::new(&root).join("auth-profiles.json").exists(),
                    "a canonical config writer must exclude bootstrap before authorization"
                );
            }
        };
        zeroclaw_providers::plan_test_transport::scope_with_observer(&base, observer, async {
            if let Ok(stage) = std::env::var("ZEROCLAW_NATIVE_FIXTURE_STAGE") {
                PAUSE_STAGE.scope((stage, ready), operation).await;
            } else {
                operation.await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn native_onboard_owner_crash_releases_exclusion_without_adopting_foreign_roots() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let ready = parent.path().join("ready");
        let mut process = child(&root, "claim", &ready, "http://127.0.0.1:1", None);
        wait_ready(&ready).await;
        assert!(
            OwnedRoot::acquire(&root, &request()).is_err(),
            "live owner must exclude another writer"
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let original = std::fs::read(root.join(RECEIPT)).unwrap();
        process.kill().await.unwrap();
        process.wait().await.unwrap();
        let mut different = request();
        different.model = "other-model".into();
        assert!(
            OwnedRoot::acquire(&root, &different).is_err(),
            "resumption must preserve exact accepted request"
        );
        assert_eq!(std::fs::read(root.join(RECEIPT)).unwrap(), original);
        let owned = OwnedRoot::acquire(&root, &request()).unwrap();
        assert_eq!(owned.receipt.phase, Phase::PendingAuth);
        assert!(owned.receipt.last_validation.is_none());
    }

    #[test]
    fn native_onboard_exclusive_publication_preserves_a_racing_empty_root() {
        let parent = tempfile::tempdir().unwrap();
        let staged = tempfile::tempdir_in(parent.path()).unwrap();
        std::fs::write(staged.path().join("owned"), b"staged").unwrap();
        let destination = parent.path().join("fresh");
        std::fs::create_dir(&destination).unwrap();
        assert!(publish_root(staged.path(), &destination).is_err());
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        assert_eq!(
            std::fs::read(staged.path().join("owned")).unwrap(),
            b"staged"
        );
    }

    #[test]
    fn native_onboard_foreign_receipt_and_replaced_lease_fail_closed() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let mut owned = OwnedRoot::acquire(&root, &request()).unwrap();
        std::fs::remove_file(root.join(LEASE)).unwrap();
        private_options()
            .create_new(true)
            .write(true)
            .open(root.join(LEASE))
            .unwrap();
        assert!(
            owned.transition(Phase::Configured, None).is_err(),
            "named lease must still match the held inode"
        );
        drop(owned);
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::copy(root.join(RECEIPT), outside.path()).unwrap();
        std::fs::remove_file(root.join(RECEIPT)).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join(RECEIPT)).unwrap();
        assert!(
            OwnedRoot::acquire(&root, &request()).is_err(),
            "receipt symlink must not establish ownership"
        );
    }

    #[tokio::test]
    async fn native_onboard_signed_auth_precommit_and_partial_commit_crashes_resume_without_rewriting()
     {
        let server = server().await;
        for stage in ["authorized", "prepared", "committed"] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("fresh");
            let ready = parent.path().join("ready");
            let mut process = child(&root, "complete", &ready, &server.uri(), Some(stage));
            wait_ready(&ready).await;
            let auth_bytes = std::fs::read(root.join("auth-profiles.json")).unwrap();
            assert!(!String::from_utf8_lossy(&auth_bytes).contains("synthetic-native-access"));
            let profile = AuthService::new(&root, true).load_profiles().await.unwrap();
            assert!(
                profile.profiles["chatgpt-plan:subscriber"]
                    .plan_registration
                    .is_some()
            );
            assert_eq!(
                root.join("config.toml").exists(),
                stage == "committed",
                "auth must precede config persistence"
            );
            let config_before = std::fs::read(root.join("config.toml")).ok();
            assert_ne!(read_receipt(&root).unwrap().phase, Phase::Ready);
            process.kill().await.unwrap();
            process.wait().await.unwrap();
            success(&mut child(&root, "complete", &ready, &server.uri(), None)).await;
            assert_eq!(
                std::fs::read(root.join("auth-profiles.json")).unwrap(),
                auth_bytes,
                "resumption must retain the signed canonical grant"
            );
            if let Some(original) = config_before {
                assert_eq!(
                    std::fs::read(root.join("config.toml")).unwrap(),
                    original,
                    "partial-commit resumption must not rewrite canonical config"
                );
            }
            let receipt = read_receipt(&root).unwrap();
            assert_eq!(receipt.phase, Phase::Failed);
            assert!(receipt.last_validation.is_none());
            let owned = OwnedRoot::acquire(&root, &request()).unwrap();
            let config = Box::pin(load_committed(&owned)).await.unwrap();
            verify_configuration(&config, &owned, "openai.subscriber").unwrap();
            assert_eq!(config.memory.embedding_provider, "none");
            assert!(config.effective_summary_provider("assistant").is_none());
            if stage == "committed" {
                for (field, value) in [
                    ("memory.embedding_provider", "openai"),
                    ("agents.assistant.classifier_provider", "openai.metered"),
                    ("agents.assistant.summary_provider", "openai.metered"),
                    ("risk_profiles.balanced.allowed_tools", "file_read"),
                    ("runtime_profiles.balanced.max_tool_iterations", "2"),
                    ("providers.models.openai.subscriber.model", "other-model"),
                    (
                        "providers.models.openai.subscriber.chatgpt_plan_auth.registration",
                        "chatgpt-plan:other",
                    ),
                ] {
                    let mut changed = config.clone();
                    changed
                        .create_map_key("providers.models.openai", "metered")
                        .unwrap();
                    changed
                        .set_prop("providers.models.openai.metered.model", "api-model")
                        .unwrap();
                    changed
                        .set_prop(
                            "providers.models.openai.metered.api_key",
                            "synthetic-api-key",
                        )
                        .unwrap();
                    changed.set_prop(field, value).unwrap();
                    assert!(
                        verify_configuration(&changed, &owned, "openai.subscriber").is_err(),
                        "must reject changed effective consumer/preset: {field}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn native_onboard_effective_environment_billing_override_is_rejected_before_commit() {
        let server = server().await;
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let ready = parent.path().join("ready");
        let mut process = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "native_onboard::tests::native_onboard_process_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZEROCLAW_NATIVE_FIXTURE_ROOT", &root)
            .env("ZEROCLAW_NATIVE_FIXTURE_ACTION", "env-rejected")
            .env("ZEROCLAW_NATIVE_FIXTURE_READY", &ready)
            .env("ZEROCLAW_NATIVE_FIXTURE_BASE", server.uri())
            .env("ZEROCLAW_memory__embedding_provider", "openai")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        success(&mut process).await;
        assert!(
            AuthService::new(&root, true)
                .load_profiles()
                .await
                .unwrap()
                .profiles
                .contains_key("chatgpt-plan:subscriber")
        );
        assert!(!root.join("config.toml").exists());
    }

    #[tokio::test]
    async fn native_onboard_environment_override_is_rejected_even_without_an_alternate_billing_path()
     {
        let server = server().await;
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let ready = parent.path().join("ready");
        let mut process = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "native_onboard::tests::native_onboard_process_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZEROCLAW_NATIVE_FIXTURE_ROOT", &root)
            .env("ZEROCLAW_NATIVE_FIXTURE_ACTION", "env-rejected")
            .env("ZEROCLAW_NATIVE_FIXTURE_READY", &ready)
            .env("ZEROCLAW_NATIVE_FIXTURE_BASE", server.uri())
            .env("ZEROCLAW_memory__auto_save", "true")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        success(&mut process).await;
        assert!(!root.join("config.toml").exists());
    }

    #[tokio::test]
    async fn native_onboard_canonical_config_writer_excludes_auth_and_commit() {
        let server = server().await;
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let ready = parent.path().join("ready");
        drop(OwnedRoot::acquire(&root, &request()).unwrap());
        let _writer = zeroclaw_runtime::live_config_authority::ConfigOwnershipGuard::acquire(
            &root.join("data"),
        )
        .unwrap();
        success(&mut child(
            &root,
            "ownership-rejected",
            &ready,
            &server.uri(),
            None,
        ))
        .await;
        assert!(!root.join("auth-profiles.json").exists());
        assert!(!root.join("config.toml").exists());
    }

    #[tokio::test]
    async fn native_onboard_ready_requires_completed_normal_agent_turn_under_each_accepted_preset()
    {
        let server = server().await;
        completed_response(&server, READY_REPLY).await;
        for action in ["ready", "ready-yolo"] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("fresh");
            success(&mut child(
                &root,
                action,
                &parent.path().join("ready"),
                &server.uri(),
                None,
            ))
            .await;
            assert_eq!(read_receipt(&root).unwrap().phase, Phase::Ready);
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/responses")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn native_onboard_completed_normal_agent_turn_with_wrong_reply_never_claims_ready() {
        let server = server().await;
        completed_response(&server, "DIFFERENT_RESPONSE").await;
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        success(&mut child(
            &root,
            "complete",
            &parent.path().join("ready"),
            &server.uri(),
            None,
        ))
        .await;
        let receipt = read_receipt(&root).unwrap();
        assert_eq!(receipt.phase, Phase::Failed);
        assert_eq!(
            receipt.failure_stage.as_deref(),
            Some("cli-native-onboard-engine-failed")
        );
        assert!(receipt.last_validation.is_none());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.url.path() == "/responses")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn native_onboard_committed_config_requires_matching_prepared_intent() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let mut owned = OwnedRoot::acquire(&root, &request()).unwrap();
        let raw = "locale = \"en\"\n";
        let config_path = root.join("config.toml");
        private_options()
            .create_new(true)
            .write(true)
            .open(&config_path)
            .unwrap()
            .write_all(raw.as_bytes())
            .unwrap();
        assert!(
            Box::pin(load_committed(&owned)).await.is_err(),
            "an unrelated file cannot become a resumable commit"
        );
        let prepared = Box::pin(Config::prepare_from_migrated_toml(
            raw,
            &config_path,
            &root.join("data"),
        ))
        .await
        .unwrap();
        owned.receipt.prepared_config_digest = Some(config_digest(&prepared).unwrap());
        assert!(Box::pin(load_committed(&owned)).await.is_ok());
        std::fs::write(config_path, "locale = \"ja\"\n").unwrap();
        assert!(
            Box::pin(load_committed(&owned)).await.is_err(),
            "changed committed state must not match prior intent"
        );
    }

    #[test]
    fn native_onboard_private_state_rejects_nonregular_and_readable_files() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            check_private(&File::open(parent.path()).unwrap()).is_err(),
            "a directory is not a state leaf"
        );
        let file = tempfile::NamedTempFile::new_in(parent.path()).unwrap();
        assert!(check_private(file.as_file()).is_ok());
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            check_private(file.as_file()).is_err(),
            "state readable by other users must not establish ownership"
        );
    }

    #[tokio::test]
    async fn native_onboard_sigint_retains_owned_auth_and_config_never_ready() {
        let server = server().await;
        for (action, stage) in [("pending", None), ("complete", Some("engine"))] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("fresh");
            let ready = parent.path().join("ready");
            let mut process = child(&root, action, &ready, &server.uri(), stage);
            wait_ready(&ready).await;
            let auth_before = std::fs::read(root.join("auth-profiles.json")).unwrap();
            let config_before = std::fs::read(root.join("config.toml")).ok();
            // SAFETY: Child owns a live process ID and libc::kill receives an integer signal.
            assert_eq!(
                unsafe { libc::kill(process.id().unwrap() as libc::pid_t, libc::SIGINT) },
                0
            );
            success(&mut process).await;
            assert_eq!(read_receipt(&root).unwrap().phase, Phase::Failed);
            assert!(read_receipt(&root).unwrap().last_validation.is_none());
            assert_eq!(
                std::fs::read(root.join("auth-profiles.json")).unwrap(),
                auth_before
            );
            if let Some(original) = config_before {
                assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), original);
            }
            assert!(
                OwnedRoot::acquire(&root, &request()).is_ok(),
                "cancelled instance must remain exclusively resumable"
            );
        }
    }

    #[test]
    fn native_onboard_ready_requires_a_completed_validation_observation() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        let mut owned = OwnedRoot::acquire(&root, &request()).unwrap();
        assert!(
            owned.transition(Phase::Ready, None).is_err(),
            "a status-only transition must not claim readiness"
        );
        assert_ne!(read_receipt(&root).unwrap().phase, Phase::Ready);
    }

    #[test]
    fn native_onboard_copied_receipt_cannot_adopt_another_directory() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let first = parent.path().join("first");
        drop(OwnedRoot::acquire(&first, &request()).unwrap());
        let second = parent.path().join("second");
        std::fs::create_dir(&second).unwrap();
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(first.join(RECEIPT), second.join(RECEIPT)).unwrap();
        private_options()
            .create_new(true)
            .write(true)
            .open(second.join(LEASE))
            .unwrap();
        std::fs::write(second.join("keep"), b"foreign").unwrap();
        assert!(
            OwnedRoot::acquire(&second, &request()).is_err(),
            "ownership is bound to the original created directory inode"
        );
        assert_eq!(std::fs::read(second.join("keep")).unwrap(), b"foreign");
    }

    #[test]
    fn native_onboard_nonprivate_or_special_receipt_is_not_an_ownership_claim() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        drop(OwnedRoot::acquire(&root, &request()).unwrap());
        std::fs::set_permissions(root.join(LEASE), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(OwnedRoot::acquire(&root, &request()).is_err());
        std::fs::set_permissions(root.join(LEASE), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(root.join(RECEIPT)).unwrap();
        let fifo = std::ffi::CString::new(root.join(RECEIPT).as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is terminated and lives through the syscall.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(
            OwnedRoot::acquire(&root, &request()).is_err(),
            "special-file state must be rejected without blocking"
        );
    }

    #[tokio::test]
    async fn native_onboard_foreign_config_without_prepared_intent_is_never_overwritten() {
        crate::i18n::init("en");
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fresh");
        drop(OwnedRoot::acquire(&root, &request()).unwrap());
        let original = b"locale = \"en\"\n";
        private_options()
            .create_new(true)
            .write(true)
            .open(root.join("config.toml"))
            .unwrap()
            .write_all(original)
            .unwrap();
        assert!(Box::pin(run(root.to_str(), request())).await.is_err());
        assert_eq!(std::fs::read(root.join("config.toml")).unwrap(), original);
        assert!(
            !root.join("auth-profiles.json").exists(),
            "foreign config must be refused before auth starts"
        );
        assert_eq!(read_receipt(&root).unwrap().phase, Phase::Failed);
    }

    #[test]
    fn native_onboard_unavailable_native_family_refuses_before_root_creation() {
        let mut request = request();
        request.client = Client::ClaudeCode;
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn native_onboard_source_fields_use_canonical_config_setter_for_explicit_registration() {
        let mut config = Config::default();
        let provider = SourceModelFields::for_request(&request())
            .materialize(&mut config, &request())
            .unwrap();
        assert_eq!(provider, "openai.subscriber");
        assert_eq!(
            config
                .get_prop("providers.models.openai.subscriber.chatgpt_plan_auth.registration")
                .unwrap(),
            "chatgpt-plan:subscriber"
        );
        assert!(
            config
                .providers
                .models
                .find("openai", "subscriber")
                .unwrap()
                .api_key
                .is_none()
        );
        assert!(
            !config
                .providers
                .models
                .find("openai", "subscriber")
                .unwrap()
                .requires_openai_auth
        );
    }
}
