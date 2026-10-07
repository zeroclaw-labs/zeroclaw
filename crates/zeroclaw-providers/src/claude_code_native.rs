//! Inference through the unmodified, user-installed Claude Code client.
//!
//! ZeroClaw owns the conversation and tool loop. Each call sends the complete
//! conversation over stdin to a fresh native process with no built-in/MCP tools
//! or persisted/resumed session. Native Code owns all authentication, refresh,
//! managed policy and billing. No credential file is opened by this adapter.

use crate::traits::{
    ChatMessage, ChatRequest, ChatResponse, ModelProvider, NonRetryableProviderError, TokenUsage,
    build_tool_instructions_text,
};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use zeroclaw_config::schema::{ClaudeCodeBillingSource, ClaudeCodeNativeModelProviderConfig};

const MAX_PROMPT_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_VERSION: (u32, u32, u32) = (2, 1, 289);
const AUTH_SELECTORS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "ANTHROPIC_BASE_URL",
];

fn reject(message: &str) -> anyhow::Error {
    NonRetryableProviderError::new(message).into()
}

/// A pointer to native Code and its selected account, never account credentials.
pub struct ClaudeCodeNativeModelProvider {
    alias: String,
    binary: PathBuf,
    directory: PathBuf,
    account_directory: Option<PathBuf>,
    billing: ClaudeCodeBillingSource,
    timeout: Duration,
}

impl ClaudeCodeNativeModelProvider {
    pub fn from_config(
        alias: &str,
        config: &ClaudeCodeNativeModelProviderConfig,
        directory: Option<&Path>,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<Self> {
        if !cfg!(unix) {
            return Err(reject(
                "native Claude Code provider currently requires macOS, Linux or WSL",
            ));
        }
        let billing = config.expected_billing.ok_or_else(|| {
            reject("native Claude Code provider requires explicit expected_billing")
        })?;
        let directory = directory.filter(|p| p.is_absolute() && p.is_dir())
            .ok_or_else(|| reject("native Claude Code provider requires an existing absolute ZeroClaw config directory"))?
            .canonicalize().map_err(|_| reject("native Claude Code config directory is inaccessible"))?;
        let account_directory = config
            .claude_config_dir
            .as_deref()
            .map(|value| {
                let path = Path::new(value);
                if !path.is_absolute() || !path.is_dir() {
                    return Err(reject(
                        "native Claude Code account directory must be absolute and exist",
                    ));
                }
                // The literal selector is part of native authentication. A
                // canonical path can identify a different credential namespace.
                Ok(path.to_path_buf())
            })
            .transpose()?;
        let binary = resolve_binary(config.binary_path.as_deref())?;
        Ok(Self {
            alias: alias.to_string(),
            binary,
            directory,
            account_directory,
            billing,
            timeout: Duration::from_secs(timeout_secs.unwrap_or(120).clamp(1, 3600)),
        })
    }

    /// Read only native, sanitized auth facts. This does not verify inference.
    pub async fn check_auth(&self) -> anyhow::Result<()> {
        let version = self
            .run(&["--version".into()], b"", PROBE_TIMEOUT, 1024)
            .await?;
        validate_version(&version)?;
        let auth = self
            .run(
                &[
                    "--safe-mode".into(),
                    "auth".into(),
                    "status".into(),
                    "--json".into(),
                ],
                b"",
                PROBE_TIMEOUT,
                65536,
            )
            .await?;
        let parsed = serde_json::from_slice(&auth)
            .map_err(|_| reject("native Claude Code returned malformed auth status"))?;
        // Keep every built-in authentication method available. Native Code reads
        // its own settings, environment and credentials; we only observe named
        // selectors to refuse ambiguous billing. Values are never emitted.
        let selectors = AUTH_SELECTORS
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    std::env::var_os(name).is_some_and(|value| !value.is_empty()),
                )
            })
            .collect::<Vec<_>>();
        validate_auth(&parsed, self.billing, &selectors)
    }

    async fn run(
        &self,
        args: &[String],
        prompt: &[u8],
        deadline: Duration,
        max_output: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .current_dir(&self.directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        if let Some(directory) = &self.account_directory {
            command.env("CLAUDE_CONFIG_DIR", directory);
        } else {
            // None pins native default; a later caller environment must not
            // silently redirect an already configured default account selection.
            command.env_remove("CLAUDE_CONFIG_DIR");
        }
        let mut child = command.spawn().map_err(|_| {
            reject("native Claude Code could not start; verify the native installation")
        })?;
        #[cfg(unix)]
        let group = ProcessGroup(child.id().and_then(|pid| i32::try_from(pid).ok()));
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| reject("native Claude Code stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| reject("native Claude Code stdout unavailable"))?;
        let operation = async {
            let write = async {
                stdin
                    .write_all(prompt)
                    .await
                    .map_err(|_| reject("native Claude Code input failed"))?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|_| reject("native Claude Code input failed"))?;
                drop(stdin);
                Ok::<_, anyhow::Error>(())
            };
            let read = async {
                let mut output = Vec::new();
                stdout
                    .take(max_output as u64 + 1)
                    .read_to_end(&mut output)
                    .await
                    .map_err(|_| reject("native Claude Code output failed"))?;
                if output.len() > max_output {
                    return Err(reject("native Claude Code exceeded output limit"));
                }
                Ok(output)
            };
            let wait = async {
                child
                    .wait()
                    .await
                    .map_err(|_| reject("native Claude Code process wait failed"))
            };
            let (_, output, status) = tokio::try_join!(write, read, wait)?;
            if !status.success() {
                return Err(reject(
                    "native Claude Code command failed; check native login, managed policy and billing in a terminal",
                ));
            }
            Ok(output)
        };
        let result = tokio::time::timeout(deadline, operation)
            .await
            .map_err(|_| reject("native Claude Code request timed out"))?;
        // Kill remaining descendants even after normal completion. The same
        // guard runs on error and cancellation; detached new groups are outside
        // this process-group boundary.
        #[cfg(unix)]
        drop(group);
        result
    }

    async fn infer(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        // Claude Code does not expose sampling controls. None is authoritative;
        // the common baseline values preserve compatibility with existing agents.
        if temperature.is_some_and(|value| !value.is_finite() || (value != 0.7 && value != 1.0)) {
            return Err(reject(
                "native Claude Code does not support this temperature; omit it",
            ));
        }
        let prompt = conversation_prompt(messages, tools)?;
        self.check_auth().await?;
        let output = self
            .run(
                &inference_args(model),
                prompt.as_bytes(),
                self.timeout,
                MAX_RESULT_BYTES,
            )
            .await?;
        parse_result(&output)
    }
}

