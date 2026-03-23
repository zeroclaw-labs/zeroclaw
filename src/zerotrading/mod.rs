//! ZeroTrading — 量化交易引擎模块
//!
//! ## 架构概述
//!
//! ```text
//! workspace/
//! └── zerotrading/
//!     └── skills/
//!         ├── 风控/    ← 🛡️ 优先级最高（硬约束）
//!         ├── 因子/    ← 📊 环境感知信号
//!         ├── 策略/    ← 📈 具体交易模型
//!         └── 经验/    ← 🧠 认知与哲学
//! ```
//!
//! ## 集成方式
//!
//! ZeroTrading 以 `PromptSection` 形式接入 zeroclaw 原有的 `SystemPromptBuilder`，
//! 在 Agent 启动时自动将量化技能知识库注入系统提示词。
//!
//! 接入示例（在 agent/agent.rs 或 gateway 初始化处）：
//! ```rust,ignore
//! use zeroclaw::zerotrading::engine::TradingEngine;
//! use zeroclaw::zerotrading::prompt::TradingSectionBuilder;
//! use zeroclaw::agent::prompt::SystemPromptBuilder;
//! use std::sync::Arc;
//!
//! let engine = Arc::new(TradingEngine::with_defaults(&workspace_dir));
//! let builder = SystemPromptBuilder::with_defaults()
//!     .add_section(Box::new(TradingSectionBuilder::new(engine.clone())));
//! ```
//!
//! ## 热重载
//!
//! ```rust,ignore
//! engine.reload(); // 重新扫描 skills 目录，无需重启
//! ```
//!
//! ## Skills 编写规范
//!
//! 在对应分类目录下创建 `.md` 文件，文件自动被加载，无需改代码。
//!
//! 文件模板（见 `workspace/zerotrading/skills/策略/README.md`）。

pub mod api;
pub mod config;
pub mod engine;
pub mod prompt;
pub mod skills;
pub mod types;

// 重新导出常用类型，方便外部使用
pub use api::{trading_router, TradingApiState, TradingRouterState};

pub use config::{TradingAccountEntry, TradingAccountStore};
pub use engine::TradingEngine;
pub use prompt::TradingSectionBuilder;
pub use types::{
    SkillCategory, TradeAction, TradeDecision, TradingEngineConfig, TradingEngineStatus,
    TradingSkill,
};
