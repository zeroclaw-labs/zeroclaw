//! Config types that were originally defined in their home modules (agent, channels, tools, trust)
//! but are needed by the config schema. Moved here to break circular dependencies.

use crate::traits::{ChannelConfig, HasPropKind, PropKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use zeroclaw_macros::Configurable;

// ── Agent config types ──────────────────────────────────────────

/// How deeply the model should reason for a given message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    Max,
}

impl HasPropKind for ThinkingLevel {
    const PROP_KIND: PropKind = PropKind::Enum;
}

impl ThinkingLevel {
    pub fn from_str_insensitive(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "minimal" | "min" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" | "med" | "default" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn default_budget_tokens(&self) -> Option<u32> {
        match self {
            Self::Off | Self::Minimal | Self::Low | Self::Medium => None,
            Self::High => Some(10_000),
            Self::Max => Some(50_000),
        }
    }
}

pub use zeroclaw_api::model_provider::{
    MAX_BUDGET_TOKENS, MIN_BUDGET_TOKENS, NativeThinkingParams,
};

/// Configuration for thinking/reasoning level control.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.thinking"]
pub struct ThinkingConfig {
    #[serde(default)]
    pub default_level: ThinkingLevel,
    /// Enables provider-native thinking parameters when the selected level has a budget.
    #[serde(default)]
    pub native_thinking: bool,
    #[serde(default)]
    pub budget_tokens: HashMap<String, u32>,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            default_level: ThinkingLevel::Medium,
            native_thinking: false,
            budget_tokens: HashMap::new(),
        }
    }
}

impl ThinkingConfig {
    pub fn budget_tokens_for(&self, level: ThinkingLevel) -> Option<u32> {
        // Guard: only levels that have a built-in budget can use native thinking.
        let default = level.default_budget_tokens()?;
        Some(
            self.budget_tokens
                .get(level.as_str())
                .copied()
                .unwrap_or(default),
        )
    }

    pub fn warn_unknown_budget_keys(&self) {
        use ThinkingLevel::{High, Low, Max, Medium, Minimal, Off};
        const ALL_LEVELS: &[ThinkingLevel] = &[Off, Minimal, Low, Medium, High, Max];
        for key in self.budget_tokens.keys() {
            if !ALL_LEVELS.iter().any(|l| l.as_str() == key) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"key": key})),
                    "Unknown thinking level in budget_tokens config; \
                     valid levels are: off, minimal, low, medium, high, max"
                );
            }
        }
    }
}

fn default_max_tokens() -> usize {
    8192
}
fn default_keep_recent() -> usize {
    4
}
fn default_collapse() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.history_pruning"]
pub struct HistoryPrunerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_keep_recent")]
    pub keep_recent: usize,
    #[serde(default = "default_collapse")]
    pub collapse_tool_results: bool,
}

impl Default for HistoryPrunerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_tokens: 8192,
            keep_recent: 4,
            collapse_tool_results: true,
        }
    }
}

fn default_cost_optimized_hint() -> String {
    "cost-optimized".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.auto_classify"]
pub struct AutoClassifyConfig {
    #[serde(default)]
    pub simple_hint: Option<String>,
    #[serde(default)]
    pub standard_hint: Option<String>,
    #[serde(default)]
    pub complex_hint: Option<String>,
    #[serde(default = "default_cost_optimized_hint")]
    pub cost_optimized_hint: String,
}

impl Default for AutoClassifyConfig {
    fn default() -> Self {
        Self {
            simple_hint: None,
            standard_hint: None,
            complex_hint: None,
            cost_optimized_hint: default_cost_optimized_hint(),
        }
    }
}

fn default_min_quality_score() -> f64 {
    0.5
}
fn default_eval_max_retries() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.eval"]
pub struct EvalConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_min_quality_score")]
    pub min_quality_score: f64,
    #[serde(default = "default_eval_max_retries")]
    pub max_retries: u32,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_quality_score: default_min_quality_score(),
            max_retries: default_eval_max_retries(),
        }
    }
}

fn default_eval_suite_dir() -> String {
    "evals".to_string()
}
fn default_eval_mode() -> String {
    "replay".to_string()
}