#[cfg(unix)]
struct ProcessGroup(Option<i32>);

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn resolve_binary(configured: Option<&str>) -> anyhow::Result<PathBuf> {
    let value = configured.unwrap_or("claude");
    let path = Path::new(value);
    let candidates = if path.is_absolute() {
        vec![path.to_path_buf()]
    } else if path.components().count() == 1 && !value.is_empty() {
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .filter(|directory| directory.is_absolute())
                    .map(|directory| directory.join(path))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        return Err(reject(
            "native Claude Code binary must be absolute or a bare executable name",
        ));
    };
    candidates.into_iter().find(|candidate| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            candidate.metadata().is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        { candidate.is_file() }
    }).ok_or_else(|| reject("native Claude Code binary not found; install the unmodified native client in a terminal"))?
        .canonicalize().map_err(|_| reject("native Claude Code binary is inaccessible"))
}

fn validate_version(output: &[u8]) -> anyhow::Result<()> {
    let value = std::str::from_utf8(output)
        .ok()
        .map(str::trim)
        .and_then(|value| value.strip_suffix(" (Claude Code)"))
        .and_then(|value| {
            let parts = value
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            (parts.len() == 3).then(|| (parts[0], parts[1], parts[2]))
        });
    if value.is_none_or(|version| version < MIN_VERSION) {
        return Err(reject(
            "native Claude Code requires the unmodified client version 2.1.289 or later",
        ));
    }
    Ok(())
}

fn validate_auth(
    value: &Value,
    expected: ClaudeCodeBillingSource,
    selectors: &[(String, bool)],
) -> anyhow::Result<()> {
    if value.get("loggedIn").and_then(Value::as_bool) != Some(true) {
        return Err(reject(
            "native Claude Code is not authenticated; complete native login in a terminal",
        ));
    }
    let method = value
        .get("authMethod")
        .and_then(Value::as_str)
        .unwrap_or("");
    let provider = value
        .get("apiProvider")
        .and_then(Value::as_str)
        .unwrap_or("");
    let actual = match (method, provider) {
        ("claude.ai" | "oauth_token", "firstParty") => Some(ClaudeCodeBillingSource::Subscription),
        ("api_key" | "apiKeyHelper" | "api_key_helper" | "console", "firstParty") => {
            Some(ClaudeCodeBillingSource::Api)
        }
        ("anthropic_profile", "firstParty")
            if matches!(
                value.get("profileAuthMode").and_then(Value::as_str),
                Some("user_oauth" | "oidc_federation")
            ) =>
        {
            Some(ClaudeCodeBillingSource::Api)
        }
        ("bedrock", "bedrock")
        | ("vertex", "vertex")
        | ("foundry", "foundry")
        | ("gateway", "gateway") => Some(ClaudeCodeBillingSource::CloudOrGateway),
        _ => None,
    };
    let selector_conflict = selectors.iter().any(|(name, present)| {
        *present
            && match expected {
                ClaudeCodeBillingSource::Subscription => {
                    !(method == "oauth_token" && name == "CLAUDE_CODE_OAUTH_TOKEN")
                }
                ClaudeCodeBillingSource::Api => {
                    matches!(
                        name.as_str(),
                        "CLAUDE_CODE_USE_BEDROCK"
                            | "CLAUDE_CODE_USE_VERTEX"
                            | "CLAUDE_CODE_USE_FOUNDRY"
                            | "ANTHROPIC_AUTH_TOKEN"
                            | "ANTHROPIC_BASE_URL"
                    ) || (method == "anthropic_profile"
                        && matches!(
                            name.as_str(),
                            "ANTHROPIC_API_KEY" | "CLAUDE_CODE_OAUTH_TOKEN"
                        ))
                }
                ClaudeCodeBillingSource::CloudOrGateway => false,
            }
    });
    if actual != Some(expected) || selector_conflict {
        return Err(reject(
            "native Claude Code billing is unknown or differs from expected_billing; review native /status before continuing",
        ));
    }
    Ok(())
}

