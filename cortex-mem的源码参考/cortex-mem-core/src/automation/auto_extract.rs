use crate::{
    Result,
    filesystem::CortexFilesystem,
    llm::LLMClient,
};
use std::sync::Arc;
use tracing::info;

/// 会话自动提取配置
#[derive(Debug, Clone)]
pub struct AutoExtractConfig {
    /// 触发自动提取的最小消息数
    pub min_message_count: usize,
    /// 是否在会话关闭时自动提取
    pub extract_on_close: bool,
}

impl Default for AutoExtractConfig {
    fn default() -> Self {
        Self {
            min_message_count: 5,
            extract_on_close: true,
        }
    }
}

/// 自动提取统计
#[derive(Debug, Clone, Default)]
pub struct AutoExtractStats {
    pub facts_extracted: usize,
    pub decisions_extracted: usize,
    pub entities_extracted: usize,
    pub user_memories_saved: usize,
    pub agent_memories_saved: usize,
}

/// 会话自动提取器
///
/// v2.5: 此结构体已被简化，记忆提取现在由 SessionManager 通过 MemoryEventCoordinator 处理。
/// 保留此结构体仅用于向后兼容。
pub struct AutoExtractor {
    #[allow(dead_code)]
    filesystem: Arc<CortexFilesystem>,
    #[allow(dead_code)]
    llm: Arc<dyn LLMClient>,
    #[allow(dead_code)]
    config: AutoExtractConfig,
    user_id: String,
}

impl AutoExtractor {
    /// 创建新的自动提取器
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        llm: Arc<dyn LLMClient>,
        config: AutoExtractConfig,
    ) -> Self {
        Self {
            filesystem,
            llm,
            config,
            user_id: "default".to_string(),
        }
    }

    /// 创建新的自动提取器,指定用户ID
    pub fn with_user_id(
        filesystem: Arc<CortexFilesystem>,
        llm: Arc<dyn LLMClient>,
        config: AutoExtractConfig,
        user_id: impl Into<String>,
    ) -> Self {
        Self {
            filesystem,
            llm,
            config,
            user_id: user_id.into(),
        }
    }

    /// 设置用户ID
    pub fn set_user_id(&mut self, user_id: impl Into<String>) {
        self.user_id = user_id.into();
    }

    /// 提取会话记忆
    ///
    /// v2.5: 此方法已被废弃。记忆提取现在由 SessionManager::close_session 通过
    /// MemoryEventCoordinator 异步处理。此方法返回空统计用于向后兼容。
    pub async fn extract_session(&self, _thread_id: &str) -> Result<AutoExtractStats> {
        info!(
            "AutoExtractor::extract_session is deprecated - memory extraction is handled by MemoryEventCoordinator"
        );
        Ok(AutoExtractStats::default())
    }

    /// 获取用户ID
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}
