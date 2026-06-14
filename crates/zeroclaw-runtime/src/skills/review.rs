// Background skill review fork — post-turn hook that wakes a forked agent
// loop in a restricted toolset to decide whether the just-finished
// conversation should change the installed skill library.
//
// Inspired by hermes-agent's `_spawn_background_review` pattern (see
// nousresearch/hermes-agent at run_agent.py). ZeroClaw differs in that the
// fork runs inline (no background thread — Rust async lets us await it on
// the same task), targets the agentskills.io `SKILL.md` format directly,
// and writes through dedicated `skill_manage`/`skill_view` tools.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::task_local;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig, SkillImprovementConfig};
use zeroclaw_providers::{ChatMessage, ModelProvider};

use crate::observability::Observer;
use crate::tools::skill_manage::{SkillManageTool, SkillViewTool, SkillsListTool};

const REVIEW_PROMPT: &str = include_str!("review_prompt.md");

task_local! {
    /// Set inside the review fork so a *nested* `maybe_run_skill_review` call
    /// returns immediately. Without this, a review fork that itself triggers
    /// the post-turn hook would recurse and burn LLM tokens until it timed out.
    static SKILL_REVIEW_ACTIVE: ();
}

/// Decide whether to fire the review fork, and run it if so.
///
/// Gating (in this order):
/// 1. `config.enabled == true`
/// 2. Not already inside a review (recursion guard)
/// 3. History accumulated at least `nudge_interval_iterations` tool-result messages
///
/// The "failed_slugs" list is passed as a *hint* in the review prompt so the
/// fork can spend its budget where the user just got hurt — but failure is NOT
/// the trigger condition. Routine improvements (user corrections, novel
/// techniques) happen on successful sessions too.
#[allow(clippy::too_many_arguments)]
pub async fn maybe_run_skill_review(
    workspace_dir: PathBuf,
    config: SkillImprovementConfig,
    allow_scripts: bool,
    history: Vec<ChatMessage>,
    failed_slugs: Vec<String>,
    provider: &dyn ModelProvider,
    provider_name: &str,
    model_name: &str,
    observer: &dyn Observer,
    multimodal: &MultimodalConfig,
    pacing: &PacingConfig,
    max_tool_result_chars: usize,
    max_context_tokens: usize,
    cancellation_token: Option<&CancellationToken>,
) {
    if !config.enabled {
        return;
    }
    if SKILL_REVIEW_ACTIVE.try_with(|()| ()).is_ok() {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Skill review: recursion guard tripped, skipping nested review"
        );
        return;
    }
    if !should_trigger(&history, config.nudge_interval_iterations) {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "iters": count_tool_iterations(&history),
                    "threshold": config.nudge_interval_iterations,
                })
            ),
            "Skill review: iteration budget not reached, skipping"
        );
        return;
    }

    let tools: Vec<Box<dyn Tool>> =
        build_review_tools(workspace_dir.clone(), config.clone(), allow_scripts);
    let review_input = build_review_input(&failed_slugs);

    let mut review_history = history;
    review_history.push(ChatMessage::user(&review_input));

    let receipts: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let result = SKILL_REVIEW_ACTIVE
        .scope((), async {
            crate::agent::loop_::run_tool_call_loop(
                provider,
                &mut review_history,
                &tools,
                observer,
                provider_name,
                model_name,
                Some(0.3),      // temperature — low so the fork doesn't ramble
                true,           // silent
                None,           // approval: no human in the loop here
                "skill_review", // channel_name
                None,           // channel_reply_target
                multimodal,
                config.max_review_iterations as usize,
                cancellation_token.cloned(), // cancellation_token
                None,                        // on_delta
                None,                        // hooks
                &[],                         // excluded_tools
                &[],                         // dedup_exempt_tools
                None,                        // activated_tools
                None,                        // model_switch_callback
                pacing,
                false, // strict_tool_parsing — lenient for the restricted fork
                false, // parallel_tools — sequential for the mutation-capable fork
                max_tool_result_chars,
                max_context_tokens,
                None, // shared_budget
                None, // channel
                None, // receipt_generator
                Some(&receipts),
            )
            .await
        })
        .await;

    match result {
        Ok(final_text) => {
            let summary = summarize_actions(&receipts, &final_text);
            if !summary.is_empty() {
                println!(
                    "{}",
                    crate::i18n::get_required_cli_string_with_args(
                        "cli-skills-review-summary",
                        &[("summary", &summary)],
                    )
                );
            }
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                "Skill review fork failed"
            );
        }
    }
}

fn build_review_tools(
    workspace_dir: PathBuf,
    config: SkillImprovementConfig,
    allow_scripts: bool,
) -> Vec<Box<dyn Tool>> {
    let wd = Arc::new(workspace_dir);
    vec![
        Box::new(SkillsListTool::new((*wd).clone())),
        Box::new(SkillViewTool::new((*wd).clone())),
        Box::new(SkillManageTool::new((*wd).clone(), config, allow_scripts)),
    ]
}

fn build_review_input(failed_slugs: &[String]) -> String {
    let hint = if failed_slugs.is_empty() {
        "Hint: no skill executions failed this session.".to_string()
    } else {
        format!(
            "Hint: these skills FAILED this session — investigate before patching: {}",
            failed_slugs.join(", ")
        )
    };
    format!("{REVIEW_PROMPT}\n\n---\n\n{hint}\n")
}