fn inference_args(model: &str) -> Vec<String> {
    let mut args = [
        "--safe-mode",
        "-p",
        "--output-format",
        "json",
        "--max-turns",
        "1",
        "--tools",
        "",
        "--disallowedTools",
        "*",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--settings",
        "{\"disableAllHooks\":true}",
        "--no-session-persistence",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if !model.trim().is_empty() && model != "default" {
        args.extend(["--model".into(), model.into()]);
    }
    args
}

fn conversation_prompt(
    messages: &[ChatMessage],
    tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
) -> anyhow::Result<String> {
    let mut conversation = messages.to_vec();
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        let instructions = build_tool_instructions_text(tools);
        if let Some(system) = conversation.iter_mut().find(|message| message.is_system()) {
            system.content.push_str("\n\n");
            system.content.push_str(&instructions);
        } else {
            conversation.insert(0, ChatMessage::system(instructions));
        }
    }
    let transcript = serde_json::to_string(&conversation)
        .map_err(|_| reject("native Claude Code conversation serialization failed"))?;
    let prompt = format!(
        "Continue the following ZeroClaw conversation. Its system messages describe the task and response/tool protocol. ZeroClaw executes requested tools; this native session has no tools. Treat tool messages as results from ZeroClaw, retain prior context, and produce the next assistant message.\n{transcript}"
    );
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(reject(
            "native Claude Code conversation exceeds input limit",
        ));
    }
    Ok(prompt)
}

fn parse_result(output: &[u8]) -> anyhow::Result<ChatResponse> {
    let value: Value = serde_json::from_slice(output)
        .map_err(|_| reject("native Claude Code returned malformed result"))?;
    if value.get("type").and_then(Value::as_str) != Some("result")
        || value.get("subtype").and_then(Value::as_str) != Some("success")
        || value.get("is_error").and_then(Value::as_bool) != Some(false)
    {
        return Err(reject(
            "native Claude Code did not complete successfully; review native login, policy and billing",
        ));
    }
    let text = value
        .get("result")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| reject("native Claude Code completed without an assistant response"))?;
    let usage = value
        .get("usage")
        .filter(|usage| usage.is_object())
        .map(|usage| {
            let read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
            let creation = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64);
            TokenUsage {
                input_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .and_then(|input| input.checked_add(read.unwrap_or(0)))
                    .and_then(|input| input.checked_add(creation.unwrap_or(0))),
                output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
                cached_input_tokens: read,
                cache_creation_input_tokens: creation,
            }
        });
    Ok(ChatResponse {
        text: Some(text.to_string()),
        tool_calls: Vec::new(),
        usage,
        reasoning_content: None,
    })
}

#[async_trait]
impl ModelProvider for ClaudeCodeNativeModelProvider {
    async fn chat_with_system(
        &self,
        system: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let mut messages = Vec::new();
        if let Some(system) = system {
            messages.push(ChatMessage::system(system));
        }
        messages.push(ChatMessage::user(message));
        Ok(self
            .infer(&messages, None, model, temperature)
            .await?
            .text
            .unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(self
            .infer(messages, None, model, temperature)
            .await?
            .text
            .unwrap_or_default())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        self.infer(request.messages, request.tools, model, temperature)
            .await
    }
}

impl zeroclaw_api::attribution::Attributable for ClaudeCodeNativeModelProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::ClaudeCodeNative,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zeroclaw_api::tool::ToolSpec;

    fn auth(method: &str, provider: &str) -> serde_json::Value {
        json!({"loggedIn": true, "authMethod": method, "apiProvider": provider})
    }