/// Configuration for the agent evaluation harness (`[eval]`), surfaced via the
/// `zeroclaw eval` command. Distinct from `[agent.eval]`, which is the in-loop
/// response-quality scorer.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "eval"]
pub struct EvalHarnessConfig {
    /// Default directory of `*.json` trace fixtures used when `--suite` is omitted.
    #[serde(default = "default_eval_suite_dir")]
    pub suite_dir: String,
    /// Default execution mode (`replay` or `live`) used when `--mode` is omitted.
    #[serde(default = "default_eval_mode")]
    pub mode: String,
}

impl Default for EvalHarnessConfig {
    fn default() -> Self {
        Self {
            suite_dir: default_eval_suite_dir(),
            mode: default_eval_mode(),
        }
    }
}

fn default_cc_enabled() -> bool {
    // The runtime compressor was intentionally removed; no production code path
    // consumes `ContextCompressionConfig` today. Default to inactive so
    // generated and inspected configs do not claim compression is enabled when
    // it has no runtime effect. A future compression architecture would be a
    // separate decision that could flip this back on.
    false
}
fn default_threshold_ratio() -> f64 {
    0.50
}
fn default_protect_first_n() -> usize {
    3
}
fn default_protect_last_n() -> usize {
    4
}
fn default_cc_max_passes() -> u32 {
    3
}
fn default_summary_max_chars() -> usize {
    4000
}
fn default_source_max_chars() -> usize {
    50_000
}
fn default_cc_timeout_secs() -> u64 {
    60
}
fn default_identifier_policy() -> String {
    "strict".to_string()
}
fn default_tool_result_retrim_chars() -> usize {
    2_000
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.context_compression"]
pub struct ContextCompressionConfig {
    /// INERT: retained for schema compatibility after the runtime compressor
    /// was removed. No runtime path consumes this today, so enabling it has no
    /// effect; `config validate` surfaces a warning when it is set to `true`.
    #[serde(default = "default_cc_enabled")]
    pub enabled: bool,
    #[serde(default = "default_threshold_ratio")]
    pub threshold_ratio: f64,
    #[serde(default = "default_protect_first_n")]
    pub protect_first_n: usize,
    #[serde(default = "default_protect_last_n")]
    pub protect_last_n: usize,
    #[serde(default = "default_cc_max_passes")]
    pub max_passes: u32,
    #[serde(default = "default_summary_max_chars")]
    pub summary_max_chars: usize,
    #[serde(default = "default_source_max_chars")]
    pub source_max_chars: usize,
    #[serde(default = "default_cc_timeout_secs")]
    pub timeout_secs: u64,
    /// Summarizer provider as a `<type>.<alias>` reference into `providers.models`.
    #[serde(default)]
    pub summary_provider: crate::providers::ModelProviderRef,
    /// DEPRECATED bare model id retained as a compatibility fallback.
    #[serde(default)]
    pub summary_model: Option<String>,
    #[serde(default = "default_identifier_policy")]
    pub identifier_policy: String,
    #[serde(default = "default_tool_result_retrim_chars")]
    pub tool_result_retrim_chars: usize,
    #[serde(default)]
    pub tool_result_trim_exempt: Vec<String>,
}

impl Default for ContextCompressionConfig {
    fn default() -> Self {
        Self {
            enabled: default_cc_enabled(),
            threshold_ratio: default_threshold_ratio(),
            protect_first_n: default_protect_first_n(),
            protect_last_n: default_protect_last_n(),
            max_passes: default_cc_max_passes(),
            summary_max_chars: default_summary_max_chars(),
            source_max_chars: default_source_max_chars(),
            timeout_secs: default_cc_timeout_secs(),
            summary_provider: crate::providers::ModelProviderRef::default(),
            summary_model: None,
            identifier_policy: default_identifier_policy(),
            tool_result_retrim_chars: default_tool_result_retrim_chars(),
            tool_result_trim_exempt: Vec::new(),
        }
    }
}

fn default_precheck_enabled() -> bool {
    true
}
fn default_precheck_timeout_secs() -> u64 {
    5
}

/// Per-channel reply-intent precheck configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.precheck"]
pub struct ChannelPrecheckConfig {
    /// When false, the precheck is skipped entirely for this agent and every
    /// accepted channel message triggers the full agent loop. Default: `true`.
    #[serde(default = "default_precheck_enabled")]
    pub enabled: bool,
    /// Hard ceiling (seconds) on the precheck LLM call. On timeout the
    /// precheck fails open to REPLY. Default: `5`.
    #[serde(default = "default_precheck_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for ChannelPrecheckConfig {
    fn default() -> Self {
        Self {
            enabled: default_precheck_enabled(),
            timeout_secs: default_precheck_timeout_secs(),
        }
    }
}

// ── Tools config types ──────────────────────────────────────────

fn default_browser_cli() -> String {
    "claude".into()
}
fn default_browser_task_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "browser_delegate"]
pub struct BrowserDelegateConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_browser_cli")]
    pub cli_binary: String,
    #[serde(default)]
    pub chrome_profile_dir: String,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default = "default_browser_task_timeout")]
    pub task_timeout_secs: u64,
}