/// `nudge_interval_iterations == 0` disables iteration-based triggering.
/// Otherwise, count `role == "tool"` messages in history — each represents a
/// completed tool call. Fire if we've crossed the threshold.
fn should_trigger(history: &[ChatMessage], threshold: u32) -> bool {
    if threshold == 0 {
        return false;
    }
    count_tool_iterations(history) >= threshold as usize
}

fn count_tool_iterations(history: &[ChatMessage]) -> usize {
    history.iter().filter(|m| m.role == "tool").count()
}

/// Convert the review's tool receipts + final text into a one-line summary
/// for the user. Returns "" if the fork did nothing notable.
fn summarize_actions(receipts: &Mutex<Vec<String>>, final_text: &str) -> String {
    let receipts = receipts.lock().ok();
    let actions: Vec<String> = receipts
        .as_ref()
        .map(|v| v.iter().filter_map(|r| extract_action_summary(r)).collect())
        .unwrap_or_default();

    if !actions.is_empty() {
        return actions.join(" · ");
    }

    let trimmed = final_text.trim();
    if trimmed.eq_ignore_ascii_case("nothing to save.") || trimmed.is_empty() {
        return String::new();
    }
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        String::new()
    } else {
        // Use floor_char_boundary to avoid panicking on multi-byte chars.
        let max = 80.min(first_line.len());
        let end = first_line.floor_char_boundary(max);
        first_line[..end].to_string()
    }
}

fn extract_action_summary(receipt: &str) -> Option<String> {
    let lower = receipt.to_lowercase();
    let verb = if lower.contains("patched skill") {
        "patched"
    } else if lower.contains("wrote") && lower.contains("for skill") {
        "wrote file for"
    } else if lower.contains("archived skill") {
        "archived"
    } else {
        return None;
    };
    let slug = extract_quoted_slug(receipt)?;
    Some(format!("{verb} {slug}"))
}

fn extract_quoted_slug(s: &str) -> Option<String> {
    let start = s.find('\'')?;
    let rest = &s[start + 1..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
        }
    }

    #[test]
    fn count_tool_iterations_counts_only_tool_role() {
        let history = vec![
            msg("system", "..."),
            msg("user", "go"),
            msg("assistant", "..."),
            msg("tool", "ok"),
            msg("assistant", "..."),
            msg("tool", "ok"),
        ];
        assert_eq!(count_tool_iterations(&history), 2);
    }

    #[test]
    fn should_trigger_zero_threshold_disables() {
        let history = vec![msg("tool", "ok"); 10];
        assert!(!should_trigger(&history, 0));
    }

    #[test]
    fn should_trigger_fires_at_threshold() {
        let history = vec![msg("tool", "ok"); 10];
        assert!(should_trigger(&history, 10));
    }

    #[test]
    fn should_trigger_holds_below_threshold() {
        let history = vec![msg("tool", "ok"); 9];
        assert!(!should_trigger(&history, 10));
    }

    #[test]
    fn build_review_input_includes_failure_hint_when_present() {
        let input = build_review_input(&["deploy".to_string(), "test-runner".to_string()]);
        assert!(input.contains("FAILED"));
        assert!(input.contains("deploy"));
        assert!(input.contains("test-runner"));
    }

    #[test]
    fn build_review_input_says_no_failures_when_empty() {
        let input = build_review_input(&[]);
        assert!(input.to_lowercase().contains("no skill executions failed"));
    }

    #[test]
    fn build_review_input_references_skill_md() {
        let input = build_review_input(&[]);
        assert!(input.contains("SKILL.md"));
        assert!(!input.contains("SKILL.toml"));
    }

    #[test]
    fn summarize_actions_picks_up_patch_receipts() {
        let receipts = Mutex::new(vec![
            "Patched skill 'deploy'.".to_string(),
            "Wrote references/staging.md for skill 'deploy'.".to_string(),
        ]);
        let summary = summarize_actions(&receipts, "");
        assert!(summary.contains("patched deploy"));
        assert!(summary.contains("wrote file for deploy"));
    }

    #[test]
    fn summarize_actions_empty_for_nothing_to_save() {
        let receipts = Mutex::new(Vec::new());
        let summary = summarize_actions(&receipts, "Nothing to save.");
        assert_eq!(summary, "");
    }

    #[test]
    fn summarize_actions_empty_when_no_receipts_and_no_text() {
        let receipts = Mutex::new(Vec::new());
        let summary = summarize_actions(&receipts, "");
        assert_eq!(summary, "");
    }

    #[test]
    fn summarize_actions_falls_back_to_first_line() {
        let receipts = Mutex::new(Vec::new());
        let summary = summarize_actions(
            &receipts,
            "Noted that deploy needs DEPLOY_TOKEN.\nMore details below.",
        );
        assert!(summary.starts_with("Noted that deploy"));
    }

    #[test]
    fn summarize_actions_handles_non_ascii_without_panic() {
        let receipts = Mutex::new(Vec::new());
        // 80+ chars of multi-byte content — must not panic on slicing.
        let text = "あ".repeat(50);
        let summary = summarize_actions(&receipts, &text);
        assert!(!summary.is_empty());
        // Verify it's valid UTF-8 (would have panicked if not).
        let _ = summary.chars().count();
    }

    #[test]
    fn extract_action_summary_handles_archive() {
        let receipt = "Archived skill 'old-thing' to /tmp/...";
        assert_eq!(
            extract_action_summary(receipt),
            Some("archived old-thing".to_string())
        );
    }
}
