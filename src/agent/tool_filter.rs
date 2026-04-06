use std::collections::HashSet;

use crate::config::schema::ToolFilterGroup;
use crate::tools::Tool;

pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    match pattern.find('*') {
        None => pattern == name,
        Some(star) => {
            let prefix = &pattern[..star];
            let suffix = &pattern[star + 1..];
            name.starts_with(prefix)
                && name.ends_with(suffix)
                && name.len() >= prefix.len() + suffix.len()
        }
    }
}

/// Returns the subset of `tool_specs` that should be sent to the LLM for this turn.
///
/// Rules (mirrors NullClaw `filterToolSpecsForTurn`):
/// - Built-in tools (names that do not start with `"mcp_"`) always pass through.
/// - When `groups` is empty, all tools pass through (backward compatible default).
/// - An MCP tool is included if at least one group matches it:
///   - `always` group: included unconditionally if any pattern matches the tool name.
///   - `dynamic` group: included if any pattern matches AND the user message contains
///     at least one keyword (case-insensitive substring).
pub(crate) fn filter_tool_specs_for_turn(
    tool_specs: Vec<crate::tools::ToolSpec>,
    groups: &[ToolFilterGroup],
    user_message: &str,
) -> Vec<crate::tools::ToolSpec> {
    use crate::config::schema::ToolFilterGroupMode;

    if groups.is_empty() {
        return tool_specs;
    }

    let msg_lower = user_message.to_ascii_lowercase();

    tool_specs
        .into_iter()
        .filter(|spec| {
            // Built-in tools always pass through.
            if !spec.name.starts_with("mcp_") {
                return true;
            }
            // MCP tool: include if any active group matches.
            groups.iter().any(|group| {
                let pattern_matches = group.tools.iter().any(|pat| glob_match(pat, &spec.name));
                if !pattern_matches {
                    return false;
                }
                match group.mode {
                    ToolFilterGroupMode::Always => true,
                    ToolFilterGroupMode::Dynamic => group
                        .keywords
                        .iter()
                        .any(|kw| msg_lower.contains(&kw.to_ascii_lowercase())),
                }
            })
        })
        .collect()
}

/// Filters a tool spec list by an optional capability allowlist.
///
/// When `allowed` is `None`, all specs pass through unchanged.
/// When `allowed` is `Some(list)`, only specs whose name appears in the list
/// are retained. Unknown names in the allowlist are silently ignored.
pub(crate) fn filter_by_allowed_tools(
    specs: Vec<crate::tools::ToolSpec>,
    allowed: Option<&[String]>,
) -> Vec<crate::tools::ToolSpec> {
    match allowed {
        None => specs,
        Some(list) => specs
            .into_iter()
            .filter(|spec| list.iter().any(|name| name == &spec.name))
            .collect(),
    }
}

