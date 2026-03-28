use crate::providers::traits::ChatMessage;
use std::collections::HashSet;

/// Signals extracted from conversation context to guide tool filtering.
#[derive(Debug, Clone)]
pub struct ContextSignals {
    /// Tool names likely needed. Empty vec means no filtering.
    pub suggested_tools: Vec<String>,
    /// Whether full history is relevant.
    pub history_relevant: bool,
}

/// Analyze context to determine which tools are likely needed.
pub fn analyze_turn_context(
    history: &[ChatMessage],
    _user_message: &str,
    iteration: usize,
    last_tool_calls: &[String],
) -> ContextSignals {
    if iteration == 0 {
        return ContextSignals {
            suggested_tools: Vec::new(),
            history_relevant: true,
        };
    }

    let mut tools: HashSet<String> = HashSet::new();
    for tool in last_tool_calls {
        tools.insert(tool.clone());
    }

    if let Some(last_assistant) = history.iter().rev().find(|m| m.role == "assistant") {
        for word in last_assistant.content.split_whitespace() {
            for tool_name in tools_for_keyword(word) {
                tools.insert(String::from(*tool_name));
            }
        }
    }

    let mut suggested: Vec<String> = tools.into_iter().collect();
    suggested.sort();

    ContextSignals {
        suggested_tools: suggested,
        history_relevant: true,
    }
}

fn tools_for_keyword(keyword: &str) -> &'static [&'static str] {
    match keyword.to_lowercase().as_str() {
        "file" | "read" | "write" | "edit" | "path" | "directory" => {
            &["file_read", "file_write", "file_edit", "glob_search"]
        }
        "shell" | "command" | "run" | "execute" | "install" | "build" => &["shell"],
        "memory" | "remember" | "recall" | "store" | "forget" => &["memory_store", "memory_recall"],
        "search" | "find" | "grep" | "look" => {
            &["content_search", "glob_search", "web_search_tool"]
        }
        "browser" | "website" | "url" | "http" | "fetch" => &["web_fetch", "web_search_tool"],
        "image" | "screenshot" | "picture" => &["image_info"],
        "git" | "commit" | "branch" | "push" | "pull" => &["git_operations", "shell"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn iteration_zero_returns_empty_suggestions() {
        let history = vec![make_message("user", "hello")];
        let signals = analyze_turn_context(&history, "do something", 0, &[]);
        assert!(signals.suggested_tools.is_empty());
        assert!(signals.history_relevant);
    }

    #[test]
    fn iteration_one_includes_last_tools() {
        let history = vec![
            make_message("user", "hello"),
            make_message("assistant", "sure"),
        ];
        let last_tools = vec!["shell".to_string(), "file_read".to_string()];
        let signals = analyze_turn_context(&history, "next step", 1, &last_tools);
        assert!(signals.suggested_tools.contains(&"shell".to_string()));
        assert!(signals.suggested_tools.contains(&"file_read".to_string()));
    }

    #[test]
    fn keyword_extraction_from_assistant_message() {
        let history = vec![
            make_message("user", "help me"),
            make_message("assistant", "I will read the file at that path"),
        ];
        let signals = analyze_turn_context(&history, "ok", 1, &[]);
        assert!(signals.suggested_tools.contains(&"file_read".to_string()));
    }

    #[test]
    fn shell_keywords_suggest_shell_tool() {
        let history = vec![
            make_message("user", "build the project"),
            make_message("assistant", "I will run the build command"),
        ];
        let signals = analyze_turn_context(&history, "go", 1, &[]);
        assert!(signals.suggested_tools.contains(&"shell".to_string()));
    }

    #[test]
    fn memory_keywords_suggest_memory_tools() {
        let history = vec![
            make_message("user", "save this"),
            make_message("assistant", "I will store that in memory"),
        ];
        let signals = analyze_turn_context(&history, "ok", 1, &[]);
        assert!(
            signals
                .suggested_tools
                .contains(&"memory_store".to_string())
        );
        assert!(
            signals
                .suggested_tools
                .contains(&"memory_recall".to_string())
        );
    }

    #[test]
    fn combined_keywords_merge_tools() {
        let history = vec![
            make_message("user", "do stuff"),
            make_message(
                "assistant",
                "I need to read the file and run a shell command to search",
            ),
        ];
        let signals = analyze_turn_context(&history, "go", 1, &[]);
        assert!(signals.suggested_tools.contains(&"file_read".to_string()));
        assert!(signals.suggested_tools.contains(&"shell".to_string()));
        assert!(
            signals
                .suggested_tools
                .contains(&"content_search".to_string())
        );
    }

    #[test]
    fn empty_history_iteration_one() {
        let history: Vec<ChatMessage> = vec![];
        let signals = analyze_turn_context(&history, "hello", 1, &[]);
        assert!(signals.suggested_tools.is_empty());
    }
}