    #[test]
    fn billing_guard_requires_matching_native_auth_and_rejects_unknown_or_logout() {
        let subscription = ClaudeCodeBillingSource::Subscription;
        assert!(validate_auth(&auth("claude.ai", "firstParty"), subscription, &[]).is_ok());
        assert!(validate_auth(&auth("oauth_token", "firstParty"), subscription, &[]).is_ok());
        for value in [
            auth("api_key", "firstParty"),
            auth("claude.ai", "gateway"),
            auth("unknown", "firstParty"),
            json!({"loggedIn": false}),
            json!({"loggedIn": "true"}),
            json!({"loggedIn": false, "authMethod":"claude.ai", "apiProvider":"firstParty"}),
            json!({"loggedIn": "true", "authMethod":"claude.ai", "apiProvider":"firstParty"}),
            json!({"authMethod":"claude.ai", "apiProvider":"firstParty"}),
            json!([]),
        ] {
            assert!(validate_auth(&value, subscription, &[]).is_err(), "{value}");
        }
        assert!(
            validate_auth(
                &auth("apiKeyHelper", "firstParty"),
                ClaudeCodeBillingSource::Api,
                &[]
            )
            .is_ok()
        );
        assert!(
            validate_auth(
                &auth("bedrock", "bedrock"),
                ClaudeCodeBillingSource::CloudOrGateway,
                &[]
            )
            .is_ok()
        );
        assert!(
            validate_auth(
                &auth("gateway", "gateway"),
                ClaudeCodeBillingSource::CloudOrGateway,
                &[]
            )
            .is_ok()
        );
    }

    #[test]
    fn billing_guard_rejects_conflicting_selectors_without_changing_native_auth() {
        assert!(
            validate_auth(
                &auth("oauth_token", "firstParty"),
                ClaudeCodeBillingSource::Subscription,
                &[("CLAUDE_CODE_OAUTH_TOKEN".into(), true)]
            )
            .is_ok()
        );
        for name in AUTH_SELECTORS {
            assert!(
                validate_auth(
                    &auth("claude.ai", "firstParty"),
                    ClaudeCodeBillingSource::Subscription,
                    &[(name.to_string(), true)]
                )
                .is_err()
            );
        }
        let value = auth("api_key", "firstParty");
        assert!(
            validate_auth(
                &value,
                ClaudeCodeBillingSource::Api,
                &[("ANTHROPIC_API_KEY".into(), true)]
            )
            .is_ok()
        );
        assert!(
            validate_auth(
                &value,
                ClaudeCodeBillingSource::Api,
                &[("ANTHROPIC_BASE_URL".into(), true)]
            )
            .is_err()
        );
    }

    #[test]
    fn version_guard_is_anchored_and_enforces_supported_native_flags() {
        assert!(validate_version(b"2.1.289 (Claude Code)\n").is_ok());
        assert!(validate_version(b"2.2.0 (Claude Code)").is_ok());
        for value in [
            b"2.1.288 (Claude Code)".as_slice(),
            b"9.0.0 other binary",
            b"2.1.289 (Claude Code) secret",
        ] {
            assert!(validate_version(value).is_err());
        }
    }

    #[test]
    fn native_argv_has_no_independent_tools_history_or_permission_bypass() {
        let args = inference_args("sonnet");
        for flag in [
            "--safe-mode",
            "--no-session-persistence",
            "--strict-mcp-config",
        ] {
            assert!(args.iter().any(|a| a == flag));
        }
        assert!(args.windows(2).any(|a| a == ["--tools", ""]));
        assert!(args.windows(2).any(|a| a == ["--max-turns", "1"]));
        assert!(args.windows(2).any(|a| a == ["--disallowedTools", "*"]));
        assert!(
            args.windows(2)
                .any(|a| a == ["--mcp-config", "{\"mcpServers\":{}}"])
        );
        assert!(
            args.windows(2)
                .any(|a| a == ["--settings", "{\"disableAllHooks\":true}"])
        );
        assert!(!args.iter().any(|a| a.contains("skip-permissions")
            || a == "--bare"
            || a == "--resume"
            || a == "--continue"
            || a == "--system-prompt"));
        assert!(inference_args("default").iter().all(|a| a != "--model"));
    }