impl Default for BrowserDelegateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cli_binary: default_browser_cli(),
            chrome_profile_dir: String::new(),
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            task_timeout_secs: default_browser_task_timeout(),
        }
    }
}

// ── Trust config types ──────────────────────────────────────────

fn default_initial_score() -> f64 {
    0.8
}
fn default_decay_half_life() -> f64 {
    30.0
}
fn default_regression_threshold() -> f64 {
    0.5
}
fn default_correction_penalty() -> f64 {
    0.05
}
fn default_success_boost() -> f64 {
    0.01
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "trust"]
pub struct TrustConfig {
    #[serde(default = "default_initial_score")]
    pub initial_score: f64,
    #[serde(default = "default_decay_half_life")]
    pub decay_half_life_days: f64,
    #[serde(default = "default_regression_threshold")]
    pub regression_threshold: f64,
    #[serde(default = "default_correction_penalty")]
    pub correction_penalty: f64,
    #[serde(default = "default_success_boost")]
    pub success_boost: f64,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            initial_score: default_initial_score(),
            decay_half_life_days: default_decay_half_life(),
            regression_threshold: default_regression_threshold(),
            correction_penalty: default_correction_penalty(),
            success_boost: default_success_boost(),
        }
    }
}

// ── Channel config types ────────────────────────────────────────

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    465
}
fn default_imap_folder() -> String {
    "INBOX".into()
}
fn default_idle_timeout() -> u64 {
    1740
}
fn default_poll_interval_secs() -> u64 {
    60
}
fn default_true() -> bool {
    true
}
fn default_subject() -> String {
    "Re: Message".into()
}
fn default_max_attachment_bytes() -> usize {
    25 * 1024 * 1024
}

/// OAuth2 settings for IMAP email authentication (XOAUTH2 / RFC 8628).
/// Populate these to use OAuth2 bearer tokens instead of plain-password LOGIN.
/// All major providers (Microsoft, Google) support this flow via device code.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct EmailOAuth2Config {
    /// OAuth2 application client ID (public client; no secret required for device flow).
    pub client_id: String,
    /// Token endpoint for refresh and device-code polling
    /// (e.g. `https://login.microsoftonline.com/consumers/oauth2/v2.0/token`).
    pub token_url: String,
    /// Device-code initiation endpoint
    /// (e.g. `https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode`).
    pub device_code_url: String,
    /// OAuth2 scopes to request (must include `offline_access` for refresh tokens).
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.email"]
pub struct EmailConfig {
    /// Whether this channel is active. The runtime only loads channels whose
    /// `enabled = true`. Default: `false` so an operator who pastes a partial
    /// `[channels.<type>.<alias>]` block doesn't accidentally bring a channel
    /// live before the rest of its config is filled in.
    #[serde(default)]
    pub enabled: bool,
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default = "default_imap_folder")]
    pub imap_folder: String,
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
    #[serde(default)]
    pub smtp_username: Option<String>,
    #[secret]
    #[serde(default)]
    pub smtp_password: Option<String>,
    pub username: String,
    #[secret]
    pub password: String,
    pub from_address: String,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// Polling interval used when the IMAP server does not advertise the IDLE
    /// capability (RFC 2177). Ignored when IDLE is available.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_subject")]
    pub default_subject: String,
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: usize,

    /// Tools excluded from this channel's tool spec. When set, these tools
    /// are not exposed to the model when responding via this channel.
    #[serde(default)]
    pub excluded_tools: Vec<String>,
    /// When `true` (default), outbound emails are rendered as HTML via Markdown conversion.
    /// Set to `false` to send plain-text emails instead.
    #[serde(default = "default_true")]
    pub html_body: bool,
    /// OAuth2 settings for IMAP authentication. When present, IMAP uses
    /// XOAUTH2 instead of plain LOGIN. Required for providers like
    /// Outlook/Hotmail that have deprecated password auth.
    #[serde(default)]
    pub oauth2: Option<EmailOAuth2Config>,
    /// When true, the daemon observes new mail but never modifies any IMAP flag.
    #[serde(default)]
    pub observer_mode: bool,
}

