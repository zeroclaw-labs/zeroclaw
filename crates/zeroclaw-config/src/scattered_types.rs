//! Config types that were originally defined in their home modules (agent, channels, tools, trust)
//! but are needed by the config schema. Moved here to break circular dependencies.

use crate::traits::{ChannelConfig, HasPropKind, PropKind};
#[cfg(feature = "schema-export")]
use serde::{Deserialize, Serialize};
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
}

/// Configuration for thinking/reasoning level control.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.thinking"]
pub struct ThinkingConfig {
    #[serde(default)]
    pub default_level: ThinkingLevel,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            default_level: ThinkingLevel::Medium,
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

/// Conversation history pruning to keep prompt size bounded (`[agent.history_pruning]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.history-pruning"]
pub struct HistoryPrunerConfig {
    /// Enable history pruning. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Approximate token cap for the pruned history (rough estimate, not exact). Default: `8192`.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// Number of most-recent turns to always keep verbatim. Default: `4`.
    #[serde(default = "default_keep_recent")]
    pub keep_recent: usize,
    /// When true, replace older tool-result blocks with summaries to save tokens. Default: `true`.
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

/// Auto-classify incoming requests by complexity and route each tier to its provider hint (`[agent.auto_classify]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.auto-classify"]
pub struct AutoClassifyConfig {
    /// Provider hint for simple requests. When unset, the default model is used.
    #[serde(default)]
    pub simple_hint: Option<String>,
    /// Provider hint for standard requests. When unset, the default model is used.
    #[serde(default)]
    pub standard_hint: Option<String>,
    /// Provider hint for complex requests. When unset, the default model is used.
    #[serde(default)]
    pub complex_hint: Option<String>,
    /// Provider hint for cost-optimized routing. Default: `"cost-optimized"`.
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

/// Inline reply evaluation: score the agent's draft and retry when below threshold (`[agent.eval]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.eval"]
pub struct EvalConfig {
    /// Enable reply evaluation. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum acceptable quality score (`0.0`–`1.0`). Default: `0.5`.
    #[serde(default = "default_min_quality_score")]
    pub min_quality_score: f64,
    /// Maximum retry attempts when the score is below threshold. Default: `1`.
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

fn default_cc_enabled() -> bool {
    true
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

/// Summarize older turns and tool results to keep context inside model limits (`[agent.context_compression]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent.context-compression"]
pub struct ContextCompressionConfig {
    /// Enable context compression. Default: `true`.
    #[serde(default = "default_cc_enabled")]
    pub enabled: bool,
    /// Compress when the current context size exceeds this fraction of the model's limit. Default: `0.5`.
    #[serde(default = "default_threshold_ratio")]
    pub threshold_ratio: f64,
    /// Number of opening turns to never compress. Default: `3`.
    #[serde(default = "default_protect_first_n")]
    pub protect_first_n: usize,
    /// Number of trailing turns to never compress. Default: `4`.
    #[serde(default = "default_protect_last_n")]
    pub protect_last_n: usize,
    /// Maximum compression passes per request. Default: `3`.
    #[serde(default = "default_cc_max_passes")]
    pub max_passes: u32,
    /// Character cap for each generated summary. Default: `4000`.
    #[serde(default = "default_summary_max_chars")]
    pub summary_max_chars: usize,
    /// Maximum characters of source to feed into the compression model per pass. Default: `50000`.
    #[serde(default = "default_source_max_chars")]
    pub source_max_chars: usize,
    /// Per-pass timeout for the compression call (seconds). Default: `60`.
    #[serde(default = "default_cc_timeout_secs")]
    pub timeout_secs: u64,
    /// Override model used for summarization. When unset, the default model is used.
    #[serde(default)]
    pub summary_model: Option<String>,
    /// How to handle code/tool identifiers in summaries (`"strict"` or `"loose"`). Default: `"strict"`.
    #[serde(default = "default_identifier_policy")]
    pub identifier_policy: String,
    /// Re-trim tool results to this many characters when retrying compression. Default: `2000`.
    #[serde(default = "default_tool_result_retrim_chars")]
    pub tool_result_retrim_chars: usize,
    /// Tool names whose results are never trimmed during compression.
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
            summary_model: None,
            identifier_policy: default_identifier_policy(),
            tool_result_retrim_chars: default_tool_result_retrim_chars(),
            tool_result_trim_exempt: Vec::new(),
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

/// Browser delegation tool: hand browser-based tasks to a CLI subprocess (e.g. claude-in-chrome) (`[browser_delegate]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "browser-delegate"]
pub struct BrowserDelegateConfig {
    /// Enable browser delegation. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// CLI binary to spawn for browser tasks. Default: `"claude"`.
    #[serde(default = "default_browser_cli")]
    pub cli_binary: String,
    /// Persistent Chrome profile directory; empty means a fresh profile each invocation.
    #[serde(default)]
    pub chrome_profile_dir: String,
    /// Domain allowlist; empty means all non-blocked domains are permitted.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Domain denylist; takes precedence over `allowed_domains`.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Per-task timeout in seconds. Default: `120`.
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

/// Per-tool trust scoring with decay and regression detection (`[trust]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "trust"]
pub struct TrustConfig {
    /// Starting trust score for a new tool (`0.0`–`1.0`). Default: `0.8`.
    #[serde(default = "default_initial_score")]
    pub initial_score: f64,
    /// Days for an unused tool's score to decay by half. Default: `30.0`.
    #[serde(default = "default_decay_half_life")]
    pub decay_half_life_days: f64,
    /// Score below which the tool is treated as untrusted. Default: `0.5`.
    #[serde(default = "default_regression_threshold")]
    pub regression_threshold: f64,
    /// Score penalty applied when the operator corrects the tool. Default: `0.05`.
    #[serde(default = "default_correction_penalty")]
    pub correction_penalty: f64,
    /// Score boost applied per successful invocation. Default: `0.01`.
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
    "ZeroClaw Message".into()
}
fn default_max_attachment_bytes() -> usize {
    25 * 1024 * 1024
}

/// Email channel via IMAP (inbound, IDLE-first) and SMTP (outbound) (`[channels.email]`).
#[derive(Debug, Clone, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.email"]
pub struct EmailConfig {
    /// Enable the email channel. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// IMAP server hostname.
    pub imap_host: String,
    /// IMAP server port. Default: `993` (implicit TLS).
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// IMAP folder to monitor. Default: `"INBOX"`.
    #[serde(default = "default_imap_folder")]
    pub imap_folder: String,
    /// SMTP server hostname.
    pub smtp_host: String,
    /// SMTP server port. Default: `465` (implicit TLS).
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Use TLS for SMTP. Default: `true`.
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
    /// IMAP/SMTP username (typically the account email address).
    pub username: String,
    /// IMAP/SMTP password or app password.
    #[secret]
    pub password: String,
    /// `From:` header for outbound mail. Typically the same address as `username`.
    pub from_address: String,
    /// IMAP IDLE timeout before re-issuing (seconds). Default: `1740` (29 min).
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// Polling interval used when the IMAP server does not advertise the IDLE
    /// capability (RFC 2177). Ignored when IDLE is available. Default: `60`.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Inbound sender allowlist. Empty = accept all.
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Default subject when the agent originates a thread. Default: `"ZeroClaw Message"`.
    #[serde(default = "default_subject")]
    pub default_subject: String,
    /// Cap on inbound attachment size in bytes. Default: `26214400` (25 MiB).
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: usize,
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
            username: String::new(),
            password: String::new(),
            from_address: String::new(),
            idle_timeout_secs: default_idle_timeout(),
            poll_interval_secs: default_poll_interval_secs(),
            allowed_senders: Vec::new(),
            default_subject: default_subject(),
            max_attachment_bytes: default_max_attachment_bytes(),
        }
    }
}

fn default_label_filter() -> Vec<String> {
    vec!["INBOX".into()]
}

/// Gmail Push channel: real-time delivery via Google Cloud Pub/Sub (`[channels.gmail_push]`).
#[derive(Debug, Clone, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.gmail"]
pub struct GmailPushConfig {
    /// Enable the Gmail Push channel. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Pub/Sub topic Gmail publishes to (e.g. `"projects/foo/topics/gmail-inbox"`).
    pub topic: String,
    /// Restrict to specific Gmail labels. Default: `["INBOX"]`.
    #[serde(default = "default_label_filter")]
    pub label_filter: Vec<String>,
    /// OAuth token authenticating against the Gmail API.
    #[serde(default)]
    #[secret]
    pub oauth_token: String,
    /// Inbound sender allowlist. Empty = accept all.
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Public URL Pub/Sub posts notifications to.
    #[serde(default)]
    pub webhook_url: String,
    /// Shared secret for verifying inbound notifications.
    #[serde(default)]
    pub webhook_secret: String,
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
            allowed_senders: Vec::new(),
            webhook_url: String::new(),
            webhook_secret: String::new(),
        }
    }
}

/// ClawdTalk: real-time SIP voice via Telnyx (`[channels.clawdtalk]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.clawdtalk"]
pub struct ClawdTalkConfig {
    /// Enable ClawdTalk. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Telnyx API key.
    #[secret]
    pub api_key: String,
    /// Telnyx SIP connection ID.
    pub connection_id: String,
    /// Caller-ID for outbound dials.
    pub from_number: String,
    /// Destinations allowed for outbound dialing. Empty = no outbound dials permitted.
    #[serde(default)]
    pub allowed_destinations: Vec<String>,
    /// Optional shared secret for verifying inbound Telnyx webhook deliveries.
    #[serde(default)]
    #[secret]
    pub webhook_secret: Option<String>,
}

impl ChannelConfig for ClawdTalkConfig {
    fn name() -> &'static str {
        "ClawdTalk"
    }
    fn desc() -> &'static str {
        "ClawdTalk Channel"
    }
}

/// Which telephony provider to use.
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

/// Voice Call: traditional carrier voice via Twilio, Telnyx, or Plivo (`[channels.voice_call]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.voice-call"]
pub struct VoiceCallConfig {
    /// Enable Voice Call. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Carrier (`"twilio"`, `"telnyx"`, or `"plivo"`). Default: `"twilio"`.
    #[serde(default)]
    pub provider: VoiceProvider,
    /// Provider-specific account identifier (e.g. Twilio Account SID).
    pub account_id: String,
    /// Provider-specific auth token.
    pub auth_token: String,
    /// Caller-ID for outbound calls.
    pub from_number: String,
    /// Port for the embedded webhook server. Default: `8090`.
    #[serde(default = "default_webhook_port")]
    pub webhook_port: u16,
    /// Require operator approval before placing outbound calls. Default: `true`.
    #[serde(default = "default_true")]
    pub require_outbound_approval: bool,
    /// Persist call transcripts. Default: `true`.
    #[serde(default = "default_true")]
    pub transcription_logging: bool,
    /// Override the TTS voice ID (provider-specific). Uses the default voice when unset.
    #[serde(default)]
    pub tts_voice: Option<String>,
    /// Hard cap on call duration in seconds. Default: `3600` (1 hour).
    #[serde(default = "default_max_call_duration")]
    pub max_call_duration_secs: u64,
    /// Public base URL when the gateway is behind a tunnel/proxy. Used to construct outbound webhook URLs.
    #[serde(default)]
    pub webhook_base_url: Option<String>,
}

impl Default for VoiceCallConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: VoiceProvider::default(),
            account_id: String::new(),
            auth_token: String::new(),
            from_number: String::new(),
            webhook_port: default_webhook_port(),
            require_outbound_approval: default_true(),
            transcription_logging: default_true(),
            tts_voice: None,
            max_call_duration_secs: default_max_call_duration(),
            webhook_base_url: None,
        }
    }
}
