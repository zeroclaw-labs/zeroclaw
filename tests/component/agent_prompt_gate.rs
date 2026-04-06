//! Agent system prompt size and structure gates (compact context, edge hardware).
//!
//! These tests enforce upper bounds on assembled prompts so small-context models and
//! low-memory hosts are not starved by prompt bloat. Bump constants only when
//! intentionally expanding prompt text (document in PR).

use std::path::Path;

use tempfile::tempdir;
use zeroclaw::channels::build_system_prompt_with_mode_and_autonomy;
use zeroclaw::config::{AutonomyConfig, SkillsPromptInjectionMode};

/// Empty tools + compact_context + empty workspace: baseline assembly size (UTF-8 bytes).
/// Intentionally generous; tighten if the prompt is put on a diet.
const AGENT_PROMPT_BASELINE_MAX_BYTES: usize = 96 * 1024;

/// Channel prompt `## Runtime` subsection (includes hostname; keep bounded).
const CHANNEL_RUNTIME_BLOCK_MAX_BYTES: usize = 4096;

/// Static `## Hardware Access` block in `channels::build_system_prompt_with_mode_and_autonomy`.
const HARDWARE_PROMPT_BLOCK_MAX_BYTES: usize = 4096;

fn build_compact_channel_prompt(workspace: &Path, tools: &[(&str, &str)]) -> String {
    build_system_prompt_with_mode_and_autonomy(
        workspace,
        "test-model",
        tools,
        &[],
        None,
        Some(0),
        Some(&AutonomyConfig::default()),
        false,
        SkillsPromptInjectionMode::default(),
        true,
        0,
    )
}

fn extract_channel_runtime_block(prompt: &str) -> &str {
    const START: &str = "## Runtime\n\n";
    let i = prompt
        .find(START)
        .expect("channel system prompt must contain ## Runtime section");
    let tail = &prompt[i..];
    let after_start = &tail[START.len()..];
    let end_rel = after_start.find("\n\n## ").unwrap_or(after_start.len());
    &tail[..START.len() + end_rel]
}

fn extract_hardware_access_block(prompt: &str) -> Option<&str> {
    const START: &str = "## Hardware Access\n\n";
    let i = prompt.find(START)?;
    let tail = &prompt[i..];
    let after_start = &tail[START.len()..];
    let end_rel = after_start.find("\n\n## ").unwrap_or(after_start.len());
    Some(&tail[..START.len() + end_rel])
}

#[test]
fn agent_prompt_baseline_compact_under_ceiling() {
    let dir = tempdir().expect("tempdir");
    let prompt = build_compact_channel_prompt(dir.path(), &[]);
    let n = prompt.len();
    assert!(
        n <= AGENT_PROMPT_BASELINE_MAX_BYTES,
        "compact baseline system prompt too large: {n} bytes (max {AGENT_PROMPT_BASELINE_MAX_BYTES}); shrink static prompt text or bump ceiling intentionally"
    );
}

#[test]
fn agent_prompt_channel_runtime_block_concise() {
    let dir = tempdir().expect("tempdir");
    let prompt = build_compact_channel_prompt(dir.path(), &[]);
    let block = extract_channel_runtime_block(&prompt);
    assert!(
        block.contains("Host:") && block.contains("OS:") && block.contains("Model:"),
        "channel runtime block must include host/OS/model: {block:?}"
    );
    let n = block.len();
    assert!(
        n <= CHANNEL_RUNTIME_BLOCK_MAX_BYTES,
        "channel ## Runtime block too large: {n} bytes (max {CHANNEL_RUNTIME_BLOCK_MAX_BYTES})"
    );
}

#[test]
fn agent_prompt_hardware_block_only_when_expected() {
    let dir = tempdir().expect("tempdir");
    let no_hw = build_compact_channel_prompt(dir.path(), &[]);
    assert!(
        !no_hw.contains("## Hardware Access"),
        "non-hardware tools must not inject hardware section"
    );

    let tools = &[("gpio_read", "test")];
    let with_hw = build_compact_channel_prompt(dir.path(), tools);
    assert!(
        with_hw.contains("## Hardware Access"),
        "hardware tools must inject hardware section"
    );
    let block = extract_hardware_access_block(&with_hw).expect("hardware block");
    let n = block.len();
    assert!(
        n <= HARDWARE_PROMPT_BLOCK_MAX_BYTES,
        "hardware prompt block too large: {n} bytes (max {HARDWARE_PROMPT_BLOCK_MAX_BYTES})"
    );
}
