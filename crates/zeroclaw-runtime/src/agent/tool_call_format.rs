//! Canonical text-mode tool-call formatting guidance.
//!
//! This module is the single source of truth for the `<tool_call>` protocol
//! block that goes into a system prompt when a model is driven with text
//! (XML-tag) tool calling rather than native tool specs.
//!
//! Every prompt builder that advertises the text tool protocol MUST push
//! [`TOOL_CALL_PROTOCOL_INSTRUCTIONS`] verbatim instead of re-typing the
//! wording. Builder-specific material (the `### Available Tools` listing, for
//! example) layers *around* the shared block; it never restates it. Two
//! builders previously carried near-duplicate copies of this text and had
//! already drifted — the XML dispatcher's copy was missing the `CRITICAL:`
//! line and the worked example — so tool-use behavior silently depended on
//! which builder produced the prompt.
//!
//! Consumers:
//! - `agent::dispatcher::XmlToolDispatcher::prompt_instructions`
//! - `agent::loop_::build_tool_instructions` (and its `_for_names` sibling)

/// The canonical tool-call formatting block, including its `## Tool Use
/// Protocol` heading and a trailing blank line.
///
/// The text tells the model to emit real `<tool_call>` tags wrapping a JSON
/// object with a top-level `name` and a nested `arguments` object.
///
/// INVARIANT: every example here stays availability-neutral — it names the
/// `tool_name` placeholder, never a concrete tool. Both consumers render this
/// block against a *policy-filtered* tool slice (the XML dispatcher does not
/// even emit the listing; `prompt::ToolsSection` advertises the filtered slice
/// separately), so a concrete tool named here is guidance the prompt cannot
/// guarantee. A `file_read`-only risk profile once got a prompt that listed
/// `file_read` while commanding an unavailable `shell`. Naming a real tool in
/// an example position is a prompt bug, not a wording preference; it is pinned
/// by `rendered_prompts_never_prescribe_a_tool_outside_the_effective_slice`.
///
/// Callers that need a leading blank line before the heading push `'\n'`
/// themselves; the constant deliberately starts at the heading so it can be
/// embedded in prompts that already end with a separator.
pub(crate) const TOOL_CALL_PROTOCOL_INSTRUCTIONS: &str = r#"## Tool Use Protocol

To use a tool, wrap a JSON object in <tool_call></tool_call> tags:

```
<tool_call>
{"name": "tool_name", "arguments": {"param": "value"}}
</tool_call>
```

CRITICAL: Output actual <tool_call> tags—never describe steps or give examples.

Example: User says something that needs a tool. You MUST respond with a real call naming one of your available tools:
<tool_call>
{"name": "tool_name", "arguments": {"param": "value"}}
</tool_call>

You may use multiple tool calls in a single response. After tool execution, results appear in <tool_result> tags. Continue reasoning with the results until you can give a final answer.

"#;

#[cfg(test)]
mod tests {
    use super::TOOL_CALL_PROTOCOL_INSTRUCTIONS;
    use crate::agent::dispatcher::{ToolDispatcher, XmlToolDispatcher};
    use crate::agent::loop_::{build_tool_instructions, build_tool_instructions_for_names};
    use crate::security::SecurityPolicy;
    use crate::tools::{Tool, default_tools};
    use std::collections::HashSet;

    fn probe_tools() -> Vec<Box<dyn Tool>> {
        let security = std::sync::Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        default_tools(security)
    }

    /// Pins the shape of the canonical block: heading first, trailing blank
    /// line last, so call sites can concatenate it without guessing at
    /// separators.
    #[test]
    fn canonical_block_starts_with_heading_and_ends_with_blank_line() {
        assert!(
            TOOL_CALL_PROTOCOL_INSTRUCTIONS.starts_with("## Tool Use Protocol\n\n"),
            "shared block must open with the protocol heading"
        );
        assert!(
            TOOL_CALL_PROTOCOL_INSTRUCTIONS.ends_with("final answer.\n\n"),
            "shared block must end with a trailing blank line"
        );
    }

    /// The acceptance criterion from the issue: the guidance still tells the
    /// model to emit real tags carrying `name` and a nested `arguments`.
    #[test]
    fn canonical_block_demands_real_tags_with_name_and_arguments() {
        for required in [
            "<tool_call>",
            "</tool_call>",
            r#"{"name": "tool_name", "arguments": {"param": "value"}}"#,
            "CRITICAL: Output actual <tool_call> tags",
            // Pins the worked example by its imperative lead-in rather than
            // by its JSON payload: the payload is now byte-identical to the
            // idiom line above, so only this sentence proves the example
            // survived — and that it stayed availability-neutral.
            "You MUST respond with a real call naming one of your available tools",
        ] {
            assert!(
                TOOL_CALL_PROTOCOL_INSTRUCTIONS.contains(required),
                "shared block lost required guidance: {required:?}"
            );
        }
    }

