//! ZeroTrading — 引擎核心
//!
//! 量化交易引擎主体：初始化、Skills 管理、状态快照、决策入口。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::zerotrading::skills::{
    load_trading_skills, resolve_skills_dir, skills_to_trading_prompt,
};
use crate::zerotrading::types::{
    SkillCategoryCount, TradingEngineConfig, TradingEngineStatus, TradingSkill,
};

// ─── 引擎主体 ─────────────────────────────────────────────────────────────────

/// ZeroTrading 量化交易引擎
///
/// 负责:
/// 1. 在启动时加载 `workspace/zerotrading/skills/` 下所有 `.md` 技能文件
/// 2. 按优先级注入 LLM 提示词（风控 > 因子 > 策略 > 经验）
/// 3. 暴露引擎状态接口（用于 `/api/status` 埋点）
/// 4. 支持热重载（调用 `reload()`）
pub struct TradingEngine {
    config: TradingEngineConfig,
    skills_dir: PathBuf,
    /// 运行时 Skills 列表（RwLock 允许并发读 + 按需热重载写）
    skills: Arc<RwLock<Vec<TradingSkill>>>,
}

impl TradingEngine {
    /// 从配置和 workspace 目录初始化引擎并加载所有 Skills。
    pub fn new(workspace_dir: &Path, config: TradingEngineConfig) -> Self {
        let skills_dir = resolve_skills_dir(workspace_dir, config.skills_dir.as_deref());

        let skills = if config.enabled {
            let loaded = load_trading_skills(&skills_dir);
            if config.log_skills_on_load && !loaded.is_empty() {
                tracing::info!(
                    count = loaded.len(),
                    dir = %skills_dir.display(),
                    "🤖 ZeroTrading: loaded quantitative skills"
                );
                for skill in &loaded {
                    tracing::debug!(
                        category = skill.category.label_en(),
                        name = %skill.name,
                        "  → skill loaded"
                    );
                }
            } else if loaded.is_empty() {
                tracing::debug!(
                    dir = %skills_dir.display(),
                    "ZeroTrading: no skills found in directory"
                );
            }
            loaded
        } else {
            tracing::debug!("ZeroTrading engine disabled");
            Vec::new()
        };

        Self {
            config,
            skills_dir,
            skills: Arc::new(RwLock::new(skills)),
        }
    }

    /// 默认初始化（从 workspace 加载，使用默认配置）
    pub fn with_defaults(workspace_dir: &Path) -> Self {
        Self::new(workspace_dir, TradingEngineConfig::default())
    }

    /// 是否已启用
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// 返回已加载的 Skills 数量
    pub fn skills_count(&self) -> usize {
        self.skills.read().len()
    }

    /// 获取当前 Skills 快照（克隆列表，避免长期持有读锁）
    pub fn skills_snapshot(&self) -> Vec<TradingSkill> {
        self.skills.read().clone()
    }

    /// 生成注入 LLM 提示词的量化技能块。
    ///
    /// 若引擎未启用或无 Skills，返回空字符串。
    pub fn build_prompt_section(&self) -> String {
        if !self.config.enabled {
            return String::new();
        }
        let skills = self.skills.read();
        if skills.is_empty() {
            return String::new();
        }
        skills_to_trading_prompt(&skills, self.config.max_skill_chars)
    }

    /// 热重载：重新扫描 Skills 目录，更新内存中的 Skills 列表。
    ///
    /// 线程安全：写锁时间极短（只替换 Vec），不影响并发读。
    pub fn reload(&self) -> usize {
        let new_skills = load_trading_skills(&self.skills_dir);
        let count = new_skills.len();

        *self.skills.write() = new_skills;

        tracing::info!(
            count,
            dir = %self.skills_dir.display(),
            "ZeroTrading: skills reloaded"
        );

        count
    }

    /// 获取引擎运行时状态快照（用于 /api/status 等接口）
    pub fn status(&self) -> TradingEngineStatus {
        let skills = self.skills.read();
        let mut by_cat = SkillCategoryCount::default();
        for skill in skills.iter() {
            use crate::zerotrading::types::SkillCategory;
            match skill.category {
                SkillCategory::RiskControl => by_cat.risk_control += 1,
                SkillCategory::Factor => by_cat.factor += 1,
                SkillCategory::Strategy => by_cat.strategy += 1,
                SkillCategory::Experience => by_cat.experience += 1,
            }
        }
        TradingEngineStatus {
            enabled: self.config.enabled,
            skills_loaded: skills.len(),
            skills_by_category: by_cat,
            skills_dir: self.skills_dir.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_workspace_with_skills(tmp: &TempDir) -> PathBuf {
        let skills_root = tmp.path().join("zerotrading").join("skills");
        for cat in &["风控", "因子", "策略", "经验"] {
            fs::create_dir_all(skills_root.join(cat)).unwrap();
        }

        fs::write(
            skills_root.join("风控/max_loss.md"),
            "# 最大亏损限制\n\n单日亏损不超过 3%。",
        )
        .unwrap();
        fs::write(
            skills_root.join("策略/btc_breakout.md"),
            "# BTC 突破策略\n\n价格突破 20 日高点时买入。",
        )
        .unwrap();

        tmp.path().to_path_buf()
    }

    #[test]
    fn engine_loads_skills_on_init() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace_with_skills(&tmp);
        let engine = TradingEngine::with_defaults(&ws);
        assert_eq!(engine.skills_count(), 2);
    }

    #[test]
    fn engine_disabled_returns_empty_prompt() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace_with_skills(&tmp);
        let config = TradingEngineConfig {
            enabled: false,
            ..Default::default()
        };
        let engine = TradingEngine::new(&ws, config);
        assert!(engine.build_prompt_section().is_empty());
    }

    #[test]
    fn engine_prompt_contains_skill_headers() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace_with_skills(&tmp);
        let engine = TradingEngine::with_defaults(&ws);
        let prompt = engine.build_prompt_section();

        assert!(prompt.contains("ZeroTrading"));
        assert!(prompt.contains("风控"));
        assert!(prompt.contains("策略"));
        assert!(prompt.contains("max_loss"));
        assert!(prompt.contains("btc_breakout"));
    }

    #[test]
    fn engine_status_counts_correctly() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace_with_skills(&tmp);
        let engine = TradingEngine::with_defaults(&ws);
        let status = engine.status();

        assert!(status.enabled);
        assert_eq!(status.skills_loaded, 2);
        assert_eq!(status.skills_by_category.risk_control, 1);
        assert_eq!(status.skills_by_category.strategy, 1);
        assert_eq!(status.skills_by_category.factor, 0);
        assert_eq!(status.skills_by_category.experience, 0);
    }

    #[test]
    fn engine_reload_picks_up_new_files() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace_with_skills(&tmp);
        let engine = TradingEngine::with_defaults(&ws);
        assert_eq!(engine.skills_count(), 2);

        // 添加新 Skill 后热重载
        let new_skill = ws.join("zerotrading/skills/因子/funding_rate.md");
        fs::write(&new_skill, "# 资金费率\n\n资金费率 > 0.01% 偏多。").unwrap();

        let count = engine.reload();
        assert_eq!(count, 3);
        assert_eq!(engine.skills_count(), 3);
    }
}
