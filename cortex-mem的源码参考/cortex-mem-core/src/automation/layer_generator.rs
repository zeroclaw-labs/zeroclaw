use crate::layers::generator::{AbstractGenerator, OverviewGenerator};
use crate::llm::LLMClient;
use crate::{CortexFilesystem, FilesystemOperations, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// 层级生成配置
#[derive(Debug, Clone)]
pub struct LayerGenerationConfig {
    /// 每批生成数量
    pub batch_size: usize,
    /// 批次间延迟（毫秒）
    pub delay_ms: u64,
    /// 启动时自动生成
    pub auto_generate_on_startup: bool,
    /// Abstract 配置
    pub abstract_config: AbstractConfig,
    /// Overview 配置
    pub overview_config: OverviewConfig,
}

#[derive(Debug, Clone)]
pub struct AbstractConfig {
    /// 最大 Token 数
    pub max_tokens: usize,
    /// 最大字符数
    pub max_chars: usize,
    /// 目标句子数
    pub target_sentences: usize,
}

#[derive(Debug, Clone)]
pub struct OverviewConfig {
    /// 最大 Token 数
    pub max_tokens: usize,
    /// 最大字符数
    pub max_chars: usize,
}

impl Default for LayerGenerationConfig {
    fn default() -> Self {
        Self {
            batch_size: 10,
            delay_ms: 2000,
            auto_generate_on_startup: false,
            abstract_config: AbstractConfig {
                max_tokens: 400,
                max_chars: 2000,
                target_sentences: 2,
            },
            overview_config: OverviewConfig {
                max_tokens: 1500,
                max_chars: 6000,
            },
        }
    }
}

/// 层级生成统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GenerationStats {
    pub total: usize,
    pub generated: usize,
    pub failed: usize,
}

/// 层级生成器
///
/// 负责扫描文件系统，检测缺失的 L0/L1 文件，并渐进式生成
pub struct LayerGenerator {
    filesystem: Arc<CortexFilesystem>,
    abstract_gen: AbstractGenerator,
    overview_gen: OverviewGenerator,
    llm_client: Arc<dyn LLMClient>,
    config: LayerGenerationConfig,
}