    /// Anti-drift guard. Both text-protocol prompt builders must embed the
    /// shared block byte-for-byte; a builder that re-types the wording (or
    /// keeps a stale copy) fails here.
    #[test]
    fn both_prompt_builders_embed_the_shared_block() {
        let tools = probe_tools();

        let dispatcher_block = XmlToolDispatcher.prompt_instructions(&tools);
        assert!(
            dispatcher_block.contains(TOOL_CALL_PROTOCOL_INSTRUCTIONS),
            "XmlToolDispatcher::prompt_instructions drifted from the shared block:\n{dispatcher_block}"
        );

        let loop_block = build_tool_instructions(&tools);
        assert!(
            loop_block.contains(TOOL_CALL_PROTOCOL_INSTRUCTIONS),
            "build_tool_instructions drifted from the shared block:\n{loop_block}"
        );
    }

    /// Regression for the availability-neutrality invariant.
    ///
    /// A policy-filtered registry that excludes `shell` must never render a
    /// prompt that commands the model to call `shell` — or any other tool
    /// outside the effective slice. The shared block used to hard-code a
    /// `shell`/`date` worked example; because the XML dispatcher appends the
    /// block whenever its effective slice is non-empty (while `ToolsSection`
    /// advertises only that filtered slice), a `file_read`-only risk profile
    /// got a prompt listing `file_read` and imperatively demanding `shell`.
    ///
    /// "Imperative example position" is checked as a JSON tool call naming a
    /// tool (`"name": "x"`), which is the only place the block tells the model
    /// what to emit. That keeps the assertion off prose in tool descriptions.
    #[test]
    fn rendered_prompts_never_prescribe_a_tool_outside_the_effective_slice() {
        const EFFECTIVE: &str = "file_read";

        let full_registry = probe_tools();
        let foreign_names: Vec<String> = full_registry
            .iter()
            .map(|tool| tool.name().to_string())
            .filter(|name| name != EFFECTIVE)
            .collect();
        assert!(
            foreign_names.iter().any(|name| name == "shell"),
            "probe registry must still contain `shell`, or this regression proves nothing"
        );

        let mut effective_tools = probe_tools();
        effective_tools.retain(|tool| tool.name() == EFFECTIVE);
        assert_eq!(
            effective_tools.len(),
            1,
            "expected exactly one `{EFFECTIVE}` tool in the default registry"
        );
        let effective_names: HashSet<&str> = HashSet::from([EFFECTIVE]);

        // Every builder that can render the shared block against a filtered
        // slice, including the policy-filtering entry point the risk profile
        // actually goes through.
        let rendered = [
            (
                "XmlToolDispatcher::prompt_instructions",
                XmlToolDispatcher.prompt_instructions(&effective_tools),
            ),
            (
                "build_tool_instructions",
                build_tool_instructions(&effective_tools),
            ),
            (
                "build_tool_instructions_for_names",
                build_tool_instructions_for_names(&full_registry, &effective_names),
            ),
        ];

        for (builder, prompt) in &rendered {
            assert!(
                !prompt.is_empty(),
                "{builder} rendered nothing for a non-empty tool slice"
            );
            for foreign in &foreign_names {
                for pattern in [
                    format!(r#""name": "{foreign}""#),
                    format!(r#""name":"{foreign}""#),
                ] {
                    assert!(
                        !prompt.contains(&pattern),
                        "{builder} prescribes unavailable tool `{foreign}` via {pattern:?}:\n{prompt}"
                    );
                }
            }
            // The neutral placeholder is what must survive in its place.
            assert!(
                prompt.contains(r#"{"name": "tool_name", "arguments": {"param": "value"}}"#),
                "{builder} lost the availability-neutral example:\n{prompt}"
            );
        }
    }

    /// Byte-identity pin for the loop_ builder: the shared block plus the
    /// builder's own `### Available Tools` header must reproduce the exact
    /// prefix the builder emitted before the block was extracted.
    #[test]
    fn loop_builder_prefix_is_shared_block_then_tool_listing() {
        let tools = probe_tools();
        let expected_prefix = format!("\n{TOOL_CALL_PROTOCOL_INSTRUCTIONS}### Available Tools\n\n");

        let instructions = build_tool_instructions(&tools);
        assert!(
            instructions.starts_with(&expected_prefix),
            "loop_ tool instructions no longer start with the canonical envelope:\n{instructions}"
        );
    }
}
