//! ZeroTrading — 量化交易专用 PromptSection
//!
//! 将 [`TradingEngine`] 输出的 skills 块作为一个独立的 `PromptSection`
//! 接入 zeroclaw 原有的 `SystemPromptBuilder`，在 `SkillsSection` 之后注入。

use crate::agent::prompt::{PromptContext, PromptSection};
use crate::zerotrading::engine::TradingEngine;
use anyhow::Result;
use std::sync::Arc;

/// 量化交易 PromptSection
///
/// 注入位置：`SystemPromptBuilder` 的末尾（在通用技能之后）。
/// 内容：按风控→因子→策略→经验顺序的分块 Markdown 技能说明。
pub struct TradingSectionBuilder {
    engine: Arc<TradingEngine>,
}

impl TradingSectionBuilder {
    pub fn new(engine: Arc<TradingEngine>) -> Self {
        Self { engine }
    }
}

impl PromptSection for TradingSectionBuilder {
    fn name(&self) -> &str {
        "zerotrading"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        let section = self.engine.build_prompt_section();
        Ok(section)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
    use crate::security::AutonomyLevel;
    use crate::zerotrading::engine::TradingEngine;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn make_engine_with_skill(tmp: &TempDir) -> Arc<TradingEngine> {
        let skills_root = tmp.path().join("zerotrading").join("skills").join("风控");
        fs::create_dir_all(&skills_root).unwrap();
        fs::write(
            skills_root.join("stop_loss.md"),
            "# 止损约束\n\n超过 5% 强制止损。",
        )
        .unwrap();
        Arc::new(TradingEngine::with_defaults(tmp.path()))
    }

    #[test]
    fn trading_section_injects_into_prompt_builder() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine_with_skill(&tmp);
        let section = TradingSectionBuilder::new(engine);

        let tools: Vec<Box<dyn crate::tools::traits::Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = section.build(&ctx).unwrap();
        assert!(output.contains("ZeroTrading"));
        assert!(output.contains("风控"));
        assert!(output.contains("stop_loss"));
    }

    #[test]
    fn trading_section_empty_when_no_skills() {
        let tmp = TempDir::new().unwrap();
        // 未创建任何 Skills 目录
        let engine = Arc::new(TradingEngine::with_defaults(tmp.path()));
        let section = TradingSectionBuilder::new(engine);

        let tools: Vec<Box<dyn crate::tools::traits::Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = section.build(&ctx).unwrap();
        assert!(output.is_empty(), "no skills → empty section");
    }

    #[test]
    fn trading_section_name_is_zerotrading() {
        let engine = Arc::new(TradingEngine::with_defaults(Path::new("/tmp")));
        let section = TradingSectionBuilder::new(engine);
        assert_eq!(section.name(), "zerotrading");
    }
}