impl ChannelConfig for EmailConfig {
    fn name() -> &'static str {
        "Email"
    }
    fn desc() -> &'static str {
        "Email over IMAP/SMTP"
    }
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            imap_host: String::new(),
            imap_port: default_imap_port(),
            imap_folder: default_imap_folder(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_tls: true,
            smtp_username: None,
            smtp_password: None,
            username: String::new(),
            password: String::new(),
            from_address: String::new(),
            idle_timeout_secs: default_idle_timeout(),
            poll_interval_secs: default_poll_interval_secs(),
            default_subject: default_subject(),
            max_attachment_bytes: default_max_attachment_bytes(),
            excluded_tools: Vec::new(),
            html_body: true,
            oauth2: None,
            observer_mode: false,
        }
    }
}

fn default_label_filter() -> Vec<String> {
    vec!["INBOX".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.gmail"]
pub struct GmailPushConfig {
    /// Whether this channel is active. The runtime only loads channels whose
    /// `enabled = true`. Default: `false` so an operator who pastes a partial
    /// `[channels.<type>.<alias>]` block doesn't accidentally bring a channel
    /// live before the rest of its config is filled in.
    #[serde(default)]
    pub enabled: bool,
    pub topic: String,
    #[serde(default = "default_label_filter")]
    pub label_filter: Vec<String>,
    #[serde(default)]
    #[secret]
    pub oauth_token: String,
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default)]
    pub webhook_secret: String,

    /// Tools excluded from this channel's tool spec. When set, these tools
    /// are not exposed to the model when responding via this channel.
    #[serde(default)]
    pub excluded_tools: Vec<String>,
}

impl ChannelConfig for GmailPushConfig {
    fn name() -> &'static str {
        "Gmail Push"
    }
    fn desc() -> &'static str {
        "Gmail Pub/Sub push notifications"
    }
}

impl Default for GmailPushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            topic: String::new(),
            label_filter: default_label_filter(),
            oauth_token: String::new(),
            webhook_url: String::new(),
            webhook_secret: String::new(),
            excluded_tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.clawdtalk"]
pub struct ClawdTalkConfig {
    /// Whether this channel is active. The runtime only loads channels whose
    /// `enabled = true`. Default: `false` so an operator who pastes a partial
    /// `[channels.<type>.<alias>]` block doesn't accidentally bring a channel
    /// live before the rest of its config is filled in.
    #[serde(default)]
    pub enabled: bool,
    #[secret]
    pub api_key: String,
    pub connection_id: String,
    pub from_number: String,
    #[serde(default)]
    pub allowed_destinations: Vec<String>,
    #[serde(default)]
    #[secret]
    pub webhook_secret: Option<String>,

    /// Tools excluded from this channel's tool spec. When set, these tools
    /// are not exposed to the model when responding via this channel.
    #[serde(default)]
    pub excluded_tools: Vec<String>,
}

impl ChannelConfig for ClawdTalkConfig {
    fn name() -> &'static str {
        "ClawdTalk"
    }
    fn desc() -> &'static str {
        "ClawdTalk Channel"
    }
}

/// Which telephony model_provider to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum VoiceProvider {
    #[default]
    Twilio,
    Telnyx,
    Plivo,
}

impl HasPropKind for VoiceProvider {
    const PROP_KIND: PropKind = PropKind::Enum;
}

impl fmt::Display for VoiceProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Twilio => write!(f, "twilio"),
            Self::Telnyx => write!(f, "telnyx"),
            Self::Plivo => write!(f, "plivo"),
        }
    }
}

