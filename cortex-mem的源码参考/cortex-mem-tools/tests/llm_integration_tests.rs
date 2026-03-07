//! Cortex-Mem LLM 集成测试
//!
//! 这些测试需要有效的 LLM 配置才能运行
//! 配置来源：config.toml 或环境变量
//!
//! 注意：这些测试需要外部服务（Qdrant, LLM, Embedding），默认被忽略。
//! 要运行这些测试，请使用：cargo test -- --ignored

#![allow(dead_code)]

// 下面的测试需要外部服务，暂时禁用
// 新的 API 需要以下依赖：
// - Qdrant 向量数据库
// - LLM 服务（OpenAI 兼容 API）
// - Embedding 服务

/*
use cortex_mem_tools::{MemoryOperations, types::*};
use std::sync::Arc;
use tempfile::TempDir;

/// 检查是否有 LLM 配置
fn has_llm_config() -> bool {
    // 先尝试从 config.toml 加载（从多个可能的位置查找）
    if load_llm_config_from_file().is_some() {
        return true;
    }
    
    // 或者检查环境变量
    std::env::var("LLM_API_BASE_URL").is_ok() && 
    std::env::var("LLM_API_KEY").is_ok()
}

/// 从 config.toml 解析 LLM 配置
fn load_llm_config_from_file() -> Option<cortex_mem_core::llm::LLMConfig> {
    // 尝试从多个位置查找 config.toml
    let possible_paths = [
        "config.toml",  // 当前目录
        "../config.toml",  // 上级目录（从 cortex-mem-tools 运行时）
        "../../config.toml",  // 上两级目录
    ];
    
    let mut content = None;
    let mut found_path = "";
    
    for path in &possible_paths {
        if let Ok(c) = std::fs::read_to_string(path) {
            content = Some(c);
            found_path = path;
            break;
        }
    }
    
    let content = content?;
    
    // 检查是否有 [llm] 段落
    if !content.contains("[llm]") {
        println!("⚠️ config.toml 中没有 [llm] 配置段落");
        return None;
    }
    
    // 简单解析 TOML
    let mut api_base_url = None;
    let mut api_key = None;
    let mut model = Some("gpt-3.5-turbo".to_string());
    let mut temperature = Some(0.1f32);
    let mut max_tokens = Some(4096u32);
    
    let mut in_llm_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        
        // 跳过空行
        if trimmed.is_empty() {
            continue;
        }
        
        // 检测 [llm] 段落开始
        if trimmed == "[llm]" {
            in_llm_section = true;
            continue;
        }
        
        // 检测其他段落开始（结束 [llm] 段落）
        if trimmed.starts_with('[') && in_llm_section {
            break;
        }
        
        // 在 [llm] 段落内
        if in_llm_section {
            // 跳过注释行（以 # 开头）
            if trimmed.starts_with('#') {
                continue;
            }
            
            // 解析 key = "value" 格式
            if let Some(eq_pos) = trimmed.find('=') {
                let key = trimmed[..eq_pos].trim();
                let value_part = trimmed[eq_pos + 1..].trim();
                
                // 跳过注释掉的配置（key 以 # 开头）
                if key.starts_with('#') {
                    continue;
                }
                
                // 移除引号
                let value = value_part
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                
                match key {
                    "api_base_url" => api_base_url = Some(value),
                    "api_key" => api_key = Some(value),
                    "model_efficient" | "model" => model = Some(value),
                    "temperature" => temperature = value.parse().ok(),
                    "max_tokens" => max_tokens = value.parse().ok(),
                    _ => {}
                }
            }
        }
    }
    
    // 检查是否获取了必需的配置
    let api_url = api_base_url?;
    let key = api_key?;
    
    // 检查值是否为空
    if api_url.is_empty() || key.is_empty() {
        println!("⚠️ config.toml 中的 api_base_url 或 api_key 为空");
        return None;
    }
    
    Some(cortex_mem_core::llm::LLMConfig {
        api_base_url: api_url,
        api_key: key,
        model_efficient: model?,
        temperature: temperature?,
        max_tokens: max_tokens? as usize,
    })
}

/// 加载 LLM 配置
fn load_llm_config() -> Option<cortex_mem_core::llm::LLMConfig> {
    // 优先从 config.toml 加载
    if let Some(config) = load_llm_config_from_file() {
        return Some(config);
    }
    
    // 从环境变量加载
    if let (Ok(api_url), Ok(api_key)) = (
        std::env::var("LLM_API_BASE_URL"),
        std::env::var("LLM_API_KEY"),
    ) {
        return Some(cortex_mem_core::llm::LLMConfig {
            api_base_url: api_url,
            api_key,
            model_efficient: std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-3.5-turbo".to_string()),
            temperature: 0.1,
            max_tokens: 4096,
        });
    }
    
    None
}

/// 创建带 LLM 的测试环境
async fn setup_llm_test_env() -> Option<(TempDir, MemoryOperations)> {
    if !has_llm_config() {
        return None;
    }
    
    let llm_config = load_llm_config()?;
    let llm_client = Arc::new(
        cortex_mem_core::llm::LLMClientImpl::new(llm_config).ok()?
    );
    
    let temp_dir = TempDir::new().unwrap();
    let ops = MemoryOperations::with_tenant_and_llm(
        temp_dir.path().to_str().unwrap(),
        "llm_test_tenant",
        llm_client,
    ).await.ok()?;
    
    Some((temp_dir, ops))
}

// ==================== LLM 功能测试 ====================

mod llm_layer_tests {
    use super::*;

    /// 测试 LLM 生成的高质量 L0 摘要
    #[tokio::test]
    async fn test_llm_l0_quality() {
        let env = setup_llm_test_env().await;
        if env.is_none() {
            println!("⚠️ 跳过测试：没有 LLM 配置");
            return;
        }
        
        let (_temp_dir, ops) = env.unwrap();
        
        // 使用需要理解的内容
        let content = r#"# Rust 所有权系统

Rust 的所有权系统是其最独特的特性之一。

## 核心规则

1. 每个值都有一个所有者
2. 同一时间只能有一个所有者  
3. 当所有者离开作用域，值被丢弃

## 为什么重要

所有权让 Rust 能够在没有垃圾回收器的情况下保证内存安全，同时保持高性能。

## 实际应用

在系统编程、嵌入式开发、Web 后端等场景都有广泛应用。"#;

        let args = StoreArgs {
            content: content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true), // 启用 LLM 生成
            scope: "user".to_string(),
            user_id: Some("llm_l0_test".to_string()),
            agent_id: None,
        };
        
        let start = std::time::Instant::now();
        let result = ops.store(args).await.unwrap();
        let duration = start.elapsed();
        
        println!("✅ LLM L0 生成耗时: {:?}", duration);
        println!("📄 存储 URI: {}", result.uri);
        
        // 获取 L0 摘要
        let l0 = ops.get_abstract(&result.uri).await.unwrap();
        println!("📝 L0 摘要 ({} tokens): {}", l0.token_count, l0.abstract_text);
        
        // 验证 L0 质量（使用字符数而不是 token 数，因为中文 token 计算不准确）
        let char_count = l0.abstract_text.chars().count();
        println!("📝 L0 字符数: {}", char_count);
        assert!(char_count > 20, "LLM 生成的 L0 应该有实质内容 ({} 字符)", char_count);
        assert!(char_count < 2000, "L0 应该相对简洁 ({} 字符)", char_count);
        
        // 验证包含关键信息（LLM 应该提取出关键概念）
        let has_keywords = l0.abstract_text.contains("所有权") || 
                          l0.abstract_text.contains("Rust") ||
                          l0.abstract_text.contains("内存安全") ||
                          l0.abstract_text.contains("owner") ||
                          l0.abstract_text.contains("memory");
        assert!(has_keywords, "L0 应该包含关键主题词: {}", l0.abstract_text);
    }

    /// 测试 LLM 生成的 L1 概览
    #[tokio::test]
    async fn test_llm_l1_quality() {
        let env = setup_llm_test_env().await;
        if env.is_none() {
            println!("⚠️ 跳过测试：没有 LLM 配置");
            return;
        }
        
        let (_temp_dir, ops) = env.unwrap();
        
        let content = r#"# OAuth 2.0 认证框架

OAuth 2.0 是一种授权框架，允许第三方应用获取对用户资源的有限访问权限。

## 授权模式

### 1. 授权码模式
最安全、最常用的模式，适用于有后端的应用。

### 2. 简化模式
适用于纯前端应用。

### 3. 密码凭证模式
用户直接向客户端提供用户名密码。

### 4. 客户端凭证模式
用于服务器之间的通信。

## 安全考虑

- 使用 HTTPS
- 验证 redirect_uri
- 设置合理的令牌过期时间"#;

        let args = StoreArgs {
            content: content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("llm_l1_test".to_string()),
            agent_id: None,
        };
        
        let start = std::time::Instant::now();
        let result = ops.store(args).await.unwrap();
        let duration = start.elapsed();
        
        println!("✅ LLM L1 生成耗时: {:?}", duration);
        
        // 获取 L1 概览
        let l1 = ops.get_overview(&result.uri).await.unwrap();
        println!("📝 L1 概览 ({} tokens):", l1.token_count);
        println!("{}", l1.overview_text);
        
        // 验证 L1 结构
        assert!(l1.token_count > 50, "L1 应该有详细内容");
        assert!(l1.overview_text.contains("#"), "L1 应该包含 Markdown 标题");
        
        // 验证 L1 有实质内容（LLM 生成的可能比原文长，因为会扩展解释）
        assert!(
            l1.token_count > 100,
            "L1 ({} tokens) 应该有详细内容",
            l1.token_count
        );
    }

    /// 对比 Fallback 和 LLM 生成的质量差异
    #[tokio::test]
    async fn test_llm_vs_fallback_quality() {
        let env = setup_llm_test_env().await;
        if env.is_none() {
            println!("⚠️ 跳过测试：没有 LLM 配置");
            return;
        }
        
        let (temp_dir, ops_with_llm) = env.unwrap();
        
        // 创建不带 LLM 的版本
        let ops_fallback = MemoryOperations::from_data_dir(
            temp_dir.path().to_str().unwrap()
        ).await.unwrap();
        
        let content = "Rust 是一种系统编程语言，专注于安全、并发和性能。它通过所有权系统在没有垃圾回收器的情况下保证内存安全。";
        
        // LLM 版本
        let llm_args = StoreArgs {
            content: content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("llm_compare".to_string()),
            agent_id: None,
        };
        
        let llm_result = ops_with_llm.store(llm_args).await.unwrap();
        let llm_l0 = ops_with_llm.get_abstract(&llm_result.uri).await.unwrap();
        
        // Fallback 版本
        let fallback_args = StoreArgs {
            content: content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("fallback_compare".to_string()),
            agent_id: None,
        };
        
        let fallback_result = ops_fallback.store(fallback_args).await.unwrap();
        let fallback_l0 = ops_fallback.get_abstract(&fallback_result.uri).await.unwrap();
        
        println!("🤖 LLM L0 ({} tokens): {}", llm_l0.token_count, llm_l0.abstract_text);
        println!("📋 Fallback L0 ({} tokens): {}", fallback_l0.token_count, fallback_l0.abstract_text);
        
        // LLM 版本通常更智能（不一定是更短，但应该更有信息量）
        println!("\n📊 对比: LLM {} tokens vs Fallback {} tokens", 
            llm_l0.token_count, fallback_l0.token_count);
    }
}

mod llm_memory_extraction_tests {
    use super::*;

    /// 测试 LLM 记忆提取（如果 AutoExtractor 可用）
    #[tokio::test]
    async fn test_llm_memory_extraction() {
        let env = setup_llm_test_env().await;
        if env.is_none() {
            println!("⚠️ 跳过测试：没有 LLM 配置");
            return;
        }
        
        let (_temp_dir, ops) = env.unwrap();
        
        // 创建一个模拟对话
        let thread_id = "extraction_test";
        ops.add_message(thread_id, "user", "我喜欢用 Rust 编程，因为它内存安全且性能高。").await.unwrap();
        ops.add_message(thread_id, "assistant", "是的，Rust 的所有权系统确实很独特。你还喜欢其他什么编程语言？").await.unwrap();
        ops.add_message(thread_id, "user", "我也喜欢 Python，适合快速原型开发。").await.unwrap();
        
        // 关闭 session 触发提取（如果 AutoExtractor 配置好）
        ops.close_session(thread_id).await.ok();
        
        println!("✅ 对话已存储，LLM 提取应在后台完成");
        
        // 验证 session 存在
        let session = ops.get_session(thread_id).await;
        assert!(session.is_ok(), "Session 应该存在");
    }
}

mod llm_performance_tests {
    use super::*;

    /// 测试 LLM API 调用性能
    #[tokio::test]
    async fn test_llm_api_performance() {
        let env = setup_llm_test_env().await;
        if env.is_none() {
            println!("⚠️ 跳过测试：没有 LLM 配置");
            return;
        }
        
        let (_temp_dir, ops) = env.unwrap();
        
        let content = "这是一段测试内容，用于测量 LLM API 调用的时间。";
        
        let start = std::time::Instant::now();
        
        let args = StoreArgs {
            content: content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("perf_test".to_string()),
            agent_id: None,
        };
        
        let result = ops.store(args).await.unwrap();
        let duration = start.elapsed();
        
        println!("⏱️ LLM 生成 L0/L1 总耗时: {:?}", duration);
        println!("📄 URI: {}", result.uri);
        
        // 通常 LLM 调用需要 1-5 秒
        assert!(duration.as_secs() < 30, "LLM 生成应在 30 秒内完成");
    }

    /// 批量 LLM 生成测试
    #[tokio::test]
    async fn test_batch_llm_generation() {
        let env = setup_llm_test_env().await;
        if env.is_none() {
            println!("⚠️ 跳过测试：没有 LLM 配置");
            return;
        }
        
        let (_temp_dir, ops) = env.unwrap();
        
        let contents = vec![
            "Rust 所有权系统介绍...",
            "OAuth 2.0 认证流程说明...",
            "PostgreSQL 数据库优化技巧...",
        ];
        
        let start = std::time::Instant::now();
        
        for (i, content) in contents.iter().enumerate() {
            let args = StoreArgs {
                content: content.to_string(),
                thread_id: "".to_string(),
                metadata: None,
                auto_generate_layers: Some(true),
                scope: "user".to_string(),
                user_id: Some(format!("batch_user_{}", i)),
                agent_id: None,
            };
            
            let result = ops.store(args).await.unwrap();
            println!("✅ 第 {} 个完成: {}", i + 1, result.uri);
        }
        
        let duration = start.elapsed();
        println!("⏱️ 批量 {} 个 LLM 生成总耗时: {:?}", contents.len(), duration);
        
        // 批量生成可能需要更长时间（取决于 API 响应速度）
        assert!(duration.as_secs() < 180, "批量 LLM 生成应在 3 分钟内完成");
    }
}

// ==================== 使用说明 ====================
//
// 运行这些测试需要配置 LLM API：
//
// 方式 1: 使用 config.toml（推荐）
// 确保项目根目录有 config.toml 且包含：
// [llm]
// api_base_url = "https://your-api-endpoint.com/v1"
// api_key = "your-api-key"
// model_efficient = "gpt-3.5-turbo"
//
// 方式 2: 使用环境变量
// export LLM_API_BASE_URL="https://your-api-endpoint.com/v1"
// export LLM_API_KEY="your-api-key"
// export LLM_MODEL="gpt-3.5-turbo"
//
// 然后运行测试：
// cargo test -p cortex-mem-tools --test llm_integration_tests -- --ignored
//
// 如果没有配置，测试会自动跳过并显示警告
*/
