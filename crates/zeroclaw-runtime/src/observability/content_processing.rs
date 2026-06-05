use crate::security::leak_detector::{LeakDetector, LeakResult};
use zeroclaw_config::schema::ObservabilityConfig;
use zeroclaw_log::{LlmIoPolicy, ToolIoPolicy};

const PRE_SANITIZATION_CAP_CHARS: usize = 1_048_576;

#[derive(Debug, Clone)]
pub struct ContentProcessor {
    llm_policy: LlmIoPolicy,
    llm_max_chars: usize,
    tool_policy: ToolIoPolicy,
    tool_max_chars: usize,
    detector: LeakDetector,
}

impl ContentProcessor {
    pub fn from_observability(config: &ObservabilityConfig) -> Self {
        warn_unknown_llm_policy(&config.log_llm_io);
        warn_unknown_tool_policy(&config.log_tool_io);

        Self {
            llm_policy: LlmIoPolicy::from_raw(&config.log_llm_io),
            llm_max_chars: config.log_llm_io_max_chars.max(1),
            tool_policy: ToolIoPolicy::from_raw(&config.log_tool_io),
            tool_max_chars: config.log_tool_io_truncate_bytes.max(1),
            detector: LeakDetector::new(),
        }
    }

    pub fn process_user_message(&self, content: &str) -> Option<String> {
        self.process_llm(content)
    }

    pub fn process_response_content(&self, content: &str) -> Option<String> {
        self.process_llm(content)
    }

    pub fn process_tool_result(&self, content: &str) -> Option<String> {
        if !self.tool_policy.captures_io() {
            return None;
        }

        let trimmed = content.trim();
        if trimmed.is_empty() {
            return None;
        }

        let capped = truncate_content(trimmed, PRE_SANITIZATION_CAP_CHARS);
        let redacted = match self.detector.scan(&capped) {
            LeakResult::Clean => capped,
            LeakResult::Detected { redacted, .. } => redacted,
        };

        match self.tool_policy {
            ToolIoPolicy::Off => None,
            ToolIoPolicy::Redacted => Some(truncate_content(&redacted, self.tool_max_chars)),
            ToolIoPolicy::Full => Some(redacted),
        }
    }

    fn process_llm(&self, content: &str) -> Option<String> {
        if !self.llm_policy.captures_io() {
            return None;
        }

        let trimmed = content.trim();
        if trimmed.is_empty() {
            return None;
        }

        let capped = truncate_content(trimmed, PRE_SANITIZATION_CAP_CHARS);
        let redacted = match self.detector.scan(&capped) {
            LeakResult::Clean => capped,
            LeakResult::Detected { redacted, .. } => redacted,
        };

        match self.llm_policy {
            LlmIoPolicy::Off => None,
            LlmIoPolicy::Redacted => Some(truncate_content(&redacted, self.llm_max_chars)),
            LlmIoPolicy::Full => Some(redacted),
        }
    }
}

fn warn_unknown_llm_policy(raw: &str) {
    if !matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "off" | "redacted" | "full" | ""
    ) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"value": raw, "config_key": "log_llm_io", "default": "off"})),
            "unknown log_llm_io value, defaulting to off"
        );
    }
}

fn warn_unknown_tool_policy(raw: &str) {
    if !matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "off" | "redacted" | "full" | ""
    ) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"value": raw, "config_key": "log_tool_io", "default": "redacted"})),
            "unknown log_tool_io value, defaulting to redacted"
        );
    }
}

fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let truncated: String = content.chars().take(max_chars).collect();
    let total = content.chars().count();
    format!("{}...(truncated, total {} chars)", truncated, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(policy: &str, max_chars: usize) -> ObservabilityConfig {
        ObservabilityConfig {
            log_llm_io: policy.to_string(),
            log_llm_io_max_chars: max_chars,
            ..ObservabilityConfig::default()
        }
    }

    #[test]
    fn off_returns_none() {
        let processor = ContentProcessor::from_observability(&config("off", 200));
        assert_eq!(processor.process_user_message("hello"), None);
        assert_eq!(processor.process_response_content("world"), None);
    }

    #[test]
    fn redacted_sanitizes_and_truncates() {
        let processor = ContentProcessor::from_observability(&config("redacted", 20));
        let processed = processor
            .process_user_message("my key is sk-ant-abcdefghijklmnopqrstuvwxyz123456")
            .expect("content should be captured");
        assert!(processed.contains("[REDACTED"));
        assert!(!processed.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert!(processed.contains("truncated"));
    }

    #[test]
    fn full_sanitizes_without_configured_truncation() {
        let processor = ContentProcessor::from_observability(&config("full", 5));
        let processed = processor
            .process_response_content("normal response text")
            .expect("content should be captured");
        assert_eq!(processed, "normal response text");
    }

    #[test]
    fn utf8_truncation_is_char_safe() {
        let processor = ContentProcessor::from_observability(&config("redacted", 3));
        let processed = processor
            .process_user_message("你好世界")
            .expect("content should be captured");
        assert!(processed.starts_with("你好世"));
    }

    fn tool_config(tool_policy: &str, tool_max_chars: usize) -> ObservabilityConfig {
        ObservabilityConfig {
            log_tool_io: tool_policy.to_string(),
            log_tool_io_truncate_bytes: tool_max_chars,
            ..ObservabilityConfig::default()
        }
    }

    #[test]
    fn tool_off_returns_none() {
        let processor = ContentProcessor::from_observability(&tool_config("off", 200));
        assert_eq!(processor.process_tool_result("hello"), None);
    }

    #[test]
    fn tool_redacted_sanitizes_and_truncates() {
        let processor = ContentProcessor::from_observability(&tool_config("redacted", 20));
        let processed = processor
            .process_tool_result("my key is sk-ant-abcdefghijklmnopqrstuvwxyz123456")
            .expect("tool result should be captured");
        assert!(processed.contains("[REDACTED"));
        assert!(!processed.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert!(processed.contains("truncated"));
    }

    #[test]
    fn tool_full_keeps_everything() {
        let processor = ContentProcessor::from_observability(&tool_config("full", 5));
        let processed = processor
            .process_tool_result("normal tool output")
            .expect("tool result should be captured");
        assert_eq!(processed, "normal tool output");
    }
}
