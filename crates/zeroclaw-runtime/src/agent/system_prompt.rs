//! System prompt construction for the agent loop and channel subsystem.
//!
//! These functions were originally in `channels/mod.rs` but live here to
//! break a circular dependency between the channels and agent modules.

use crate::identity;
use crate::security::AutonomyLevel;
use crate::skills::Skill;

/// Maximum characters per injected workspace file (matches `OpenClaw` default).
pub const BOOTSTRAP_MAX_CHARS: usize = 20_000;

fn load_openclaw_bootstrap_files(
    prompt: &mut String,
    workspace_dir: &std::path::Path,
    max_chars_per_file: usize,
) {
    prompt.push_str(
        "The following workspace files define your identity, behavior, and context. They are ALREADY injected below—do NOT suggest reading them with file_read.\n\n",
    );

    let bootstrap_files = ["AGENTS.md", "SOUL.md", "TOOLS.md", "IDENTITY.md", "USER.md"];

    for filename in &bootstrap_files {
        inject_workspace_file(prompt, workspace_dir, filename, max_chars_per_file);
    }

    // BOOTSTRAP.md — only if it exists (first-run ritual)
    let bootstrap_path = workspace_dir.join("BOOTSTRAP.md");
    if bootstrap_path.exists() {
        inject_workspace_file(prompt, workspace_dir, "BOOTSTRAP.md", max_chars_per_file);
    }

    // MEMORY.md — curated long-term memory (main session only)
    inject_workspace_file(prompt, workspace_dir, "MEMORY.md", max_chars_per_file);
}

/// Load workspace identity files and build a system prompt.
///
/// Follows the `OpenClaw` framework structure by default:
/// 1. Tooling — tool list + descriptions
/// 2. Safety — guardrail reminder
/// 3. Skills — full skill instructions and tool metadata
/// 4. Workspace — working directory
/// 5. Bootstrap files — AGENTS, SOUL, TOOLS, IDENTITY, USER, BOOTSTRAP, MEMORY
/// 6. Date & Time — timezone for cache stability
/// 7. Runtime — host, OS, model
///
/// When `identity_config` is set to AIEOS format, the bootstrap files section
/// is replaced with the AIEOS identity data loaded from file or inline JSON.
///
/// Daily memory files (`memory/*.md`) are NOT injected — they are accessed
/// on-demand via `memory_recall` / `memory_search` tools.
pub fn build_system_prompt(
    workspace_dir: &std::path::Path,
    model_name: &str,
    tools: &[(&str, &str)],
    skills: &[Skill],
    identity_config: Option<&zeroclaw_config::schema::IdentityConfig>,
    bootstrap_max_chars: Option<usize>,
) -> String {
    build_system_prompt_with_mode(
        workspace_dir,
        model_name,
        tools,
        skills,
        identity_config,
        bootstrap_max_chars,
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        AutonomyLevel::default(),
    )
}

