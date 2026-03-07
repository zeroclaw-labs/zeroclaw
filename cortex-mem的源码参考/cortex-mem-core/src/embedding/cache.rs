use crate::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

/// LRU 缓存项
#[derive(Clone)]
struct CacheItem {
    embedding: Vec<f32>,
    timestamp: Instant,
}

/// Embedding 缓存配置
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// 最大缓存条目数
    pub max_entries: usize,
    /// 缓存过期时间（秒）
    pub ttl_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 10000,
            ttl_secs: 3600,  // 1 小时
        }
    }
}

/// Embedding 缓存层
/// 
/// 为 EmbeddingClient 提供 LRU 缓存，显著减少重复查询
pub struct EmbeddingCache<T> {
    inner: Arc<T>,
    cache: Arc<RwLock<HashMap<String, CacheItem>>>,
    access_order: Arc<RwLock<Vec<String>>>,  // LRU 访问顺序
    config: CacheConfig,
}

impl<T> EmbeddingCache<T>
where
    T: EmbeddingProvider + Send + Sync,
{
    pub fn new(inner: Arc<T>, config: CacheConfig) -> Self {
        Self {
            inner,
            cache: Arc::new(RwLock::new(HashMap::new())),
            access_order: Arc::new(RwLock::new(Vec::new())),
            config,
        }
    }
    
    /// 嵌入单个文本（带缓存）
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let key = self.compute_cache_key(text);
        
        // 1. 尝试从缓存读取
        {
            let cache = self.cache.read().await;
            if let Some(item) = cache.get(&key) {
                // 检查是否过期
                if item.timestamp.elapsed() < Duration::from_secs(self.config.ttl_secs) {
                    // 更新 LRU 访问顺序
                    self.update_access_order(&key).await;
                    return Ok(item.embedding.clone());
                }
            }
        }
        
        // 2. 缓存未命中，调用底层 API
        let embedding = self.inner.embed(text).await?;
        
        // 3. 写入缓存
        self.put_cache(key, embedding.clone()).await;
        
        Ok(embedding)
    }
    
    /// 批量嵌入（带缓存）
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        let mut cache_misses = Vec::new();
        let mut miss_indices = Vec::new();
        
        // 1. 检查缓存
        {
            let cache = self.cache.read().await;
            for (idx, text) in texts.iter().enumerate() {
                let key = self.compute_cache_key(text);
                
                if let Some(item) = cache.get(&key) {
                    if item.timestamp.elapsed() < Duration::from_secs(self.config.ttl_secs) {
                        results.push(Some(item.embedding.clone()));
                        self.update_access_order(&key).await;
                        continue;
                    }
                }
                
                // 缓存未命中
                results.push(None);
                cache_misses.push(text.clone());
                miss_indices.push(idx);
            }
        }
        
        // 2. 批量查询未命中的项
        if !cache_misses.is_empty() {
            let embeddings = self.inner.embed_batch(&cache_misses).await?;
            
            // 3. 写入缓存并填充结果
            for (text, embedding) in cache_misses.iter().zip(embeddings.iter()) {
                let key = self.compute_cache_key(text);
                self.put_cache(key, embedding.clone()).await;
            }
            
            // 4. 填充缺失的结果
            for (miss_idx, embedding) in miss_indices.iter().zip(embeddings.iter()) {
                results[*miss_idx] = Some(embedding.clone());
            }
        }
        
        // 5. 转换 Option<Vec<f32>> -> Vec<f32>
        Ok(results.into_iter().map(|opt| opt.unwrap()).collect())
    }
    
    /// 计算缓存键（使用文本哈希）
    fn compute_cache_key(&self, text: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }
    
    /// 写入缓存（带 LRU 淘汰）
    async fn put_cache(&self, key: String, embedding: Vec<f32>) {
        let mut cache = self.cache.write().await;
        let mut access_order = self.access_order.write().await;
        
        // LRU 淘汰
        if cache.len() >= self.config.max_entries {
            if let Some(oldest_key) = access_order.first().cloned() {
                cache.remove(&oldest_key);
                access_order.remove(0);
            }
        }
        
        // 插入新项
        cache.insert(key.clone(), CacheItem {
            embedding,
            timestamp: Instant::now(),
        });
        
        access_order.push(key);
    }
    
    /// 更新 LRU 访问顺序
    async fn update_access_order(&self, key: &str) {
        let mut access_order = self.access_order.write().await;
        
        if let Some(pos) = access_order.iter().position(|k| k == key) {
            access_order.remove(pos);
            access_order.push(key.to_string());
        }
    }
    
    /// 获取缓存统计
    pub async fn stats(&self) -> CacheStats {
        let cache = self.cache.read().await;
        CacheStats {
            total_entries: cache.len(),
            max_entries: self.config.max_entries,
            ttl_secs: self.config.ttl_secs,
        }
    }
    
    /// 清空缓存
    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        let mut access_order = self.access_order.write().await;
        cache.clear();
        access_order.clear();
    }
}

/// 缓存统计
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: usize,
    pub max_entries: usize,
    pub ttl_secs: u64,
}

/// Embedding 提供者 Trait
/// 
/// 抽象出 embed 和 embed_batch 方法，便于缓存层包装
#[async_trait::async_trait]
pub trait EmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

// 为 EmbeddingClient 实现 EmbeddingProvider
#[async_trait::async_trait]
impl EmbeddingProvider for crate::embedding::EmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }
    
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_batch(texts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_cache_config_default() {
        let config = CacheConfig::default();
        assert_eq!(config.max_entries, 10000);
        assert_eq!(config.ttl_secs, 3600);
    }
}
