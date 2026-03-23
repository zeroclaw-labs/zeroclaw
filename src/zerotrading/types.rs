//! ZeroTrading — 共享类型定义
//!
//! 量化交易引擎的核心数据结构。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── 技能分类 ─────────────────────────────────────────────────────────────────

/// 量化 Skill 的分类枚举，决定注入优先级（数字越小越优先）。
///
/// 注入顺序: 风控(0) → 因子(1) → 策略(2) → 经验(3)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SkillCategory {
    /// 🛡️ 风控 — 强制性约束，最高优先级，不可被覆盖
    RiskControl = 0,
    /// 📊 因子 — 宏观/微观监控因子，环境上下文
    Factor = 1,
    /// 📈 策略 — 具体交易策略模型
    Strategy = 2,
    /// 🧠 经验 — 交易哲学与操盘心法
    Experience = 3,
}

impl SkillCategory {
    /// 返回该分类对应的目录名（中文）
    pub fn dir_name(&self) -> &'static str {
        match self {
            SkillCategory::RiskControl => "风控",
            SkillCategory::Factor => "因子",
            SkillCategory::Strategy => "策略",
            SkillCategory::Experience => "经验",
        }
    }

    /// 返回该分类的 Emoji 图标（用于 LLM 提示词标记）
    pub fn icon(&self) -> &'static str {
        match self {
            SkillCategory::RiskControl => "🛡️",
            SkillCategory::Factor => "📊",
            SkillCategory::Strategy => "📈",
            SkillCategory::Experience => "🧠",
        }
    }

    /// 返回该分类的英文标签
    pub fn label_en(&self) -> &'static str {
        match self {
            SkillCategory::RiskControl => "Risk Control",
            SkillCategory::Factor => "Factor",
            SkillCategory::Strategy => "Strategy",
            SkillCategory::Experience => "Experience",
        }
    }

    /// 从目录名推断分类
    pub fn from_dir_name(name: &str) -> Option<Self> {
        match name {
            "风控" | "risk" | "risk_control" | "riskcontrol" => Some(SkillCategory::RiskControl),
            "因子" | "factor" | "factors" => Some(SkillCategory::Factor),
            "策略" | "strategy" | "strategies" => Some(SkillCategory::Strategy),
            "经验" | "experience" | "exp" => Some(SkillCategory::Experience),
            _ => None,
        }
    }
}

// ─── 量化 Skill ───────────────────────────────────────────────────────────────

/// 一条量化 Skill 条目（从 .md 文件加载）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingSkill {
    /// Skill 名称（文件名去掉 .md）
    pub name: String,
    /// Skill 分类
    pub category: SkillCategory,
    /// Skill 全文内容（Markdown）
    pub content: String,
    /// 文件来源路径
    pub path: PathBuf,
}

impl TradingSkill {
    /// 返回一个人类可读的摘要（前 120 字符）
    pub fn summary(&self) -> String {
        let s = self.content.trim();
        if s.chars().count() <= 120 {
            s.to_string()
        } else {
            format!(
                "{}…",
                s.char_indices().nth(120).map(|(i, _)| &s[..i]).unwrap_or(s)
            )
        }
    }
}

// ─── 交易决策 ──────────────────────────────────────────────────────────────────

/// LLM 输出的标准化交易决策结构
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeDecision {
    /// 决策动作
    pub decision: TradeAction,
    /// 仓位大小（0.0–1.0，相对总权益百分比）
    pub size: f64,
    /// 可选：目标价格（限价单）
    pub price: Option<f64>,
    /// 可选：止损价格
    pub stop_loss: Option<f64>,
    /// 可选：止盈价格
    pub take_profit: Option<f64>,
    /// LLM 决策理由（用于审计）
    pub reason: String,
    /// 触发的技能名称
    pub triggered_skill: Option<String>,
}

/// 标准化的交易动作
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TradeAction {
    /// 做多开仓
    Buy,
    /// 做空开仓
    Sell,
    /// 平仓
    Close,
    /// 持仓观望
    Hold,
    /// 需要人工确认
    RequestApproval,
}

impl std::fmt::Display for TradeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TradeAction::Buy => write!(f, "BUY"),
            TradeAction::Sell => write!(f, "SELL"),
            TradeAction::Close => write!(f, "CLOSE"),
            TradeAction::Hold => write!(f, "HOLD"),
            TradeAction::RequestApproval => write!(f, "REQUEST_APPROVAL"),
        }
    }
}

// ─── 引擎配置 ─────────────────────────────────────────────────────────────────

/// ZeroTrading 引擎配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingEngineConfig {
    /// Skills 根目录（默认为 workspace/zerotrading/skills）
    pub skills_dir: Option<PathBuf>,
    /// 是否在 LLM 提示词中启用量化技能注入
    pub enabled: bool,
    /// 最大注入 Skill 字符总量（防止超出 context window）
    pub max_skill_chars: usize,
    /// 是否输出 Skills 加载摘要到日志
    pub log_skills_on_load: bool,
}

impl Default for TradingEngineConfig {
    fn default() -> Self {
        Self {
            skills_dir: None,
            enabled: true,
            max_skill_chars: 32_000,
            log_skills_on_load: true,
        }
    }
}

// ─── 引擎状态 ─────────────────────────────────────────────────────────────────

/// 引擎运行时快照（用于 /api/status 埋点）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingEngineStatus {
    /// 是否已启用
    pub enabled: bool,
    /// 已加载的 Skills 总数
    pub skills_loaded: usize,
    /// 按分类统计
    pub skills_by_category: SkillCategoryCount,
    /// Skills 根目录（实际使用）
    pub skills_dir: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillCategoryCount {
    pub risk_control: usize,
    pub factor: usize,
    pub strategy: usize,
    pub experience: usize,
}