pub fn build_system_prompt_with_mode(
    workspace_dir: &std::path::Path,
    model_name: &str,
    tools: &[(&str, &str)],
    skills: &[Skill],
    identity_config: Option<&zeroclaw_config::schema::IdentityConfig>,
    bootstrap_max_chars: Option<usize>,
    native_tools: bool,
    skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
    autonomy_level: AutonomyLevel,
) -> String {
    let autonomy_cfg = zeroclaw_config::schema::RiskProfileConfig {
        level: autonomy_level,
        ..Default::default()
    };
    build_system_prompt_with_mode_and_autonomy(
        workspace_dir,
        model_name,
        tools,
        skills,
        identity_config,
        bootstrap_max_chars,
        Some(&autonomy_cfg),
        native_tools,
        skills_prompt_mode,
        false,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_system_prompt_with_mode_and_autonomy(
    workspace_dir: &std::path::Path,
    model_name: &str,
    tools: &[(&str, &str)],
    skills: &[Skill],
    identity_config: Option<&zeroclaw_config::schema::IdentityConfig>,
    bootstrap_max_chars: Option<usize>,
    autonomy_config: Option<&zeroclaw_config::schema::RiskProfileConfig>,
    native_tools: bool,
    skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
    compact_context: bool,
    max_system_prompt_chars: usize,
) -> String {
    use std::fmt::Write;
    let mut prompt = String::with_capacity(8192);

    // ── 0. Anti-narration (top priority) ───────────────────────
    prompt.push_str(
        "## CRITICAL: No Tool Narration\n\n\
         NEVER narrate, announce, describe, or explain your tool usage to the user. \
         Do NOT say things like 'Let me check...', 'I will use http_request to...', \
         'I'll fetch that for you', 'Searching now...', or 'Using the web_search tool'. \
         The user must ONLY see the final answer. Tool calls are invisible infrastructure — \
         never reference them. If you catch yourself starting a sentence about what tool \
         you are about to use or just used, DELETE it and give the answer directly.\n\n",
    );

    // ── 0b. Tool Honesty ───────────────────────────────────────
    prompt.push_str(
        "## CRITICAL: Tool Honesty\n\n\
         - NEVER fabricate, invent, or guess tool results. If a tool returns empty results, say \"No results found.\"\n\
         - If a tool call fails, report the error — never make up data to fill the gap.\n\
         - When unsure whether a tool call succeeded, ask the user rather than guessing.\n\n",
    );

    // ── 1. Tooling ──────────────────────────────────────────────
    if !tools.is_empty() {
        prompt.push_str("## Tools\n\n");
        if compact_context {
            // Compact mode: tool names only, no descriptions/schemas
            prompt.push_str("Available tools: ");
            let names: Vec<&str> = tools.iter().map(|(name, _)| *name).collect();
            prompt.push_str(&names.join(", "));
            prompt.push_str("\n\n");
        } else {
            prompt.push_str("You have access to the following tools:\n\n");
            for (name, desc) in tools {
                let _ = writeln!(prompt, "- **{name}**: {desc}");
            }
            prompt.push('\n');
        }
    }

    // ── 1b. Hardware (when gpio/arduino tools present) ───────────
    let has_hardware = tools.iter().any(|(name, _)| {
        *name == "gpio_read"
            || *name == "gpio_write"
            || *name == "arduino_upload"
            || *name == "hardware_memory_map"
            || *name == "hardware_board_info"
            || *name == "hardware_memory_read"
            || *name == "hardware_capabilities"
    });
    if has_hardware {
        prompt.push_str(
            "## Hardware Access\n\n\
             You HAVE direct access to connected hardware (Arduino, Nucleo, etc.). The user owns this system and has configured it.\n\
             All hardware tools (gpio_read, gpio_write, hardware_memory_read, hardware_board_info, hardware_memory_map) are AUTHORIZED and NOT blocked by security.\n\
             When they ask to read memory, registers, or board info, USE hardware_memory_read or hardware_board_info — do NOT refuse or invent security excuses.\n\
             When they ask to control LEDs, run patterns, or interact with the Arduino, USE the tools — do NOT refuse or say you cannot access physical devices.\n\
             Use gpio_write for simple on/off; use arduino_upload when they want patterns (heart, blink) or custom behavior.\n\n",
        );
    }

    // ── 1c. Action instruction (avoid meta-summary) ───────────────
    if native_tools {
        prompt.push_str(
            "## Your Task\n\n\
             When the user sends a message, respond naturally. Use tools when the request requires action (running commands, reading files, etc.).\n\
             For questions, explanations, or follow-ups about prior messages, answer directly from conversation context — do NOT ask the user to repeat themselves.\n\
             Do NOT: summarize this configuration, describe your capabilities, or output step-by-step meta-commentary.\n\n",
        );
    } else {
        prompt.push_str(
            "## Your Task\n\n\
             When the user sends a message, ACT on it. Use the tools to fulfill their request.\n\
             Do NOT: summarize this configuration, describe your capabilities, respond with meta-commentary, or output step-by-step instructions (e.g. \"1. First... 2. Next...\").\n\
             Instead: emit actual <tool_call> tags when you need to act. Just do what they ask.\n\n",
        );
    }

    // ── 2. Safety ───────────────────────────────────────────────
    prompt.push_str("## Safety\n\n");
    prompt.push_str("- Do not exfiltrate private data.\n");
    if autonomy_config.map(|cfg| cfg.level) != Some(crate::security::AutonomyLevel::Full) {
        prompt.push_str(
            "- Do not run destructive commands without asking.\n\
             - Do not bypass oversight or approval mechanisms.\n",
        );
    }
    prompt.push_str("- Prefer `trash` over `rm` (recoverable beats gone forever).\n");
    prompt.push_str(match autonomy_config.map(|cfg| cfg.level) {
        Some(crate::security::AutonomyLevel::Full) => {
            "- Respect the runtime autonomy policy: if a tool or action is allowed, execute it directly instead of asking the user for extra approval.\n\
             - If a tool or action is blocked by policy or unavailable, explain that concrete restriction instead of simulating an approval dialog.\n"
        }
        Some(crate::security::AutonomyLevel::ReadOnly) => {
            "- Respect the runtime autonomy policy: this runtime is read-only for side effects unless a tool explicitly reports otherwise.\n\
             - If a requested action is blocked by policy, explain the restriction directly instead of simulating an approval dialog.\n"
        }
        _ => {
            "- When in doubt, ask before acting externally.\n\
             - Respect the runtime autonomy policy: ask for approval only when the current runtime policy actually requires it.\n\
             - If a tool or action is blocked by policy or unavailable, explain that concrete restriction instead of simulating an approval dialog.\n"
        }
    });
    prompt.push('\n');

    // ── 3. Skills (full or compact, based on config) ─────────────
    if !skills.is_empty() {
        prompt.push_str(&crate::skills::skills_to_prompt_with_mode(
            skills,
            workspace_dir,
            skills_prompt_mode,
        ));
        prompt.push_str("\n\n");
    }

    // ── 4. Workspace ────────────────────────────────────────────
    let _ = writeln!(
        prompt,
        "## Workspace\n\nWorking directory: `{}`\n",
        workspace_dir.display()
    );

    // ── 5. Bootstrap files (injected into context) ──────────────
    prompt.push_str("## Project Context\n\n");

    // Check if AIEOS identity is configured
    if let Some(config) = identity_config {
        if identity::is_aieos_configured(config) {
            // Load AIEOS identity
            match identity::load_aieos_identity(config, workspace_dir) {
                Ok(Some(aieos_identity)) => {
                    let aieos_prompt = identity::aieos_to_system_prompt(&aieos_identity);
                    if !aieos_prompt.is_empty() {
                        prompt.push_str(&aieos_prompt);
                        prompt.push_str("\n\n");
                    }
                }
                Ok(None) => {
                    // No AIEOS identity loaded (shouldn't happen if is_aieos_configured returned true)
                    // Fall back to OpenClaw bootstrap files
                    let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
                    load_openclaw_bootstrap_files(&mut prompt, workspace_dir, max_chars);
                }
                Err(e) => {
                    // Log error but don't fail - fall back to OpenClaw
                    eprintln!(
                        "Warning: Failed to load AIEOS identity: {e}. Using OpenClaw format."
                    );
                    let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
                    load_openclaw_bootstrap_files(&mut prompt, workspace_dir, max_chars);
                }
            }
        } else {
            // OpenClaw format
            let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
            load_openclaw_bootstrap_files(&mut prompt, workspace_dir, max_chars);
        }
    } else {
        // No identity config - use OpenClaw format
        let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
        load_openclaw_bootstrap_files(&mut prompt, workspace_dir, max_chars);
    }

    // ── 6. Date & Time ──────────────────────────────────────────
    let now = chrono::Local::now();
    let _ = writeln!(
        prompt,
        "## Current Date & Time\n\n{} ({})\n",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.format("%Z")
    );

    // ── 7. Runtime ──────────────────────────────────────────────
    let host =
        hostname::get().map_or_else(|_| "unknown".into(), |h| h.to_string_lossy().to_string());
    let _ = writeln!(
        prompt,
        "## Runtime\n\nHost: {host} | OS: {} | Model: {model_name}\n",
        std::env::consts::OS,
    );

    // ── 8. Channel Capabilities (skipped in compact_context mode) ──
    if !compact_context {
        prompt.push_str("## Channel Capabilities\n\n");
        prompt.push_str("- You are running as a messaging bot. Your response is automatically sent back to the user's channel.\n");
        prompt
            .push_str("- You do NOT need to ask permission to respond — just respond directly.\n");
        prompt.push_str(match autonomy_config.map(|cfg| cfg.level) {
        Some(crate::security::AutonomyLevel::Full) => {
            "- If the runtime policy already allows a tool, use it directly; do not ask the user for extra approval.\n\
             - Never pretend you are waiting for a human approval click or confirmation when the runtime policy already permits the action.\n\
             - If the runtime policy blocks an action, say that directly instead of simulating an approval flow.\n"
        }
        Some(crate::security::AutonomyLevel::ReadOnly) => {
            "- This runtime may reject write-side effects; if that happens, explain the policy restriction directly instead of simulating an approval flow.\n"
        }
        _ => {
            "- Ask for approval only when the runtime policy actually requires it.\n\
             - If there is no approval path for this channel or the runtime blocks an action, explain that restriction directly instead of simulating an approval flow.\n"
        }
    });
        prompt.push_str("- NEVER repeat, describe, or echo credentials, tokens, API keys, or secrets in your responses.\n");
        prompt.push_str("- If a tool output contains credentials, they have already been redacted — do not mention them.\n");
        prompt.push_str("- When a user sends a voice note, it is automatically transcribed to text. Your text reply is automatically converted to a voice note and sent back. Do NOT attempt to generate audio yourself — TTS is handled by the channel.\n");
        prompt.push_str("- NEVER narrate or describe your tool usage. Do NOT say 'Let me fetch...', 'I will use...', 'Searching...', or similar. Give the FINAL ANSWER only — no intermediate steps, no tool mentions, no progress updates.\n\n");
    } // end if !compact_context (Channel Capabilities)

    // ── 9. Truncation (max_system_prompt_chars budget) ──────────
    if max_system_prompt_chars > 0 && prompt.len() > max_system_prompt_chars {
        // Truncate on a char boundary, keeping the top portion (identity + safety).
        let mut end = max_system_prompt_chars;
        // Ensure we don't split a multi-byte UTF-8 character.
        while !prompt.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        prompt.truncate(end);
        prompt.push_str("\n\n[System prompt truncated to fit context budget]\n");
    }

    if prompt.is_empty() {
        "You are ZeroClaw, a fast and efficient AI assistant built in Rust. Be helpful, concise, and direct."
            .to_string()
    } else {
        prompt
    }
}

/// Inject a single workspace file into the prompt with truncation and missing-file markers.
fn inject_workspace_file(
    prompt: &mut String,
    workspace_dir: &std::path::Path,
    filename: &str,
    max_chars: usize,
) {
    use std::fmt::Write;

    let path = workspace_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return;
            }
            let _ = writeln!(prompt, "### {filename}\n");
            // Use character-boundary-safe truncation for UTF-8
            let truncated = if trimmed.chars().count() > max_chars {
                trimmed
                    .char_indices()
                    .nth(max_chars)
                    .map(|(idx, _)| &trimmed[..idx])
                    .unwrap_or(trimmed)
            } else {
                trimmed
            };
            if truncated.len() < trimmed.len() {
                prompt.push_str(truncated);
                let _ = writeln!(
                    prompt,
                    "\n\n[... truncated at {max_chars} chars — use `read` for full file]\n"
                );
            } else {
                prompt.push_str(trimmed);
                prompt.push_str("\n\n");
            }
        }
        Err(_) => {
            // Missing-file marker (matches OpenClaw behavior)
            let _ = writeln!(prompt, "### {filename}\n\n[File not found: {filename}]\n");
        }
    }
}