impl LayerGenerator {
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        llm_client: Arc<dyn LLMClient>,
        config: LayerGenerationConfig,
    ) -> Self {
        Self {
            filesystem,
            abstract_gen: AbstractGenerator::new(),
            overview_gen: OverviewGenerator::new(),
            llm_client,
            config,
        }
    }

    /// 扫描所有目录
    pub async fn scan_all_directories(&self) -> Result<Vec<String>> {
        let mut directories = Vec::new();

        // 扫描四个核心维度
        for scope in &["session", "user", "agent", "resources"] {
            let scope_uri = format!("cortex://{}", scope);

            // 检查维度是否存在
            match self.filesystem.exists(&scope_uri).await {
                Ok(true) => {
                    log::info!("📂 扫描维度: {} ({})", scope, scope_uri);
                    match self.scan_scope(&scope_uri).await {
                        Ok(dirs) => {
                            log::info!("📂 维度 {} 发现 {} 个目录", scope, dirs.len());
                            directories.extend(dirs);
                        }
                        Err(e) => {
                            log::warn!("⚠️ 扫描维度 {} 失败: {}", scope, e);
                            warn!("Failed to scan scope {}: {}", scope, e);
                        }
                    }
                }
                Ok(false) => {
                    log::info!("📂 维度 {} 不存在，跳过", scope);
                }
                Err(e) => {
                    log::warn!("⚠️ 检查维度 {} 存在性失败: {}", scope, e);
                }
            }
        }

        Ok(directories)
    }

    /// 扫描单个维度
    async fn scan_scope(&self, scope_uri: &str) -> Result<Vec<String>> {
        let mut directories = Vec::new();
        
        // 先检查维度是否存在
        match self.filesystem.exists(scope_uri).await {
            Ok(true) => {
                log::info!("📂 维度目录存在: {}", scope_uri);
            }
            Ok(false) => {
                log::info!("📂 维度目录不存在: {}", scope_uri);
                return Ok(directories);
            }
            Err(e) => {
                log::warn!("⚠️ 检查维度存在性失败: {} - {}", scope_uri, e);
                return Ok(directories);
            }
        }
        
        // 尝试列出目录内容
        match self.filesystem.list(scope_uri).await {
            Ok(entries) => {
                log::info!("📂 维度 {} 下有 {} 个条目", scope_uri, entries.len());
                for entry in &entries {
                    log::info!("📂   - {} (is_dir: {})", entry.name, entry.is_directory);
                }
            }
            Err(e) => {
                log::warn!("⚠️ 列出维度目录失败: {} - {}", scope_uri, e);
                return Ok(directories);
            }
        }
        
        self.scan_recursive(scope_uri, &mut directories).await?;
        Ok(directories)
    }

    /// 递归扫描目录
    fn scan_recursive<'a>(
        &'a self,
        uri: &'a str,
        directories: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // 列出当前目录
            let entries = match self.filesystem.list(uri).await {
                Ok(entries) => entries,
                Err(e) => {
                    debug!("Failed to list {}: {}", uri, e);
                    return Ok(());
                }
            };

            for entry in entries {
                // 跳过隐藏文件
                if entry.name.starts_with('.') {
                    continue;
                }

                if entry.is_directory {
                    // 添加目录到列表
                    directories.push(entry.uri.clone());

                    // 递归扫描子目录
                    self.scan_recursive(&entry.uri, directories).await?;
                }
            }

            Ok(())
        })
    }

    /// 检测目录是否有 L0/L1 文件
    pub async fn has_layers(&self, uri: &str) -> Result<bool> {
        let abstract_path = format!("{}/.abstract.md", uri);
        let overview_path = format!("{}/.overview.md", uri);

        let has_abstract = self.filesystem.exists(&abstract_path).await?;
        let has_overview = self.filesystem.exists(&overview_path).await?;

        Ok(has_abstract && has_overview)
    }

    /// 过滤出缺失 L0/L1 的目录
    pub async fn filter_missing_layers(&self, dirs: &[String]) -> Result<Vec<String>> {
        let mut missing = Vec::new();

        for dir in dirs {
            match self.has_layers(dir).await {
                Ok(has) => {
                    if !has {
                        missing.push(dir.clone());
                    }
                }
                Err(e) => {
                    debug!("Failed to check layers for {}: {}", dir, e);
                }
            }
        }

        Ok(missing)
    }

    /// 确保所有目录拥有 L0/L1
    pub async fn ensure_all_layers(&self) -> Result<GenerationStats> {
        log::info!("🔍 开始扫描目录...");
        info!("开始扫描目录...");
        let directories = self.scan_all_directories().await?;
        log::info!("📋 发现 {} 个目录", directories.len());
        info!("发现 {} 个目录", directories.len());
        
        // 🔧 Debug: 打印扫描到的目录
        for dir in &directories {
            log::debug!("扫描到目录: {}", dir);
            debug!("扫描到目录: {}", dir);
        }

        log::info!("🔎 检测缺失的 L0/L1...");
        info!("检测缺失的 L0/L1...");
        let missing = self.filter_missing_layers(&directories).await?;
        log::info!("📋 发现 {} 个目录缺失 L0/L1", missing.len());
        info!("发现 {} 个目录缺失 L0/L1", missing.len());
        
        // 🔧 Debug: 打印缺失层级文件的目录
        for dir in &missing {
            log::info!("📝 需要生成层级文件: {}", dir);
            info!("需要生成层级文件: {}", dir);
        }

        if missing.is_empty() {
            return Ok(GenerationStats {
                total: 0,
                generated: 0,
                failed: 0,
            });
        }

        let mut stats = GenerationStats {
            total: missing.len(),
            generated: 0,
            failed: 0,
        };

        // 分批生成
        let total_batches = (missing.len() + self.config.batch_size - 1) / self.config.batch_size;

        for (batch_idx, batch) in missing.chunks(self.config.batch_size).enumerate() {
            log::info!("📦 处理批次 {}/{}", batch_idx + 1, total_batches);
            info!("处理批次 {}/{}", batch_idx + 1, total_batches);

            for dir in batch {
                match self.generate_layers_for_directory(dir).await {
                    Ok(_) => {
                        stats.generated += 1;
                        log::info!("✅ 生成成功: {}", dir);
                        info!("✓ 生成成功: {}", dir);
                    }
                    Err(e) => {
                        stats.failed += 1;
                        log::warn!("⚠️ 生成失败: {} - {}", dir, e);
                        warn!("✗ 生成失败: {} - {}", dir, e);
                    }
                }
            }

            // 批次间延迟
            if batch_idx < total_batches - 1 {
                tokio::time::sleep(tokio::time::Duration::from_millis(self.config.delay_ms)).await;
            }
        }

        log::info!("✅ 生成完成: 成功 {}, 失败 {}", stats.generated, stats.failed);
        info!("生成完成: 成功 {}, 失败 {}", stats.generated, stats.failed);
        Ok(stats)
    }

    /// 确保特定timeline目录拥有L0/L1层级文件
    /// 用于会话关闭时触发生成，避免频繁更新
    pub async fn ensure_timeline_layers(&self, timeline_uri: &str) -> Result<GenerationStats> {
        info!("开始为timeline生成层级文件: {}", timeline_uri);

        // 扫描timeline下的所有目录
        let mut directories = Vec::new();
        self.scan_recursive(timeline_uri, &mut directories).await?;

        info!("发现 {} 个timeline目录", directories.len());

        // 检测缺失的 L0/L1
        let missing = self.filter_missing_layers(&directories).await?;
        info!("发现 {} 个目录缺失 L0/L1", missing.len());

        if missing.is_empty() {
            return Ok(GenerationStats {
                total: 0,
                generated: 0,
                failed: 0,
            });
        }

        let mut stats = GenerationStats {
            total: missing.len(),
            generated: 0,
            failed: 0,
        };

        // 生成层级文件（不需要分批，因为timeline通常不大）
        for dir in missing {
            match self.generate_layers_for_directory(&dir).await {
                Ok(_) => {
                    stats.generated += 1;
                    info!("✓ 生成成功: {}", dir);
                }
                Err(e) => {
                    stats.failed += 1;
                    warn!("✗ 生成失败: {} - {}", dir, e);
                }
            }
        }

        info!(
            "Timeline层级生成完成: 成功 {}, 失败 {}",
            stats.generated, stats.failed
        );
        Ok(stats)
    }

    /// 为单个目录生成 L0/L1
    async fn generate_layers_for_directory(&self, uri: &str) -> Result<()> {
        debug!("生成层级文件: {}", uri);

        // 1. 检查是否需要重新生成（避免重复生成未变更的内容）
        if !self.should_regenerate(uri).await? {
            debug!("目录内容未变更，跳过生成: {}", uri);
            return Ok(());
        }

        // 2. 读取目录内容（聚合所有子文件）
        let content = self.aggregate_directory_content(uri).await?;

        if content.is_empty() {
            debug!("目录为空，跳过: {}", uri);
            return Ok(());
        }

        // 3. 使用现有的 AbstractGenerator 生成 L0 抽象
        let abstract_text = self
            .abstract_gen
            .generate_with_llm(&content, &self.llm_client)
            .await?;

        // 4. 使用现有的 OverviewGenerator 生成 L1 概览
        let overview = self
            .overview_gen
            .generate_with_llm(&content, &self.llm_client)
            .await?;

        // 5. 强制执行长度限制
        let abstract_text = self.enforce_abstract_limit(abstract_text)?;
        let overview = self.enforce_overview_limit(overview)?;

        // 6. 添加 "Added" 日期标记（与 extraction.rs 保持一致）
        let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let abstract_with_date = format!("{}\n\n**Added**: {}", abstract_text, timestamp);
        let overview_with_date = format!("{}\n\n---\n\n**Added**: {}", overview, timestamp);

        // 7. 写入文件
        let abstract_path = format!("{}/.abstract.md", uri);
        let overview_path = format!("{}/.overview.md", uri);

        self.filesystem
            .write(&abstract_path, &abstract_with_date)
            .await?;
        self.filesystem
            .write(&overview_path, &overview_with_date)
            .await?;

        debug!("层级文件生成完成: {}", uri);
        Ok(())
    }

    /// 检查是否需要重新生成层级文件
    ///
    /// 检查逻辑：
    /// 1. 如果 .abstract.md 或 .overview.md 不存在 → 需要生成
    /// 2. 如果目录中有文件比 .abstract.md 更新 → 需要重新生成
    /// 3. 否则 → 跳过（避免重复生成）
    async fn should_regenerate(&self, uri: &str) -> Result<bool> {
        let abstract_path = format!("{}/.abstract.md", uri);
        let overview_path = format!("{}/.overview.md", uri);

        // 检查层级文件是否存在
        let abstract_exists = self.filesystem.exists(&abstract_path).await?;
        let overview_exists = self.filesystem.exists(&overview_path).await?;

        if !abstract_exists || !overview_exists {
            debug!("层级文件缺失，需要生成: {}", uri);
            return Ok(true);
        }

        // 读取 .abstract.md 中的时间戳
        let abstract_content = match self.filesystem.read(&abstract_path).await {
            Ok(content) => content,
            Err(_) => {
                debug!("无法读取 .abstract.md，需要重新生成: {}", uri);
                return Ok(true);
            }
        };

        // 提取 "Added" 时间戳
        let abstract_timestamp = self.extract_added_timestamp(&abstract_content);

        if abstract_timestamp.is_none() {
            debug!(".abstract.md 缺少时间戳，需要重新生成: {}", uri);
            return Ok(true);
        }

        let abstract_time = abstract_timestamp.unwrap();

        // 检查目录中的文件是否有更新
        let entries = self.filesystem.list(uri).await?;
        for entry in entries {
            // 跳过隐藏文件和目录
            if entry.name.starts_with('.') || entry.is_directory {
                continue;
            }

            // 只检查 .md 和 .txt 文件
            if entry.name.ends_with(".md") || entry.name.ends_with(".txt") {
                // 读取文件内容，提取其中的时间戳（如果有）
                if let Ok(file_content) = self.filesystem.read(&entry.uri).await {
                    if let Some(file_time) = self.extract_added_timestamp(&file_content) {
                        // 如果文件时间戳晚于 abstract 时间戳，需要重新生成
                        if file_time > abstract_time {
                            debug!("文件 {} 有更新，需要重新生成: {}", entry.name, uri);
                            return Ok(true);
                        }
                    }
                }
            }
        }

        debug!("目录内容未变更，无需重新生成: {}", uri);
        Ok(false)
    }

    /// 从内容中提取 "Added" 时间戳
    fn extract_added_timestamp(&self, content: &str) -> Option<DateTime<Utc>> {
        // 查找 "**Added**: YYYY-MM-DD HH:MM:SS UTC" 格式
        if let Some(start) = content.find("**Added**: ") {
            let timestamp_str = &content[start + 11..];
            if let Some(end) = timestamp_str.find('\n') {
                let timestamp_str = &timestamp_str[..end].trim();
                // 解析时间戳
                if let Ok(dt) = DateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S UTC") {
                    return Some(dt.with_timezone(&Utc));
                }
            }
        }
        None
    }

    /// 聚合目录内容
    async fn aggregate_directory_content(&self, uri: &str) -> Result<String> {
        let entries = self.filesystem.list(uri).await?;
        let mut content = String::new();

        for entry in entries {
            // 跳过隐藏文件和目录
            if entry.name.starts_with('.') || entry.is_directory {
                continue;
            }

            // 只读取文本文件
            if entry.name.ends_with(".md") || entry.name.ends_with(".txt") {
                match self.filesystem.read(&entry.uri).await {
                    Ok(file_content) => {
                        content.push_str(&format!("\n\n=== {} ===\n\n", entry.name));
                        content.push_str(&file_content);
                    }
                    Err(e) => {
                        debug!("Failed to read {}: {}", entry.uri, e);
                    }
                }
            }
        }

        // 截断到合理长度（避免超出 LLM 上下文限制）
        let max_chars = 10000;
        if content.chars().count() > max_chars {
            let truncated: String = content.chars().take(max_chars).collect();
            let mut content = truncated;
            content.push_str("\n\n[内容已截断...]");
            return Ok(content);
        }

        Ok(content)
    }

    /// 强制执行 Abstract 长度限制
    fn enforce_abstract_limit(&self, text: String) -> Result<String> {
        let mut result = text.trim().to_string();
        let max_chars = self.config.abstract_config.max_chars;

        if result.chars().count() <= max_chars {
            return Ok(result);
        }

        // 找到 max_chars 字符对应的字节位置
        let byte_limit = result
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(result.len());

        // 截断到最后一个句号/问号/叹号
        if let Some(pos) = result[..byte_limit]
            .rfind(|c| c == '。' || c == '.' || c == '?' || c == '!' || c == '！' || c == '？')
        {
            result.truncate(pos + 1);
        } else {
            // 找到 max_chars - 3 字符对应的字节位置
            let truncate_pos = result
                .char_indices()
                .nth(max_chars.saturating_sub(3))
                .map(|(i, _)| i)
                .unwrap_or(result.len());
            result.truncate(truncate_pos);
            result.push_str("...");
        }

        Ok(result)
    }

    /// 强制执行 Overview 长度限制
    fn enforce_overview_limit(&self, text: String) -> Result<String> {
        let mut result = text.trim().to_string();
        let max_chars = self.config.overview_config.max_chars;

        if result.chars().count() <= max_chars {
            return Ok(result);
        }

        // 找到 max_chars 字符对应的字节位置
        let byte_limit = result
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(result.len());

        // 截断到最后一个段落
        if let Some(pos) = result[..byte_limit].rfind("\n\n") {
            result.truncate(pos);
            result.push_str("\n\n[内容已截断...]");
        } else {
            // 找到 max_chars - 3 字符对应的字节位置
            let truncate_pos = result
                .char_indices()
                .nth(max_chars.saturating_sub(3))
                .map(|(i, _)| i)
                .unwrap_or(result.len());
            result.truncate(truncate_pos);
            result.push_str("...");
        }

        Ok(result)
    }

    /// 重新生成所有超大的 .abstract 文件
    pub async fn regenerate_oversized_abstracts(&self) -> Result<RegenerationStats> {
        info!("扫描超大的 .abstract 文件...");
        let directories = self.scan_all_directories().await?;
        let max_chars = self.config.abstract_config.max_chars;

        let mut stats = RegenerationStats {
            total: 0,
            regenerated: 0,
            failed: 0,
        };

        for dir in directories {
            let abstract_path = format!("{}/.abstract.md", dir);

            if let Ok(content) = self.filesystem.read(&abstract_path).await {
                // 移除 "Added" 标记后再检查长度
                let content_without_metadata = self.strip_metadata(&content);

                if content_without_metadata.len() > max_chars {
                    stats.total += 1;
                    info!(
                        "发现超大 .abstract: {} ({} 字符)",
                        dir,
                        content_without_metadata.len()
                    );

                    match self.generate_layers_for_directory(&dir).await {
                        Ok(_) => {
                            stats.regenerated += 1;
                            info!("✓ 重新生成成功: {}", dir);
                        }
                        Err(e) => {
                            stats.failed += 1;
                            warn!("✗ 重新生成失败: {} - {}", dir, e);
                        }
                    }
                }
            }
        }

        info!(
            "重新生成完成: 总计 {}, 成功 {}, 失败 {}",
            stats.total, stats.regenerated, stats.failed
        );

        Ok(stats)
    }

    /// 移除元数据（Added、Confidence等）
    fn strip_metadata(&self, content: &str) -> String {
        let mut result = content.to_string();

        // 移除 **Added**: ... 行
        if let Some(pos) = result.find("\n\n**Added**:") {
            result.truncate(pos);
        } else if let Some(pos) = result.find("**Added**:") {
            result.truncate(pos);
        }

        result.trim().to_string()
    }
}

/// 重新生成统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegenerationStats {
    pub total: usize,
    pub regenerated: usize,
    pub failed: usize,
}
