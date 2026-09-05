use crate::agent::personality;
use crate::identity;
use crate::security::AutonomyLevel;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::Result;
use chrono::{Datelike, Local};
use std::borrow::Cow;
use std::fmt::Write;
use std::path::Path;
use zeroclaw_config::schema::IdentityConfig;
use zeroclaw_providers::ChatMessage;

/// Closed identifier supplied by a trusted interaction client. The identifier
/// selects host-owned descriptive semantics; it never carries prompt prose or
/// capability claims from the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionSurface {
    ZerocodeCode,
}

impl InteractionSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZerocodeCode => "zerocode_code",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "zerocode_code" => Some(Self::ZerocodeCode),
            _ => None,
        }
    }

    /// Resolve the client identifier into the canonical host-owned facts that
    /// may be described to the model.
    pub fn resolve(self) -> InteractionContext {
        match self {
            Self::ZerocodeCode => InteractionContext {
                surface: self,
                mode: InteractionMode::InteractiveCoding,
                response_delivery: ResponseDelivery::CurrentTranscript,
                workspace: WorkspaceBinding::ActiveSessionWorkingDirectory,
                tools_and_approvals: ToolAuthority::RuntimeEnforced,
                memory: MemoryAccess::PersistentMemoryDisabled,
                persistence: SessionPersistence::HostStoredTranscript,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    InteractiveCoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDelivery {
    CurrentTranscript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceBinding {
    ActiveSessionWorkingDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAuthority {
    RuntimeEnforced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryAccess {
    PersistentMemoryDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPersistence {
    HostStoredTranscript,
}

/// Product-neutral description of the active user-facing interaction harness.
/// All fields are resolved by ZeroClaw from a closed surface identifier and
/// canonical session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionContext {
    surface: InteractionSurface,
    mode: InteractionMode,
    response_delivery: ResponseDelivery,
    workspace: WorkspaceBinding,
    tools_and_approvals: ToolAuthority,
    memory: MemoryAccess,
    persistence: SessionPersistence,
}

pub(crate) const TIMESTAMP_ORIENTATION: &str = "This is an interactive conversation with a user; a leading `[CURRENT DATE & TIME: ...]` line on their message is timestamp metadata added by the runtime, not log or API data — treat it as an ordinary conversational message and respond naturally and directly.\n\n";
pub(crate) const SYSTEM_PROMPT_TRUNCATION_MARKER: &str =
    "\n\n[System prompt truncated to fit context budget]\n";
const SESSION_PROMPTS_EXPORT_MARKER: &str = "\n\n[Persistent session prompts omitted from export]";
const SESSION_PROMPT_TOOL_EXCHANGE_EXPORT_MARKER: &str =
    "[Session-prompt tool exchange omitted from export]";

/// Return an observability-safe view of a host system prompt.
///
/// Session-prompt attachments are appended as the final host-owned section.
/// They are provider input, not diagnostic, hook, or telemetry content.
pub(crate) fn redact_session_prompt_attachments_for_export(prompt: &str) -> Cow<'_, str> {
    let section_prefix = format!(
        "\n\n{}",
        zeroclaw_infra::session_prompts::SESSION_PROMPTS_SECTION_PREFIX
    );
    let Some(start) = prompt.find(&section_prefix) else {
        return Cow::Borrowed(prompt);
    };
    Cow::Owned(format!(
        "{}{SESSION_PROMPTS_EXPORT_MARKER}",
        &prompt[..start]
    ))
}

/// Replace sensitive session-prompt tool exchanges at non-provider export
/// boundaries. The provider keeps the raw history; observers, hooks, logs, and
/// retained transcripts do not receive opaque attachment bodies.
pub fn redact_session_prompt_tool_exchanges_for_export(
    messages: &[ChatMessage],
) -> Vec<ChatMessage> {
    // A native batch can produce several `tool` messages, while the XML text
    // protocol uses one following `user` message for all results. Keep those
    // states separate: a user message after native results is ordinary next-
    // turn input and must not be swallowed by export redaction.
    let mut redact_native_tool_results = false;
    let mut redact_text_protocol_result = false;

    messages
        .iter()
        .map(|message| {
            let is_sensitive_call = message.role == "assistant"
                && session_prompt_tool_call_envelope_mentioned(&message.content);
            let is_native_result = message.role == "tool";
            let is_text_protocol_result = message.role == "user";
            let redact = is_sensitive_call
                || (redact_native_tool_results && is_native_result)
                || (redact_text_protocol_result && is_text_protocol_result);

            if message.role == "assistant" {
                // Provider adapters may retain JSON itself or its escaped text
                // representation, so test the stable field name after the
                // message is already known to name a sensitive tool.
                let native_batch = message.content.contains("tool_calls");
                redact_native_tool_results = is_sensitive_call && native_batch;
                redact_text_protocol_result = is_sensitive_call && !native_batch;
            } else if is_text_protocol_result {
                redact_text_protocol_result = false;
            }

            if redact {
                ChatMessage {
                    role: message.role.clone(),
                    content: SESSION_PROMPT_TOOL_EXCHANGE_EXPORT_MARKER.to_string(),
                }
            } else if message.role == "system" {
                ChatMessage {
                    role: message.role.clone(),
                    content: redact_session_prompt_attachments_for_export(&message.content)
                        .into_owned(),
                }
            } else {
                message.clone()
            }
        })
        .collect()
}

/// Redact a text-protocol provider response before it crosses an export
/// boundary. Native tool calls are represented separately, but this response
/// string may contain a complete XML tool call including an attachment body.
pub(crate) fn redact_session_prompt_text_protocol_for_export(content: &str) -> Cow<'_, str> {
    if session_prompt_tool_call_envelope_mentioned(content) {
        Cow::Borrowed(SESSION_PROMPT_TOOL_EXCHANGE_EXPORT_MARKER)
    } else {
        Cow::Borrowed(content)
    }
}

/// Identify a session-prompt invocation embedded in a provider tool envelope.
///
/// Mentioning a tool name in normal user, assistant, or system prose is not a
/// sensitive exchange. Provider adapters may retain native JSON as literal or
/// escaped text, while malformed XML is still a log-sensitive tool envelope.
pub(crate) fn session_prompt_tool_call_envelope_mentioned(content: &str) -> bool {
    let has_tool_envelope = content.contains("<tool_call")
        || content.contains("\"tool_calls\"")
        || content.contains("\\\"tool_calls\\\"");
    has_tool_envelope
        && zeroclaw_api::SESSION_PROMPT_TOOL_NAMES
            .iter()
            .any(|name| content.contains(name))
}

/// Whether a tool belongs in the model-visible catalog for this turn.
///
/// Session-prompt tools are registered in the sealed agent registry so their
/// execution path can be enabled for durable primary chat turns. Their
/// availability is nevertheless per-turn: advertising them outside that
/// capability scope would invite a call the runtime must reject.
pub(crate) fn tool_is_advertised_for_current_turn(name: &str) -> bool {
    zeroclaw_api::TOOL_LOOP_SESSION_PROMPTS_ALLOWED
        .try_with(|allowed| *allowed)
        .unwrap_or(false)
        || !zeroclaw_api::SESSION_PROMPT_TOOL_NAMES.contains(&name)
}

pub(crate) fn append_timestamp_orientation(prompt: &mut String) {
    prompt.push_str(TIMESTAMP_ORIENTATION);
}

/// Truncate a host-authored system prompt to a finite character budget.
///
/// The timestamp orientation is runtime-critical and is retained with the
/// truncation marker, matching the legacy system-prompt construction path.
pub(crate) fn truncate_system_prompt_to_budget(prompt: &mut String, max_chars: usize) {
    if max_chars == 0 || prompt.len() <= max_chars {
        return;
    }

    let reserved = TIMESTAMP_ORIENTATION.len() + SYSTEM_PROMPT_TRUNCATION_MARKER.len();
    if max_chars >= reserved {
        let mut end = max_chars - reserved;
        while end > 0 && !prompt.is_char_boundary(end) {
            end -= 1;
        }
        prompt.truncate(end);
        prompt.push_str(SYSTEM_PROMPT_TRUNCATION_MARKER);
        append_timestamp_orientation(prompt);
    } else {
        let mut end = max_chars.min(TIMESTAMP_ORIENTATION.len());
        while end > 0 && !TIMESTAMP_ORIENTATION.is_char_boundary(end) {
            end -= 1;
        }
        prompt.clear();
        prompt.push_str(&TIMESTAMP_ORIENTATION[..end]);
    }
}

/// Reserve a finite system-prompt budget for mandatory session attachments.
///
/// Attachments are durable session context, so callers must never silently
/// omit them. A finite budget may truncate a prompt without attachments, but
/// an attachment-bearing turn must retain the complete host prompt: dropping
/// its tail could remove safety or runtime policy while retaining mutable
/// session context.
pub fn append_required_session_prompt_attachments(
    prompt: &mut String,
    attachments: &str,
    max_chars: usize,
) -> Result<()> {
    if attachments.is_empty() {
        truncate_system_prompt_to_budget(prompt, max_chars);
        return Ok(());
    }

    let attachment_len = attachments.len().saturating_add(2);
    let total_len = prompt.len().saturating_add(attachment_len);
    if max_chars > 0 && total_len > max_chars {
        anyhow::bail!(
            "Persistent session prompts and required host context exceed max_system_prompt_chars ({max_chars}); refusing to dispatch without them"
        );
    }
    prompt.push_str("\n\n");
    prompt.push_str(attachments);
    Ok(())
}

pub struct PromptContext<'a> {
    pub workspace_dir: &'a Path,
    pub agent_workspace_dir: &'a Path,
    pub model_name: &'a str,
    pub tools: &'a [Box<dyn Tool>],
    pub skills: &'a [Skill],
    pub skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
    pub identity_config: Option<&'a IdentityConfig>,
    pub interaction: Option<&'a InteractionContext>,
    pub dispatcher_instructions: &'a str,
    /// True when the provider request carries native tool specs. In that mode
    /// the prompt must not duplicate the same tool catalog in prose.
    pub sends_native_tool_specs: bool,
    /// Pre-rendered security policy summary for inclusion in the Safety
    /// prompt section.  When present, the LLM sees the concrete constraints
    /// (allowed commands, forbidden paths, autonomy level) so it can plan
    /// tool calls without trial-and-error.  See
    pub security_summary: Option<String>,
    /// Autonomy level from config. Controls whether the safety section
    /// includes "ask before acting" instructions. Full autonomy omits them
    /// so the model executes tools directly without simulating approval.
    pub autonomy_level: AutonomyLevel,
    /// The shell the runtime adapter will spawn, or `None` for a shell-less
    /// runtime (which omits the `Shell:` field and the dialect guidance).
    /// Resolved from `RuntimeAdapter::shell_profile` so the reported shell
    /// cannot drift from the executed one.
    pub shell_profile: Option<zeroclaw_api::runtime_traits::ShellProfile>,
}

pub trait PromptSection: Send + Sync {
    fn name(&self) -> &str;
    fn build(&self, ctx: &PromptContext<'_>) -> Result<String>;
}

#[derive(Default)]
pub struct SystemPromptBuilder {
    sections: Vec<Box<dyn PromptSection>>,
}

impl SystemPromptBuilder {
    pub fn with_defaults() -> Self {
        Self {
            sections: vec![
                Box::new(DateTimeSection),
                Box::new(InteractionSection),
                Box::new(IdentitySection),
                Box::new(ToolHonestySection),
                Box::new(ToolsSection),
                Box::new(SafetySection),
                Box::new(ShellSection),
                Box::new(SkillsSection),
                Box::new(WorkspaceSection),
                Box::new(RuntimeSection),
                Box::new(ChannelMediaSection),
            ],
        }
    }

    pub fn add_section(mut self, section: Box<dyn PromptSection>) -> Self {
        self.sections.push(section);
        self
    }

    pub fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut output = String::new();
        for section in &self.sections {
            let part = section.build(ctx)?;
            if part.trim().is_empty() {
                continue;
            }
            output.push_str(part.trim_end());
            output.push_str("\n\n");
        }
        Ok(output)
    }
}

pub struct IdentitySection;
pub struct InteractionSection;
pub struct ToolHonestySection;
pub struct ToolsSection;
pub struct SafetySection;
pub struct SkillsSection;
pub struct WorkspaceSection;
pub struct RuntimeSection;
pub struct ShellSection;
pub struct DateTimeSection;
pub struct ChannelMediaSection;

impl PromptSection for InteractionSection {
    fn name(&self) -> &str {
        "interaction"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let Some(interaction) = ctx.interaction else {
            return Ok(String::new());
        };

        let surface = match interaction.surface {
            InteractionSurface::ZerocodeCode => "ZeroCode Code (ACP)",
        };
        let mode = match interaction.mode {
            InteractionMode::InteractiveCoding => "interactive coding session",
        };
        let response_delivery = match interaction.response_delivery {
            ResponseDelivery::CurrentTranscript => "shown in the current ZeroCode transcript",
        };
        let workspace = match interaction.workspace {
            WorkspaceBinding::ActiveSessionWorkingDirectory => {
                "the active session working directory"
            }
        };
        let tools_and_approvals = match interaction.tools_and_approvals {
            ToolAuthority::RuntimeEnforced => {
                "provided and enforced by the ZeroClaw runtime; this description grants no capabilities"
            }
        };
        let memory = match interaction.memory {
            MemoryAccess::PersistentMemoryDisabled => {
                "persistent memory is unavailable in this session"
            }
        };
        let persistence = match interaction.persistence {
            SessionPersistence::HostStoredTranscript => {
                "conversation history is stored by the host for resume"
            }
        };

        Ok(format!(
            "## Interaction Context\n\n\
             Surface: {surface}\n\
             Mode: {mode}\n\
             User messages: direct conversation, not API payloads or log records\n\
             Response delivery: {response_delivery}\n\
             Workspace: {workspace}\n\
             Tools and approvals: {tools_and_approvals}\n\
             Memory: {memory}\n\
             Session persistence: {persistence}"
        ))
    }
}

impl PromptSection for IdentitySection {
    fn name(&self) -> &str {
        "identity"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut prompt = String::from("## Project Context\n\n");
        let mut has_aieos = false;
        if let Some(config) = ctx.identity_config
            && identity::is_aieos_configured(config)
            && let Ok(Some(aieos)) = identity::load_aieos_identity(config, ctx.agent_workspace_dir)
        {
            let rendered = identity::aieos_to_system_prompt(&aieos);
            if !rendered.is_empty() {
                prompt.push_str(&rendered);
                prompt.push_str("\n\n");
                has_aieos = true;
            }
        }

        if !has_aieos {
            prompt.push_str(
                "The following workspace files define your identity, behavior, and context.\n\n",
            );
        }

        let profile = personality::load_personality(ctx.agent_workspace_dir);
        prompt.push_str(&profile.render());

        Ok(prompt)
    }
}

impl PromptSection for ToolHonestySection {
    fn name(&self) -> &str {
        "tool_honesty"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        if !ctx
            .tools
            .iter()
            .any(|tool| tool_is_advertised_for_current_turn(tool.name()))
        {
            return Ok(String::new());
        }

        Ok(
            "## CRITICAL: Tool Honesty\n\n\
             - NEVER fabricate, invent, or guess tool results. If a tool returns empty results, say \"No results found.\"\n\
             - If a tool call fails, report the error — never make up data to fill the gap.\n\
             - When unsure whether a tool call succeeded, ask the user rather than guessing."
                .into(),
        )
    }
}

impl PromptSection for ToolsSection {
    fn name(&self) -> &str {
        "tools"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        if !ctx
            .tools
            .iter()
            .any(|tool| tool_is_advertised_for_current_turn(tool.name()))
        {
            return Ok(String::new());
        }
        if ctx.sends_native_tool_specs {
            return Ok(ctx.dispatcher_instructions.to_string());
        }

        let mut out = String::from("## Tools\n\n");
        for tool in ctx
            .tools
            .iter()
            .filter(|tool| tool_is_advertised_for_current_turn(tool.name()))
        {
            let i18n_description = crate::i18n::get_tool_description(tool.name());
            let desc = i18n_description.unwrap_or_else(|| tool.description());
            let _ = writeln!(
                out,
                "- **{}**: {}\n  Parameters: `{}`",
                tool.name(),
                desc,
                tool.parameters_schema()
            );
        }
        if !ctx.dispatcher_instructions.is_empty() {
            out.push('\n');
            out.push_str(ctx.dispatcher_instructions);
        }
        Ok(out)
    }
}

impl PromptSection for SafetySection {
    fn name(&self) -> &str {
        "safety"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut out = String::from("## Safety\n\n- Do not exfiltrate private data.\n");

        // Omit "ask before acting" instructions when autonomy is Full —
        // mirrors build_system_prompt_with_mode_and_autonomy.
        if ctx.autonomy_level != AutonomyLevel::Full {
            out.push_str(
                "- Do not run destructive commands without asking.\n\
                 - Do not bypass oversight or approval mechanisms.\n",
            );
        }

        // Deletion advice follows the dialect: `trash` is POSIX-only, so
        // recommending it to a PowerShell or `cmd.exe` session would name a
        // command that is not there.
        out.push_str(ctx.shell_profile.as_ref().map_or(
            zeroclaw_api::runtime_traits::POSIX_DELETION_GUIDANCE,
            zeroclaw_api::runtime_traits::ShellProfile::safe_deletion_guidance,
        ));
        out.push_str(match ctx.autonomy_level {
            AutonomyLevel::Full => {
                "- Execute tools and actions directly — no extra approval needed.\n\
                 - You have full access to all configured tools. Use them confidently to accomplish tasks.\n\
                 - Only refuse an action if the runtime explicitly rejects it — do not preemptively decline."
            }
            AutonomyLevel::ReadOnly => {
                "- This runtime is read-only. Write operations will be rejected by the runtime if attempted.\n\
                 - Use read-only tools freely and confidently."
            }
            AutonomyLevel::Supervised => {
                "- Ask for approval when the runtime policy requires it for the specific action.\n\
                 - Do not preemptively refuse actions — attempt them and let the runtime enforce restrictions.\n\
                 - Use available tools confidently; the security policy will enforce boundaries."
            }
        });

        // Append concrete security policy constraints when available.
        // This tells the LLM exactly what commands are allowed, which paths
        // are off-limits, etc. — preventing wasteful trial-and-error.
        if let Some(ref summary) = ctx.security_summary {
            out.push_str("\n\n### Active Security Policy\n\n");
            out.push_str(summary);
        }

        Ok(out)
    }
}

impl PromptSection for SkillsSection {
    fn name(&self) -> &str {
        "skills"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mode = crate::skills::skills_prompt_mode_with_loader_fallback(
            ctx.skills_prompt_mode,
            ctx.tools.iter().any(|tool| {
                tool_is_advertised_for_current_turn(tool.name()) && tool.name() == "read_skill"
            }),
        );
        Ok(crate::skills::skills_to_prompt_with_mode_and_availability(
            ctx.skills,
            ctx.workspace_dir,
            mode,
            |name| {
                ctx.tools.iter().any(|tool| {
                    tool_is_advertised_for_current_turn(tool.name()) && tool.name() == name
                })
            },
        ))
    }
}

impl PromptSection for WorkspaceSection {
    fn name(&self) -> &str {
        "workspace"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        Ok(format!(
            "## Workspace\n\nWorking directory: `{}`",
            ctx.workspace_dir.display()
        ))
    }
}

impl PromptSection for RuntimeSection {
    fn name(&self) -> &str {
        "runtime"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let host =
            hostname::get().map_or_else(|_| "unknown".into(), |h| h.to_string_lossy().to_string());
        // The shell sits next to the OS because the OS alone does not
        // determine it: on Windows `cmd.exe` and PowerShell are both
        // reachable. Omitted for shell-less runtimes.
        match &ctx.shell_profile {
            Some(profile) => Ok(format!(
                "## Runtime\n\nHost: {host} | OS: {} | Shell: {} | Model: {}",
                std::env::consts::OS,
                profile.name,
                ctx.model_name
            )),
            None => Ok(format!(
                "## Runtime\n\nHost: {host} | OS: {} | Model: {}",
                std::env::consts::OS,
                ctx.model_name
            )),
        }
    }
}

impl PromptSection for ShellSection {
    fn name(&self) -> &str {
        "shell"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        // Only when a registered tool takes a model-authored command: the
        // syntax list is dead weight otherwise. An empty string is dropped by
        // the builder, so this section costs nothing when skipped.
        if !zeroclaw_api::runtime_traits::needs_shell_dialect_guidance(
            ctx.tools
                .iter()
                .filter(|tool| tool_is_advertised_for_current_turn(tool.name()))
                .map(|tool| tool.name()),
        ) {
            return Ok(String::new());
        }
        Ok(ctx
            .shell_profile
            .as_ref()
            .map(zeroclaw_api::runtime_traits::ShellProfile::prompt_section)
            .unwrap_or_default())
    }
}

impl PromptSection for DateTimeSection {
    fn name(&self) -> &str {
        "datetime"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        let now = Local::now();
        // Force Gregorian year to avoid confusion with local calendars (e.g. Buddhist calendar).
        let (year, month, day) = (now.year(), now.month(), now.day());

        Ok(format!(
            "## CRITICAL CONTEXT: CURRENT DATE\n\n\
             The following is the ABSOLUTE TRUTH regarding the current date. \
             Use this for all relative time calculations (e.g. \"last 7 days\").\n\n\
             Date: {year:04}-{month:02}-{day:02}\n\
             UTC offset: {}",
            now.format("%:z")
        ))
    }
}

impl PromptSection for ChannelMediaSection {
    fn name(&self) -> &str {
        "channel_media"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Ok("## Channel Media Markers\n\n\
            Messages from channels may contain media markers:\n\
            - `[Voice] <text>` — The user sent a voice/audio message that has already been transcribed to text. Respond to the transcribed content directly.\n\
            - `[IMAGE:<path>]` — An image attachment, processed by the vision pipeline.\n\
            - `[Document: <name>] <path>` — A file attachment saved to the workspace."
            .into())
    }
}

#[cfg(test)]
mod tests {
    use zeroclaw_providers::ChatMessage;

    use super::*;
    use async_trait::async_trait;
    use zeroclaw_api::tool::Tool;

    #[test]
    fn export_view_omits_trailing_session_prompt_attachments() {
        let raw =
            "host context\n\n## Session Prompts\n- id: \"task\"; content: \"private marker\"\n";
        let redacted = redact_session_prompt_attachments_for_export(raw);
        assert!(!redacted.contains("private marker"));
        assert!(redacted.contains("host context"));
        assert!(redacted.contains("omitted from export"));
    }

    #[test]
    fn attachments_fail_closed_before_host_safety_context_is_truncated() {
        let mut prompt =
            "## Identity\n\ntrusted host context\n\n## Safety\n\nmandatory policy".to_string();
        let original = prompt.clone();
        let attachments = "## Session Prompts\n\n[task] persistent instruction";
        let error = append_required_session_prompt_attachments(
            &mut prompt,
            attachments,
            original.len() + attachments.len() + 1,
        )
        .expect_err("one-byte overflow must fail instead of truncating host policy");

        assert!(error.to_string().contains("required host context"));
        assert_eq!(
            prompt, original,
            "failed composition must preserve the host prompt"
        );
    }

    #[test]
    fn attachments_append_after_the_complete_host_prompt_when_they_fit() {
        let mut prompt = "## Safety\n\nmandatory policy".to_string();
        let attachments = "## Session Prompts\n\n[task] persistent instruction";
        let budget = prompt.len() + 2 + attachments.len();

        append_required_session_prompt_attachments(&mut prompt, attachments, budget).unwrap();

        assert_eq!(
            prompt,
            format!("## Safety\n\nmandatory policy\n\n{attachments}")
        );
    }

    zeroclaw_api::mock_tool_attribution!(TestTool);
    zeroclaw_api::mock_tool_attribution!(ReadSkillTestTool);
    zeroclaw_api::mock_tool_attribution!(ShellTestTool);
    zeroclaw_api::mock_tool_attribution!(CronAddTestTool);
    zeroclaw_api::mock_tool_attribution!(SkillToolTestTool);
    zeroclaw_api::mock_tool_attribution!(SessionPromptTestTool);

    struct TestTool;
    struct ReadSkillTestTool;
    /// Stands in for the real `shell` tool: `ShellSection` keys on the name.
    struct ShellTestTool;
    /// Stands in for `cron_add`, which also takes a model-authored command.
    struct CronAddTestTool;
    struct SkillToolTestTool(&'static str);

    #[async_trait]
    impl Tool for SkillToolTestTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "registered skill tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[async_trait]
    impl Tool for ShellTestTool {
        fn name(&self) -> &str {
            "shell"
        }

        fn description(&self) -> &str {
            "Execute a shell command in the workspace directory"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[async_trait]
    impl Tool for CronAddTestTool {
        fn name(&self) -> &str {
            "cron_add"
        }

        fn description(&self) -> &str {
            "Schedule a recurring shell command"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            "test_tool"
        }

        fn description(&self) -> &str {
            "tool desc"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    struct SessionPromptTestTool;

    #[async_trait]
    impl Tool for SessionPromptTestTool {
        fn name(&self) -> &str {
            "session_prompt_set"
        }

        fn description(&self) -> &str {
            "Attach durable session context"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[async_trait]
    impl Tool for ReadSkillTestTool {
        fn name(&self) -> &str {
            "read_skill"
        }

        fn description(&self) -> &str {
            "load skill instructions"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[test]
    fn identity_section_with_aieos_includes_workspace_files() {
        let workspace =
            std::env::temp_dir().join(format!("zeroclaw_prompt_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("AGENTS.md"),
            "Always respond with: AGENTS_MD_LOADED",
        )
        .unwrap();

        let identity_config = zeroclaw_config::schema::IdentityConfig {
            format: "aieos".into(),
            aieos_path: None,
            aieos_inline: Some(r#"{"identity":{"names":{"first":"Nova"}}}"#.into()),
        };

        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: &workspace,
            agent_workspace_dir: &workspace,
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: Some(&identity_config),
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let section = IdentitySection;
        let output = section.build(&ctx).unwrap();

        assert!(
            output.contains("Nova"),
            "AIEOS identity should be present in prompt"
        );
        assert!(
            output.contains("AGENTS_MD_LOADED"),
            "AGENTS.md content should be present even when AIEOS is configured"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn prompt_builder_assembles_sections() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(TestTool)];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "instr",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();
        assert!(prompt.contains("## Tools"));
        assert!(prompt.contains("test_tool"));
        assert!(prompt.contains("instr"));
    }

    #[test]
    fn tool_catalog_omits_session_prompt_tools_without_durable_turn_capability() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(SessionPromptTestTool)];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "XML tool protocol",
            sends_native_tool_specs: false,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let hidden = zeroclaw_api::TOOL_LOOP_SESSION_PROMPTS_ALLOWED
            .sync_scope(false, || ToolsSection.build(&ctx).unwrap());
        assert!(
            hidden.is_empty(),
            "an unsupported text-protocol turn must not advertise session-prompt tools"
        );

        let visible = zeroclaw_api::TOOL_LOOP_SESSION_PROMPTS_ALLOWED
            .sync_scope(true, || ToolsSection.build(&ctx).unwrap());
        assert!(visible.contains("session_prompt_set"));
        assert!(visible.contains("XML tool protocol"));
    }

    #[test]
    fn interaction_section_renders_only_host_owned_zerocode_code_facts() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let interaction = InteractionSurface::ZerocodeCode.resolve();
        let ctx = PromptContext {
            workspace_dir: Path::new("/private/project"),
            agent_workspace_dir: Path::new("/private/agent"),
            model_name: "secret-model-name",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: Some(&interaction),
            dispatcher_instructions: "",
            sends_native_tool_specs: false,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = InteractionSection.build(&ctx).unwrap();
        assert_eq!(
            output,
            "## Interaction Context\n\n\
             Surface: ZeroCode Code (ACP)\n\
             Mode: interactive coding session\n\
             User messages: direct conversation, not API payloads or log records\n\
             Response delivery: shown in the current ZeroCode transcript\n\
             Workspace: the active session working directory\n\
             Tools and approvals: provided and enforced by the ZeroClaw runtime; this description grants no capabilities\n\
             Memory: persistent memory is unavailable in this session\n\
             Session persistence: conversation history is stored by the host for resume"
        );
        assert!(!output.contains("/private"));
        assert!(!output.contains("secret-model-name"));
    }

    #[test]
    fn compact_prompt_keeps_interaction_context() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let interaction = InteractionSurface::ZerocodeCode.resolve();
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
            identity_config: None,
            interaction: Some(&interaction),
            dispatcher_instructions: "",
            sends_native_tool_specs: false,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();
        assert!(prompt.contains("## Interaction Context"));
        assert!(prompt.contains("Surface: ZeroCode Code (ACP)"));
    }

    #[test]
    fn prompt_builder_skips_tools_section_for_native_tool_specs() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(TestTool)];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: true,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();
        assert!(!prompt.contains("## Tools"));
        assert!(!prompt.contains("test_tool"));
        assert!(prompt.contains("## Safety"));
    }

    #[test]
    fn prompt_builder_omits_tool_sections_when_no_tools_available() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(!prompt.contains("## Tools"));
        assert!(!prompt.contains("## CRITICAL: Tool Honesty"));
        assert!(!prompt.contains("## Tool Use Protocol"));
        assert!(!prompt.contains("<tool_call>"));
        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("## Workspace"));
        assert!(prompt.contains("## Runtime"));
    }

    #[test]
    fn skills_section_includes_instructions_and_tools_in_full_mode() {
        let tools: Vec<Box<dyn Tool>> =
            vec![Box::new(SkillToolTestTool("deploy__release_checklist"))];
        let skills = vec![crate::skills::Skill {
            name: "deploy".into(),
            description: "Release safely".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "release_checklist".into(),
                description: "Validate release readiness".into(),
                kind: "shell".into(),
                command: "echo ok".into(),
                args: std::collections::HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
                timeout_secs: None,
            }],
            prompts: vec!["Run smoke tests before deploy.".into()],
            slash_options: Vec::new(),
            always: false,
            location: None,
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>deploy</name>"));
        assert!(output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        // Registered tools (shell kind) appear under <callable_tools> with prefixed names
        assert!(output.contains("<callable_tools"));
        assert!(output.contains("<name>deploy__release_checklist</name>"));
    }

    #[test]
    fn skills_section_compact_mode_omits_instructions_but_keeps_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(ReadSkillTestTool),
            Box::new(SkillToolTestTool("deploy__release_checklist")),
        ];
        let skills = vec![crate::skills::Skill {
            name: "deploy".into(),
            description: "Release safely".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "release_checklist".into(),
                description: "Validate release readiness".into(),
                kind: "shell".into(),
                command: "echo ok".into(),
                args: std::collections::HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
                timeout_secs: None,
            }],
            prompts: vec!["Run smoke tests before deploy.".into()],
            slash_options: Vec::new(),
            always: false,
            location: Some(Path::new("/tmp/workspace/skills/deploy/SKILL.md").to_path_buf()),
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            agent_workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>deploy</name>"));
        assert!(output.contains("<location>skills/deploy/SKILL.md</location>"));
        assert!(output.contains("read_skill(name)"));
        assert!(!output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        // Compact mode should still include tools so the LLM knows about them.
        // Registered tools (shell kind) appear under <callable_tools> with prefixed names.
        assert!(output.contains("<callable_tools"));
        assert!(output.contains("<name>deploy__release_checklist</name>"));
    }

    #[test]
    fn skills_section_preserves_instructions_when_compact_loader_is_unavailable() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let skills = vec![crate::skills::Skill {
            name: "deploy".into(),
            description: "Release safely".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec!["Run smoke tests before deploy.".into()],
            slash_options: Vec::new(),
            always: false,
            location: None,
        }];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        assert!(!output.contains("read_skill(name)"));
    }

    #[test]
    fn skills_section_compact_mode_keeps_instructions_for_always_skill() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(SkillToolTestTool(
            "security-policy__release_checklist",
        ))];
        let skills = vec![crate::skills::Skill {
            name: "security-policy".into(),
            description: "Critical safety rules".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "release_checklist".into(),
                description: "Validate release readiness".into(),
                kind: "shell".into(),
                command: "echo ok".into(),
                args: std::collections::HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
                timeout_secs: None,
            }],
            prompts: vec!["Never skip the safety review.".into()],
            slash_options: Vec::new(),
            always: true,
            location: Some(
                Path::new("/tmp/workspace/skills/security-policy/SKILL.md").to_path_buf(),
            ),
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            agent_workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>security-policy</name>"));
        // `always: true` forces instructions to stay inlined even in compact mode.
        assert!(output.contains("<instruction>Never skip the safety review.</instruction>"));
        // Tools are still listed as in any other skill.
        assert!(output.contains("<callable_tools"));
        assert!(output.contains("<name>security-policy__release_checklist</name>"));
    }

    #[test]
    fn datetime_section_includes_date_and_offset_without_wall_clock_time() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "instr",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let rendered = DateTimeSection.build(&ctx).unwrap();
        assert!(rendered.starts_with("## CRITICAL CONTEXT: CURRENT DATE\n\n"));
        assert!(!rendered.contains("CURRENT DATE & TIME"));

        let payload = rendered.trim_start_matches("## CRITICAL CONTEXT: CURRENT DATE\n\n");
        assert!(payload.chars().any(|c| c.is_ascii_digit()));
        assert!(payload.contains("Date:"));
        assert!(payload.contains("UTC offset:"));
        assert!(!payload.contains("Time:"));
        assert!(!payload.contains("ISO 8601:"));
    }

    #[test]
    fn prompt_builder_inlines_and_escapes_skills() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let skills = vec![crate::skills::Skill {
            name: "code<review>&".into(),
            description: "Review \"unsafe\" and 'risky' bits".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "run\"linter\"".into(),
                description: "Run <lint> & report".into(),
                kind: "shell&exec".into(),
                command: "cargo clippy".into(),
                args: std::collections::HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
                timeout_secs: None,
            }],
            prompts: vec!["Use <tool_call> and & keep output \"safe\"".into()],
            slash_options: Vec::new(),
            always: false,
            location: None,
        }];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            agent_workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>code&lt;review&gt;&amp;</name>"));
        assert!(prompt.contains(
            "<description>Review &quot;unsafe&quot; and &apos;risky&apos; bits</description>"
        ));
        assert!(prompt.contains("<name>run&quot;linter&quot;</name>"));
        assert!(prompt.contains("<description>Run &lt;lint&gt; &amp; report</description>"));
        assert!(prompt.contains("<kind>shell&amp;exec</kind>"));
        assert!(prompt.contains(
            "<instruction>Use &lt;tool_call&gt; and &amp; keep output &quot;safe&quot;</instruction>"
        ));
    }

    #[test]
    fn safety_section_includes_security_summary_when_present() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let summary = "**Autonomy level**: Supervised\n\
                        **Allowed shell commands**: `git`, `ls`.\n"
            .to_string();
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: Some(summary.clone()),
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            output.contains("## Safety"),
            "should contain base safety header"
        );
        assert!(
            output.contains("### Active Security Policy"),
            "should contain security policy header"
        );
        assert!(
            output.contains("Autonomy level"),
            "should contain autonomy level from summary"
        );
        assert!(
            output.contains("`git`"),
            "should contain allowed commands from summary"
        );
    }

    #[test]
    fn safety_section_omits_security_policy_when_none() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            output.contains("## Safety"),
            "should contain base safety header"
        );
        assert!(
            !output.contains("### Active Security Policy"),
            "should NOT contain security policy header when None"
        );
    }

    #[test]
    fn safety_section_full_autonomy_omits_approval_instructions() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Full,
            shell_profile: None,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            !output.contains("without asking"),
            "full autonomy should NOT include 'ask before acting' instructions"
        );
        assert!(
            !output.contains("bypass oversight"),
            "full autonomy should NOT include 'bypass oversight' instructions"
        );
        assert!(
            output.contains("Execute tools and actions directly"),
            "full autonomy should instruct to execute directly"
        );
        assert!(
            output.contains("Do not exfiltrate"),
            "full autonomy should still include data exfiltration guard"
        );
    }

    #[test]
    fn safety_section_supervised_includes_approval_instructions() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,

            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile: None,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            output.contains("without asking"),
            "supervised should include 'ask before acting' instructions"
        );
        assert!(
            output.contains("bypass oversight"),
            "supervised should include 'bypass oversight' instructions"
        );
    }

    /// Build a context for the shell-reporting sections. `tools` decides
    /// whether `ShellSection` fires; `shell_profile` is what it reports.
    fn shell_ctx<'a>(
        tools: &'a [Box<dyn Tool>],
        shell_profile: Option<zeroclaw_api::runtime_traits::ShellProfile>,
    ) -> PromptContext<'a> {
        PromptContext {
            workspace_dir: Path::new("/tmp"),
            agent_workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools,
            skills: &[],
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: false,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
            shell_profile,
        }
    }

    fn profile(
        name: &str,
        dialect: zeroclaw_api::runtime_traits::ShellDialect,
    ) -> zeroclaw_api::runtime_traits::ShellProfile {
        zeroclaw_api::runtime_traits::ShellProfile {
            name: name.to_string(),
            dialect,
        }
    }

    #[test]
    fn runtime_section_reports_the_configured_shell() {
        use zeroclaw_api::runtime_traits::ShellDialect;
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = shell_ctx(&tools, Some(profile("zsh", ShellDialect::Posix)));
        let output = RuntimeSection.build(&ctx).unwrap();
        assert!(output.contains("Shell: zsh"), "{output}");
        // The field sits between OS and Model so the line stays scannable.
        assert!(output.contains("| Shell: zsh | Model:"), "{output}");
    }

    #[test]
    fn runtime_section_omits_the_shell_field_for_a_shell_less_runtime() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = shell_ctx(&tools, None);
        let output = RuntimeSection.build(&ctx).unwrap();
        assert!(!output.contains("Shell:"), "{output}");
        // Everything else still renders, so a WASM runtime reads no worse
        // than it did before the field existed.
        assert!(
            output.contains("Host:") && output.contains("Model:"),
            "{output}"
        );
    }

    #[test]
    fn shell_section_is_silent_without_the_shell_tool() {
        // No way to run a command means the syntax table is dead weight.
        use zeroclaw_api::runtime_traits::ShellDialect;
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = shell_ctx(&tools, Some(profile("pwsh", ShellDialect::PowerShell)));
        assert!(ShellSection.build(&ctx).unwrap().is_empty());
    }

    #[test]
    fn shell_section_fires_for_a_cron_only_tool_surface() {
        // `cron_add` takes a model-authored `command` that runs through the
        // same interpreter, so the dialect matters even without `shell`.
        use zeroclaw_api::runtime_traits::ShellDialect;
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(CronAddTestTool)];
        let ctx = shell_ctx(&tools, Some(profile("pwsh", ShellDialect::PowerShell)));
        let output = ShellSection.build(&ctx).unwrap();
        assert!(output.contains("Get-ChildItem"), "{output}");
    }

    #[test]
    fn shell_section_is_silent_for_a_shell_less_runtime() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ShellTestTool)];
        let ctx = shell_ctx(&tools, None);
        assert!(ShellSection.build(&ctx).unwrap().is_empty());
    }

    #[test]
    fn shell_section_corrects_dialect_when_the_shell_tool_is_registered() {
        use zeroclaw_api::runtime_traits::ShellDialect;
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ShellTestTool)];

        let ps = ShellSection
            .build(&shell_ctx(
                &tools,
                Some(profile("pwsh", ShellDialect::PowerShell)),
            ))
            .unwrap();
        assert!(ps.contains("Get-ChildItem -Force"), "{ps}");
        assert!(ps.contains("`cmd` builtins"), "{ps}");

        let cmd = ShellSection
            .build(&shell_ctx(
                &tools,
                Some(profile("cmd", ShellDialect::WindowsCmd)),
            ))
            .unwrap();
        assert!(cmd.contains("findstr"), "{cmd}");
        assert!(cmd.contains("PowerShell cmdlets"), "{cmd}");

        // POSIX names the shell but carries no correction table: `ls`/`grep`
        // are already what the model reaches for.
        let posix = ShellSection
            .build(&shell_ctx(
                &tools,
                Some(profile("bash", ShellDialect::Posix)),
            ))
            .unwrap();
        assert!(posix.contains("`bash`"), "{posix}");
        assert!(!posix.contains("Get-ChildItem"), "{posix}");
        assert!(!posix.contains("findstr"), "{posix}");
    }

    #[test]
    fn safety_deletion_advice_names_a_command_the_dialect_has() {
        use zeroclaw_api::runtime_traits::ShellDialect;
        let tools: Vec<Box<dyn Tool>> = vec![];

        let posix = SafetySection
            .build(&shell_ctx(
                &tools,
                Some(profile("bash", ShellDialect::Posix)),
            ))
            .unwrap();
        assert!(posix.contains("trash"), "{posix}");

        let ps = SafetySection
            .build(&shell_ctx(
                &tools,
                Some(profile("pwsh", ShellDialect::PowerShell)),
            ))
            .unwrap();
        assert!(!ps.contains("trash"), "{ps}");
        assert!(ps.contains("-WhatIf"), "{ps}");

        // A shell-less runtime keeps the POSIX wording it rendered before.
        let none = SafetySection.build(&shell_ctx(&tools, None)).unwrap();
        assert!(none.contains("trash"), "{none}");
    }

    #[test]
    fn export_copy_redacts_native_prompt_tool_call_and_its_result() {
        let marker = "session-prompt-private-marker";
        let messages = vec![
            ChatMessage::assistant(format!(
                r#"{{\"tool_calls\":[{{\"name\":\"session_prompt_set\",\"arguments\":{{\"content\":\"{marker}\"}}}}]}}"#
            )),
            ChatMessage::tool(format!(r#"{{\"content\":\"{marker}\"}}"#)),
            ChatMessage::user("ordinary follow-up"),
        ];

        let export = redact_session_prompt_tool_exchanges_for_export(&messages);
        assert!(
            export
                .iter()
                .all(|message| !message.content.contains(marker))
        );
        assert_eq!(
            messages[0].content,
            format!(
                r#"{{\"tool_calls\":[{{\"name\":\"session_prompt_set\",\"arguments\":{{\"content\":\"{marker}\"}}}}]}}"#
            )
        );
        assert_eq!(export[2].content, "ordinary follow-up");
    }

    #[test]
    fn export_copy_redacts_text_protocol_prompt_call_and_result() {
        let marker = "session-prompt-private-marker";
        let messages = vec![
            ChatMessage::assistant(format!(
                r#"<tool_call>{{\"name\":\"session_prompt_set\",\"arguments\":{{\"content\":\"{marker}\"}}}}</tool_call>"#
            )),
            ChatMessage::user(format!(
                r#"[Tool results]\n<tool_result name=\"session_prompt_set\">{marker}</tool_result>"#
            )),
        ];

        let export = redact_session_prompt_tool_exchanges_for_export(&messages);
        assert!(
            export
                .iter()
                .all(|message| !message.content.contains(marker))
        );
    }

    #[test]
    fn export_copy_redacts_every_result_from_a_mixed_sensitive_batch() {
        let marker = "session-prompt-private-marker";
        let messages = vec![
            ChatMessage::assistant(format!(
                r#"{{"tool_calls":[{{"name":"shell","arguments":{{}}}},{{"name":"session_prompt_list","arguments":{{}}}}]}}"#
            )),
            ChatMessage::tool("shell output"),
            ChatMessage::tool(format!("prompt list: {marker}")),
            ChatMessage::assistant("next model response"),
            ChatMessage::user("ordinary next-turn input"),
        ];

        let export = redact_session_prompt_tool_exchanges_for_export(&messages);
        assert!(
            export
                .iter()
                .all(|message| !message.content.contains(marker)),
            "every result from a mixed sensitive batch is an export boundary"
        );
        assert_eq!(export[4].content, "ordinary next-turn input");
    }

    #[test]
    fn text_protocol_export_redactor_omits_prompt_tool_bodies() {
        let marker = "session-prompt-private-marker";
        let response = format!(
            r#"<tool_call>{{\"name\":\"session_prompt_set\",\"arguments\":{{\"content\":\"{marker}\"}}}}</tool_call>"#
        );
        let export = redact_session_prompt_text_protocol_for_export(&response);
        assert!(!export.contains(marker));
        assert!(export.contains("omitted from export"));
    }

    #[test]
    fn export_copy_preserves_ordinary_mentions_of_prompt_tools() {
        let messages = vec![
            ChatMessage::user("How do I use session_prompt_set?"),
            ChatMessage::assistant("Use session_prompt_set to attach context."),
            ChatMessage::system("The host documents session_prompt_set here."),
        ];

        let export = redact_session_prompt_tool_exchanges_for_export(&messages);
        assert_eq!(export.len(), messages.len());
        for (actual, expected) in export.iter().zip(&messages) {
            assert_eq!(actual.role, expected.role);
            assert_eq!(actual.content, expected.content);
        }
    }

    #[test]
    fn text_protocol_export_redactor_covers_malformed_prompt_envelopes() {
        let marker = "session-prompt-private-marker";
        let malformed =
            format!(r#"<tool_call {{\"name\":\"session_prompt_set\",\"content\":\"{marker}\""#);
        let export = redact_session_prompt_text_protocol_for_export(&malformed);
        assert!(!export.contains(marker));
        assert!(export.contains("omitted from export"));
    }
}