/// Computes the list of MCP tool names that should be excluded for a given turn
/// based on `tool_filter_groups` and the user message.
///
/// Returns an empty `Vec` when `groups` is empty (no filtering).
pub(crate) fn compute_excluded_mcp_tools(
    tools_registry: &[Box<dyn Tool>],
    groups: &[ToolFilterGroup],
    user_message: &str,
) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    let filtered_specs = filter_tool_specs_for_turn(
        tools_registry.iter().map(|t| t.spec()).collect(),
        groups,
        user_message,
    );
    let included: HashSet<&str> = filtered_specs.iter().map(|s| s.name.as_str()).collect();
    tools_registry
        .iter()
        .filter(|t| t.name().starts_with("mcp_") && !included.contains(t.name()))
        .map(|t| t.name().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{ToolFilterGroup, ToolFilterGroupMode};

    fn make_spec(name: &str) -> crate::tools::ToolSpec {
        crate::tools::ToolSpec {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }
    }

    #[test]
    fn glob_match_exact_no_wildcard() {
        assert!(glob_match("mcp_browser_navigate", "mcp_browser_navigate"));
        assert!(!glob_match("mcp_browser_navigate", "mcp_browser_click"));
    }

    #[test]
    fn glob_match_prefix_wildcard() {
        // Suffix pattern: mcp_browser_*
        assert!(glob_match("mcp_browser_*", "mcp_browser_navigate"));
        assert!(glob_match("mcp_browser_*", "mcp_browser_click"));
        assert!(!glob_match("mcp_browser_*", "mcp_filesystem_read"));

        // Prefix pattern: *_read
        assert!(glob_match("*_read", "mcp_filesystem_read"));
        assert!(!glob_match("*_read", "mcp_filesystem_write"));

        // Infix: mcp_*_navigate
        assert!(glob_match("mcp_*_navigate", "mcp_browser_navigate"));
        assert!(!glob_match("mcp_*_navigate", "mcp_browser_click"));
    }

    #[test]
    fn glob_match_star_matches_everything() {
        assert!(glob_match("*", "anything_at_all"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn filter_tool_specs_no_groups_returns_all() {
        let specs = vec![
            make_spec("shell_exec"),
            make_spec("mcp_browser_navigate"),
            make_spec("mcp_filesystem_read"),
        ];
        let result = filter_tool_specs_for_turn(specs, &[], "hello");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn filter_tool_specs_always_group_includes_matching_mcp_tool() {
        use crate::config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![
            make_spec("shell_exec"),
            make_spec("mcp_browser_navigate"),
            make_spec("mcp_filesystem_read"),
        ];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Always,
            tools: vec!["mcp_filesystem_*".into()],
            keywords: vec![],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "anything");
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        // Built-in passes through, matched MCP passes, unmatched MCP excluded.
        assert!(names.contains(&"shell_exec"));
        assert!(names.contains(&"mcp_filesystem_read"));
        assert!(!names.contains(&"mcp_browser_navigate"));
    }

    #[test]
    fn filter_tool_specs_dynamic_group_included_on_keyword_match() {
        use crate::config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![make_spec("shell_exec"), make_spec("mcp_browser_navigate")];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["browse".into(), "website".into()],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "please browse this page");
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"shell_exec"));
        assert!(names.contains(&"mcp_browser_navigate"));
    }

    #[test]
    fn filter_tool_specs_dynamic_group_excluded_on_no_keyword_match() {
        use crate::config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![make_spec("shell_exec"), make_spec("mcp_browser_navigate")];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["browse".into(), "website".into()],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "read the file /etc/hosts");
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"shell_exec"));
        assert!(!names.contains(&"mcp_browser_navigate"));
    }

    #[test]
    fn filter_tool_specs_dynamic_keyword_match_is_case_insensitive() {
        use crate::config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![make_spec("mcp_browser_navigate")];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["Browse".into()],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "BROWSE the site");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_by_allowed_tools_none_passes_all() {
        let specs = vec![
            make_spec("shell"),
            make_spec("memory_store"),
            make_spec("file_read"),
        ];
        let result = filter_by_allowed_tools(specs, None);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn filter_by_allowed_tools_some_restricts_to_listed() {
        let specs = vec![
            make_spec("shell"),
            make_spec("memory_store"),
            make_spec("file_read"),
        ];
        let allowed = vec!["shell".to_string(), "memory_store".to_string()];
        let result = filter_by_allowed_tools(specs, Some(&allowed));
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"memory_store"));
        assert!(!names.contains(&"file_read"));
    }

    #[test]
    fn filter_by_allowed_tools_unknown_names_silently_ignored() {
        let specs = vec![make_spec("shell"), make_spec("file_read")];
        let allowed = vec![
            "shell".to_string(),
            "nonexistent_tool".to_string(),
            "another_missing".to_string(),
        ];
        let result = filter_by_allowed_tools(specs, Some(&allowed));
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"shell"));
    }

    #[test]
    fn filter_by_allowed_tools_empty_list_excludes_all() {
        let specs = vec![make_spec("shell"), make_spec("file_read")];
        let allowed: Vec<String> = vec![];
        let result = filter_by_allowed_tools(specs, Some(&allowed));
        assert!(result.is_empty());
    }
}