fn default_webhook_port() -> u16 {
    8090
}
fn default_max_call_duration() -> u64 {
    3600
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.voice_call"]
pub struct VoiceCallConfig {
    /// Whether this channel is active. The runtime only loads channels whose
    /// `enabled = true`. Default: `false` so an operator who pastes a partial
    /// `[channels.<type>.<alias>]` block doesn't accidentally bring a channel
    /// live before the rest of its config is filled in.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub model_provider: VoiceProvider,
    pub account_id: String,
    pub auth_token: String,
    pub from_number: String,
    #[serde(default = "default_webhook_port")]
    pub webhook_port: u16,
    #[serde(default = "default_true")]
    pub require_outbound_approval: bool,
    #[serde(default = "default_true")]
    pub transcription_logging: bool,
    #[serde(default)]
    pub tts_voice: Option<String>,
    #[serde(default = "default_max_call_duration")]
    pub max_call_duration_secs: u64,
    #[serde(default)]
    pub webhook_base_url: Option<String>,

    /// Tools excluded from this channel's tool spec. When set, these tools
    /// are not exposed to the model when responding via this channel.
    #[serde(default)]
    pub excluded_tools: Vec<String>,
}

impl crate::traits::ChannelConfig for VoiceCallConfig {
    fn name() -> &'static str {
        "Voice Call"
    }
    fn desc() -> &'static str {
        "outbound voice call channel"
    }
}

impl Default for VoiceCallConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_provider: VoiceProvider::default(),
            account_id: String::new(),
            auth_token: String::new(),
            from_number: String::new(),
            webhook_port: default_webhook_port(),
            require_outbound_approval: default_true(),
            transcription_logging: default_true(),
            tts_voice: None,
            max_call_duration_secs: default_max_call_duration(),
            webhook_base_url: None,
            excluded_tools: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_level_from_str_canonical_aliases() {
        let cases = [
            ("off", ThinkingLevel::Off),
            ("none", ThinkingLevel::Off),
            ("minimal", ThinkingLevel::Minimal),
            ("min", ThinkingLevel::Minimal),
            ("low", ThinkingLevel::Low),
            ("medium", ThinkingLevel::Medium),
            ("med", ThinkingLevel::Medium),
            ("default", ThinkingLevel::Medium),
            ("high", ThinkingLevel::High),
            ("max", ThinkingLevel::Max),
            ("maximum", ThinkingLevel::Max),
        ];
        for (s, expected) in cases {
            assert_eq!(
                ThinkingLevel::from_str_insensitive(s),
                Some(expected),
                "input={s}"
            );
        }
    }

    #[test]
    fn thinking_level_from_str_case_insensitive() {
        assert_eq!(
            ThinkingLevel::from_str_insensitive("OFF"),
            Some(ThinkingLevel::Off)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("HIGH"),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("MAX"),
            Some(ThinkingLevel::Max)
        );
        assert_eq!(
            ThinkingLevel::from_str_insensitive("Medium"),
            Some(ThinkingLevel::Medium)
        );
    }

    #[test]
    fn thinking_level_from_str_unknown_returns_none() {
        assert_eq!(ThinkingLevel::from_str_insensitive("unknown"), None);
        assert_eq!(ThinkingLevel::from_str_insensitive(""), None);
        assert_eq!(ThinkingLevel::from_str_insensitive("ultra"), None);
        assert_eq!(ThinkingLevel::from_str_insensitive("123"), None);
    }

    #[test]
    fn thinking_level_as_str_roundtrips() {
        for level in [
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Max,
        ] {
            let s = level.as_str();
            assert_eq!(
                ThinkingLevel::from_str_insensitive(s),
                Some(level),
                "roundtrip failed for {s}"
            );
        }
    }

    #[test]
    fn thinking_level_budget_tokens_only_high_and_max() {
        assert_eq!(ThinkingLevel::Off.default_budget_tokens(), None);
        assert_eq!(ThinkingLevel::Minimal.default_budget_tokens(), None);
        assert_eq!(ThinkingLevel::Low.default_budget_tokens(), None);
        assert_eq!(ThinkingLevel::Medium.default_budget_tokens(), None);
        assert_eq!(ThinkingLevel::High.default_budget_tokens(), Some(10_000));
        assert_eq!(ThinkingLevel::Max.default_budget_tokens(), Some(50_000));
    }

    #[test]
    fn thinking_level_default_is_medium() {
        assert_eq!(ThinkingLevel::default(), ThinkingLevel::Medium);
    }
}