    #[test]
    fn native_result_requires_success_and_never_relays_raw_errors() {
        let good = json!({"type":"result", "subtype":"success", "is_error":false,
                          "result":"hello", "usage":{"input_tokens":3, "output_tokens":4,
                          "cache_read_input_tokens":5, "cache_creation_input_tokens":6}});
        let result = parse_result(&serde_json::to_vec(&good).unwrap()).unwrap();
        assert_eq!(result.text.as_deref(), Some("hello"));
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(14));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.cached_input_tokens, Some(5));
        assert_eq!(usage.cache_creation_input_tokens, Some(6));
        for bad in [
            json!({"type":"result", "subtype":"success", "is_error":true, "result":"synthetic-secret"}),
            json!({"type":"result", "subtype":"error_max_turns", "is_error":false, "result":"synthetic-secret"}),
            json!({"type":"system", "subtype":"success", "is_error":false, "result":"synthetic-secret"}),
            json!({"result":"synthetic-secret"}),
            json!([]),
            json!({"type":"result", "subtype":"success", "is_error":false, "result":""}),
        ] {
            let error = parse_result(&serde_json::to_vec(&bad).unwrap())
                .unwrap_err()
                .to_string();
            assert!(!error.contains("synthetic-secret"));
        }
    }

    #[test]
    fn complete_history_and_canonical_tool_instructions_reach_stdin_prompt() {
        let messages = [
            ChatMessage::system("system"),
            ChatMessage::user("first"),
            ChatMessage::assistant("prior"),
            ChatMessage::tool("tool result"),
            ChatMessage::user("next"),
        ];
        let tools = [ToolSpec::new(
            "file_read",
            "Read file",
            json!({"type":"object"}),
        )];
        let prompt = conversation_prompt(&messages, Some(&tools)).unwrap();
        for content in [
            "system",
            "first",
            "prior",
            "tool result",
            "next",
            "file_read",
            "<tool_call>",
        ] {
            assert!(prompt.contains(content));
        }
        assert!(
            conversation_prompt(&[ChatMessage::user("x".repeat(MAX_PROMPT_BYTES))], None).is_err()
        );
    }

    #[test]
    fn native_config_canonical_setters_roundtrip_base_and_native_fields() {
        let mut config = zeroclaw_config::schema::Config::default();
        assert!(
            config
                .create_map_key("providers.models.claude_code_native", "personal")
                .unwrap()
        );
        for (field, value) in [
            ("model", "sonnet"),
            ("claude_config_dir", "/operator/native-account"),
            ("expected_billing", "subscription"),
        ] {
            let path = format!("providers.models.claude_code_native.personal.{field}");
            config.set_prop(&path, value).unwrap();
            assert_eq!(config.get_prop(&path).unwrap(), value);
        }
        let entry = config
            .providers
            .models
            .claude_code_native
            .get("personal")
            .unwrap();
        assert_eq!(entry.base.model.as_deref(), Some("sonnet"));
        assert_eq!(
            entry.claude_config_dir.as_deref(),
            Some("/operator/native-account")
        );
        assert_eq!(
            entry.expected_billing,
            Some(ClaudeCodeBillingSource::Subscription)
        );
        let serialized = serde_json::to_value(&config).unwrap();
        let mut reloaded: zeroclaw_config::schema::Config =
            serde_json::from_value(serialized).unwrap();
        assert_eq!(
            reloaded
                .get_prop("providers.models.claude_code_native.personal.model")
                .unwrap(),
            "sonnet"
        );
        reloaded
            .set_prop(
                "providers.models.claude_code_native.personal.expected_billing",
                "api",
            )
            .unwrap();
        assert_eq!(
            reloaded
                .get_prop("providers.models.claude_code_native.personal.expected_billing")
                .unwrap(),
            "api"
        );
        assert!(
            reloaded
                .set_prop(
                    "providers.models.claude_code_native.personal.expected_billing",
                    "unknown"
                )
                .is_err()
        );
    }

    #[cfg(unix)]
    mod process {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn fixture(directory: &std::path::Path, body: &str) -> std::path::PathBuf {
            let path = directory.join("native client with spaces");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        }

        fn provider(
            directory: &std::path::Path,
            binary: std::path::PathBuf,
        ) -> ClaudeCodeNativeModelProvider {
            ClaudeCodeNativeModelProvider {
                alias: "fixture".into(),
                binary,
                directory: directory.to_path_buf(),
                account_directory: None,
                billing: ClaudeCodeBillingSource::Subscription,
                timeout: Duration::from_secs(2),
            }
        }

        #[tokio::test]
        async fn native_account_selection_process_child() {
            let Some(root) = std::env::var_os("ZEROCLAW_NATIVE_SELECTION_FIXTURE") else {
                return;
            };
            let root = PathBuf::from(root);
            let config = ClaudeCodeNativeModelProviderConfig {
                binary_path: Some(
                    root.join("native client with spaces")
                        .to_str()
                        .unwrap()
                        .into(),
                ),
                claude_config_dir: std::env::var("ZEROCLAW_NATIVE_SELECTED_ACCOUNT").ok(),
                expected_billing: Some(ClaudeCodeBillingSource::Subscription),
                ..Default::default()
            };
            let provider = ClaudeCodeNativeModelProvider::from_config(
                "fixture",
                &config,
                Some(&root),
                Some(2),
            )
            .unwrap();
            assert_eq!(
                provider
                    .chat_with_system(None, "fixture prompt", "sonnet", None)
                    .await
                    .unwrap(),
                "done"
            );
        }

        async fn native_account_selection_case(literal: bool) {
            let temp = tempfile::tempdir().unwrap();
            let account = temp.path().join("account");
            let alias = temp.path().join("literal account alias");
            std::fs::create_dir(&account).unwrap();
            std::fs::write(account.join("credentials"), b"synthetic-private-sentinel").unwrap();
            std::os::unix::fs::symlink(&account, &alias).unwrap();
            fixture(
                temp.path(),
                "if [ \"$1\" = '--version' ]; then printf '2.1.289 (Claude Code)'; exit 0; fi\nif [ \"$2\" = 'auth' ]; then if [ \"${CLAUDE_CONFIG_DIR-__NATIVE_DEFAULT__}\" = \"$ZEROCLAW_NATIVE_EXPECTED_ACCOUNT\" ]; then printf '{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"apiProvider\":\"firstParty\"}'; else printf '{\"loggedIn\":false}'; fi; exit 0; fi\nprintf '%s' \"${CLAUDE_CONFIG_DIR-__NATIVE_DEFAULT__}\" > selected-account\n/bin/cat >/dev/null\nprintf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'",
            );
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "claude_code_native::tests::process::native_account_selection_process_child",
                    "--exact",
                ])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", temp.path())
                .env("ZEROCLAW_NATIVE_SELECTION_FIXTURE", temp.path())
                .env("CLAUDE_CONFIG_DIR", &account)
                .kill_on_drop(true);
            let expected = if literal {
                alias.to_str().unwrap()
            } else {
                "__NATIVE_DEFAULT__"
            };
            child.env("ZEROCLAW_NATIVE_EXPECTED_ACCOUNT", expected);
            if literal {
                child.env("ZEROCLAW_NATIVE_SELECTED_ACCOUNT", &alias);
            }
            let output = tokio::time::timeout(Duration::from_secs(10), child.output())
                .await
                .unwrap()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_eq!(
                std::fs::read_to_string(temp.path().join("selected-account")).unwrap(),
                expected
            );
            assert_eq!(
                std::fs::read(account.join("credentials")).unwrap(),
                b"synthetic-private-sentinel"
            );
        }

        #[tokio::test]
        async fn native_account_selection_default_removes_later_ambient_override() {
            native_account_selection_case(false).await;
        }

        #[tokio::test]
        async fn native_account_selection_explicit_preserves_literal_symlink() {
            native_account_selection_case(true).await;
        }

        #[tokio::test]
        async fn process_boundary_sends_literal_stdin_with_no_prompt_in_argv() {
            let temp = tempfile::tempdir().unwrap();
            let binary = fixture(
                temp.path(),
                "printf '%s\\n' \"$@\" > argv; cat > prompt; printf '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'",
            );
            let provider = provider(temp.path(), binary);
            let prompt = "sensitive prompt; $(touch injected)";
            let output = provider
                .run(
                    &inference_args("sonnet"),
                    prompt.as_bytes(),
                    Duration::from_secs(2),
                    MAX_RESULT_BYTES,
                )
                .await
                .unwrap();
            assert_eq!(parse_result(&output).unwrap().text.as_deref(), Some("done"));
            assert_eq!(
                std::fs::read_to_string(temp.path().join("prompt")).unwrap(),
                prompt
            );
            assert!(
                !std::fs::read_to_string(temp.path().join("argv"))
                    .unwrap()
                    .contains("sensitive prompt")
            );
            assert!(
                std::fs::read_to_string(temp.path().join("argv"))
                    .unwrap()
                    .contains("--max-turns\n1\n")
            );
            assert!(!temp.path().join("injected").exists());
        }

        #[tokio::test]
        async fn process_boundary_preserves_account_reference_and_discards_stderr() {
            let temp = tempfile::tempdir().unwrap();
            let account = temp.path().join("native account");
            std::fs::create_dir(&account).unwrap();
            std::fs::write(account.join("credentials"), "synthetic-sentinel").unwrap();
            let binary = fixture(
                temp.path(),
                "printf '%s' \"$CLAUDE_CONFIG_DIR\" > account-ref; cat >/dev/null; printf 'synthetic-private-diagnostic' >&2; printf '{}'",
            );
            let mut provider = provider(temp.path(), binary);
            provider.account_directory = Some(account.clone());
            assert_eq!(
                provider
                    .run(&[], b"", Duration::from_secs(2), 64)
                    .await
                    .unwrap(),
                b"{}"
            );
            assert_eq!(
                std::fs::read_to_string(temp.path().join("account-ref")).unwrap(),
                account.to_str().unwrap()
            );
            assert_eq!(
                std::fs::read_to_string(account.join("credentials")).unwrap(),
                "synthetic-sentinel"
            );
        }

        #[tokio::test]
        async fn process_boundary_rejects_nonzero_timeout_and_live_output_overflow() {
            let temp = tempfile::tempdir().unwrap();
            for (body, limit, deadline, reason) in [
                (
                    "printf synthetic-secret >&2; exit 2",
                    64,
                    Duration::from_secs(1),
                    "failed",
                ),
                ("sleep 10", 64, Duration::from_millis(40), "timed out"),
                (
                    "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
                    32,
                    Duration::from_secs(1),
                    "output limit",
                ),
            ] {
                let binary = fixture(temp.path(), body);
                let provider = provider(temp.path(), binary);
                let error = provider
                    .run(&[], b"", deadline, limit)
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(reason), "{error}");
                assert!(!error.contains("synthetic-secret"));
            }
        }

        #[tokio::test]
        async fn cancellation_terminates_native_process_group() {
            let temp = tempfile::tempdir().unwrap();
            let binary = fixture(
                temp.path(),
                "(sleep 0.3; echo escaped > escaped) & echo $! > child-pid; wait",
            );
            let provider = provider(temp.path(), binary);
            let mut run = Box::pin(provider.run(&[], b"", Duration::from_secs(10), 64));
            let started = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    result = &mut run => panic!("native process ended before cancellation: {result:?}"),
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {
                        if temp.path().join("child-pid").exists() { break; }
                        assert!(started.elapsed() < Duration::from_secs(2), "fixture did not start");
                    }
                }
            }
            drop(run);
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(
                !temp.path().join("escaped").exists(),
                "native descendant survived cancellation"
            );
        }

        #[test]
        fn provider_requires_explicit_billing_existing_config_root_and_absolute_account() {
            let temp = tempfile::tempdir().unwrap();
            let binary = fixture(temp.path(), "exit 0");
            let mut config = ClaudeCodeNativeModelProviderConfig {
                binary_path: Some(binary.to_str().unwrap().to_string()),
                ..Default::default()
            };
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(temp.path()),
                    Some(2)
                )
                .is_err()
            );
            config.expected_billing = Some(ClaudeCodeBillingSource::Subscription);
            assert!(
                ClaudeCodeNativeModelProvider::from_config("test", &config, None, Some(2)).is_err()
            );
            config.claude_config_dir = Some("relative".into());
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(temp.path()),
                    Some(2)
                )
                .is_err()
            );
            config.claude_config_dir = None;
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(temp.path()),
                    Some(2)
                )
                .is_ok()
            );
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(Path::new(".")),
                    Some(2)
                )
                .is_err()
            );
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(&temp.path().join("missing")),
                    Some(2)
                )
                .is_err()
            );
            config.claude_config_dir =
                Some(temp.path().join("missing account").to_str().unwrap().into());
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(temp.path()),
                    Some(2)
                )
                .is_err()
            );
            config.claude_config_dir = Some(".".into());
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(temp.path()),
                    Some(2)
                )
                .is_err()
            );
            config.claude_config_dir = Some(binary.to_str().unwrap().into());
            assert!(
                ClaudeCodeNativeModelProvider::from_config(
                    "test",
                    &config,
                    Some(temp.path()),
                    Some(2)
                )
                .is_err()
            );
            config.claude_config_dir = None;
            assert!(
                ClaudeCodeNativeModelProvider::from_config("test", &config, Some(&binary), Some(2))
                    .is_err()
            );
        }

        #[test]
        fn native_binary_is_an_existing_executable_without_relative_path_authority() {
            use crate::test_util::{EnvGuard, env_lock};
            let _guard = env_lock();
            let temp = tempfile::tempdir().unwrap();
            let binary = fixture(temp.path(), "exit 0");
            assert_eq!(
                resolve_binary(binary.to_str()).unwrap(),
                binary.canonicalize().unwrap()
            );
            assert!(resolve_binary(Some("relative/path/to/claude")).is_err());
            assert!(resolve_binary(temp.path().to_str()).is_err());
            assert!(resolve_binary(temp.path().join("missing binary").to_str()).is_err());
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(resolve_binary(binary.to_str()).is_err());
            let nested = temp.path().join("relative");
            std::fs::create_dir(&nested).unwrap();
            let nested_binary = fixture(&nested, "exit 0");
            let _path = EnvGuard::set("PATH", temp.path().to_str());
            assert!(resolve_binary(Some("relative/native client with spaces")).is_err());
            std::fs::copy(nested_binary, temp.path().join("claude")).unwrap();
            let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
            let mut relative_path = PathBuf::new();
            for _ in cwd.components().skip(1) {
                relative_path.push("..");
            }
            relative_path.push(
                temp.path()
                    .canonicalize()
                    .unwrap()
                    .strip_prefix("/")
                    .unwrap(),
            );
            let _relative_path = EnvGuard::set("PATH", relative_path.to_str());
            assert!(resolve_binary(None).is_err());
        }

        #[test]
        fn native_profile_factory_has_truthful_identity_and_preserves_legacy_http_alias() {
            use crate::factory::FamilyProviderFactory;
            let temp = tempfile::tempdir().unwrap();
            let binary = fixture(temp.path(), "exit 0");
            let mut config = zeroclaw_config::schema::Config {
                config_path: temp.path().join("config.toml"),
                ..Default::default()
            };
            config.providers.models.claude_code_native.insert(
                "fixture".into(),
                ClaudeCodeNativeModelProviderConfig {
                    binary_path: Some(binary.to_str().unwrap().into()),
                    expected_billing: Some(ClaudeCodeBillingSource::Subscription),
                    ..Default::default()
                },
            );
            let options = crate::model_provider_runtime_options_from_model_provider_entry(
                &config,
                config
                    .providers
                    .models
                    .find("claude_code_native", "fixture"),
            );
            let provider = crate::create_model_provider_for_alias(
                &config,
                "claude_code_native",
                "fixture",
                None,
                &options,
            )
            .unwrap();
            assert!(!provider.supports_native_tools());
            assert!(!provider.supports_vision());
            assert!(!provider.has_stable_request_identity("sonnet"));
            assert!(format!("{:?}", provider.role()).contains("ClaudeCodeNative"));
            let mut entry = config
                .providers
                .models
                .claude_code_native
                .get("fixture")
                .unwrap()
                .clone();
            entry.base.api_key = Some("synthetic-key".into());
            assert!(
                entry
                    .create_provider("fixture", None, None, &options)
                    .is_err()
            );
            entry.base.api_key = None;
            entry.base.uri = Some("https://invalid.example".into());
            assert!(
                entry
                    .create_provider("fixture", None, None, &options)
                    .is_err()
            );
            assert!(
                crate::create_model_provider_for_alias(
                    &config,
                    "claude_code_native",
                    "fixture",
                    Some("synthetic-key"),
                    &options
                )
                .is_err()
            );
            let mut options = options;
            options.provider_api_url = Some("https://invalid.example".into());
            assert!(
                crate::create_model_provider_for_alias(
                    &config,
                    "claude_code_native",
                    "fixture",
                    None,
                    &options
                )
                .is_err()
            );
            let legacy = crate::create_model_provider("claude-code", None).unwrap();
            assert!(format!("{:?}", legacy.role()).contains("Anthropic"));
            assert!(!format!("{:?}", legacy.role()).contains("ClaudeCodeNative"));
        }

        #[test]
        fn genuine_provider_boundary_replays_tool_history_and_rechecks_auth_each_turn() {
            use crate::test_util::{EnvGuard, env_lock};
            let _guard = env_lock();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _selectors = AUTH_SELECTORS
                .iter()
                .map(|name| EnvGuard::set(name, None))
                .collect::<Vec<_>>();
            let temp = tempfile::tempdir().unwrap();
            let binary = fixture(
                temp.path(),
                r#"
if [ "$1" = --version ]; then printf '2.1.289 (Claude Code)'; exit 0; fi
if [ "$2" = auth ]; then
  echo checked >> auth-checks
  if [ -f wrong-billing ]; then printf '{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}';
  else printf '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'; fi
  exit 0
fi
echo inference >> inferences
cat > full-prompt
printf '%s' '{"type":"result","subtype":"success","is_error":false,"result":"<tool_call>\n{\"name\":\"file_read\",\"arguments\":{}}\n</tool_call>"}'
"#,
            );
            let provider = provider(temp.path(), binary);
            let messages = [
                ChatMessage::user("read fixture"),
                ChatMessage::assistant("prior"),
                ChatMessage::tool("canonical tool result"),
            ];
            let tools = [ToolSpec::new("file_read", "Read", json!({"type":"object"}))];
            let result = runtime
                .block_on(provider.chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                    },
                    "sonnet",
                    None,
                ))
                .unwrap();
            assert!(result.text.unwrap().contains("<tool_call>"));
            let prompt = std::fs::read_to_string(temp.path().join("full-prompt")).unwrap();
            assert!(
                prompt.contains("prior")
                    && prompt.contains("canonical tool result")
                    && prompt.contains("Tool Use Protocol")
            );
            std::fs::write(temp.path().join("wrong-billing"), "yes").unwrap();
            assert!(
                runtime
                    .block_on(provider.simple_chat("another turn", "sonnet", None))
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(temp.path().join("auth-checks"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
            assert_eq!(
                std::fs::read_to_string(temp.path().join("inferences"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        }
    }
}
