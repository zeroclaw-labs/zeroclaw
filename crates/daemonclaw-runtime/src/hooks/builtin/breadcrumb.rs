use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;

use crate::hooks::traits::HookHandler;
use crate::tasks::CURRENT_TASK_BINDING;
use daemonclaw_api::tool::ToolResult;

pub struct BreadcrumbHook {
    workspace_dir: PathBuf,
    summary_cap: usize,
}

impl BreadcrumbHook {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            summary_cap: 500,
        }
    }
}

fn truncate_summary(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        s.to_string()
    } else {
        format!("{}…[truncated]", &s[..cap])
    }
}

#[async_trait]
impl HookHandler for BreadcrumbHook {
    fn name(&self) -> &str {
        "breadcrumb"
    }

    fn priority(&self) -> i32 {
        -200
    }

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, _duration: Duration) {
        let binding = match CURRENT_TASK_BINDING.try_with(|b| b.clone()) {
            Ok(Some(b)) => b,
            _ => return,
        };

        let result_summary = truncate_summary(&result.output, self.summary_cap);

        if let Err(e) = crate::tasks::store::insert_breadcrumb(
            &self.workspace_dir,
            &binding.task_id,
            Some(&binding.actor_id),
            tool,
            None,
            Some(&result_summary),
        ) {
            tracing::warn!(task_id = %binding.task_id, tool, "breadcrumb write failed: {e}");
        }
    }
}
